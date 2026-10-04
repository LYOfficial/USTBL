"""MCDR plugin: relay server chat and USTBL launcher messages."""

import json
import re
import socket
import threading
import time
import urllib.error
import urllib.request

from mcdreforged.api.all import *


PLUGIN_METADATA = {
    'id': 'ustbl_message_sync',
    'version': '1.2.0',
    'name': 'USTBL Message Sync',
    'description': '通过 vUSTB 同步服务器与 USTBL 纯文本消息',
    'author': 'USTBL Team',
}

CONFIG_FILE = 'config/USTBLMessageSync/USTBLMessageSync.json'
CONFIG_IN_DATA_FOLDER = False
DEFAULT_CONFIG = {
    'api_base': 'https://www.ustb.world',
    'server_id': 0,
    'token': '',
    'poll_interval': 3,
    'heartbeat_interval': 30,
    'request_timeout': 20,
    'debug_logging': False,
}
PLUGIN_STATE = {'running': False, 'thread': None, 'config': None, 'last_id': 0}


def _is_enabled(value):
    if isinstance(value, str):
        return value.strip().lower() in {'1', 'true', 'yes', 'on', '是', '开启'}
    return bool(value)


def _debug(server, config, message):
    if _is_enabled(config.get('debug_logging')):
        server.logger.info(f'[debug] {message}')


def _load_config(server):
    config = server.load_config_simple(
        CONFIG_FILE,
        default_config=DEFAULT_CONFIG,
        in_data_folder=CONFIG_IN_DATA_FOLDER,
    )
    try:
        server.save_config_simple(
            config,
            file_name=CONFIG_FILE,
            in_data_folder=CONFIG_IN_DATA_FOLDER,
        )
    except Exception as exc:
        server.logger.warning(f'USTBL 配置文件自动保存失败: {exc}')
    return config


def _request(config, method, path, payload=None):
    data = None if payload is None else json.dumps(payload).encode('utf-8')
    base = str(config['api_base']).rstrip('/')
    if base.endswith('/api'):
        base = base[:-4]
    token = str(config.get('token') or '').strip()
    request = urllib.request.Request(
        base + path,
        data=data,
        method=method,
        headers={
            'Content-Type': 'application/json',
            'X-MCDR-Token': token,
            'X-USTBL-MCDR-Token': token,
            'Authorization': f'Bearer {token}',
            'User-Agent': f"USTBL-MCDR/{PLUGIN_METADATA['version']}",
            'Accept': 'application/json',
            'Connection': 'close',
        },
    )
    try:
        with urllib.request.urlopen(request, timeout=max(5, int(config.get('request_timeout', 20)))) as response:
            raw = response.read().decode('utf-8')
            return json.loads(raw) if raw else None
    except urllib.error.HTTPError as exc:
        body = exc.read().decode('utf-8', errors='replace')[:300]
        if exc.code == 401:
            raise RuntimeError(f'{method} {path} 返回 HTTP 401：Token 无效、已复制不完整，或 server_id 与 vUSTB 不匹配；响应: {body}') from exc
        raise RuntimeError(f'{method} {path} 返回 HTTP {exc.code}: {body}') from exc


def _config_ready(config):
    api_base = str(config.get('api_base') or '').strip()
    server_id = config.get('server_id')
    token = str(config.get('token') or '').strip()
    if not api_base or not api_base.startswith(('http://', 'https://')):
        return False, 'api_base 必须是 http:// 或 https:// 地址'
    try:
        server_id = int(server_id)
    except (TypeError, ValueError):
        return False, 'server_id 必须是正整数'
    if server_id <= 0:
        return False, '请先在配置中填写 vUSTB 消息管理的 server_id'
    if not token:
        return False, '请先在配置中填写 vUSTB 消息管理生成的 token'
    return True, ''


def _server_status(server):
    status = {
        'plugin': 'ustbl_message_sync',
        'plugin_version': PLUGIN_METADATA['version'],
        'online': True,
        'reported_at': int(time.time()),
    }
    is_running = getattr(server, 'is_server_running', None)
    if callable(is_running):
        try:
            status['server_running'] = bool(is_running())
        except Exception:
            pass
    get_pid = getattr(server, 'get_server_pid', None)
    if callable(get_pid):
        try:
            status['server_pid'] = get_pid()
        except Exception:
            pass
    get_information = getattr(server, 'get_server_information', None)
    if callable(get_information):
        try:
            information = get_information()
            status.update({
                key: value
                for key, value in {
                    'minecraft_version': getattr(information, 'version', None),
                    'minecraft_ip': getattr(information, 'ip', None),
                    'minecraft_port': getattr(information, 'port', None),
                }.items()
                if value is not None
            })
        except Exception:
            pass
    get_type = getattr(server, 'get_server_type', None)
    if callable(get_type):
        try:
            status['server_type'] = str(get_type())
        except Exception:
            pass
    get_version = getattr(server, 'get_mcdr_version', None)
    if callable(get_version):
        try:
            status['mcdr_version'] = str(get_version())
        except Exception:
            pass
    return status


def _message_list(value):
    if isinstance(value, dict) and isinstance(value.get('messages'), list):
        return value['messages']
    if isinstance(value, list):
        return value
    raise RuntimeError(f'接口返回格式不是数组: {type(value).__name__}')


def _prime_cursor(server, config):
    cursor = 0
    scanned = 0
    while True:
        messages = _message_list(
            _request(
                config,
                'GET',
                f"/api/mcdr/messages/{int(config['server_id'])}?after_id={cursor}",
            )
        )
        if not messages:
            break
        next_cursor = cursor
        for item in messages:
            if isinstance(item, dict):
                try:
                    next_cursor = max(next_cursor, int(item['id']))
                except (KeyError, TypeError, ValueError):
                    continue
        scanned += len(messages)
        if next_cursor <= cursor:
            break
        cursor = next_cursor
    PLUGIN_STATE['last_id'] = cursor
    _debug(server, config, f'启动游标已定位到 {cursor}，静默跳过 {scanned} 条历史消息')


def on_load(server: PluginServerInterface, old):
    config = _load_config(server)
    if not config.get('token') and config.get('mcdr_token'):
        config['token'] = config['mcdr_token']
    if not config.get('server_id') and config.get('serverId'):
        config['server_id'] = config['serverId']
    if not config.get('api_base') and config.get('api_url'):
        config['api_base'] = config['api_url']
    config['api_base'] = str(config.get('api_base') or '').strip().rstrip('/')
    config['token'] = str(config.get('token') or '').strip()
    try:
        config['server_id'] = int(config.get('server_id') or 0)
    except (TypeError, ValueError):
        config['server_id'] = 0
    server.logger.info(
        f'USTBL 消息同步插件已加载，配置文件={CONFIG_FILE}，'
        f'api_base={config["api_base"]!r}，server_id={config["server_id"]}，'
        f'token_configured={bool(config["token"])}，debug_logging={_is_enabled(config.get("debug_logging"))}'
    )
    _debug(server, config, f'插件已加载，api_base={config["api_base"]!r}, server_id={config["server_id"]}, token_configured={bool(config["token"])}')
    PLUGIN_STATE.update(running=True, config=config, last_id=0, cursor_primed=False, last_config_warning=None)
    server.register_help_message('!!ustbl', '显示 USTBL 消息同步状态')
    server.register_command(
        Literal('!!ustbl').runs(
            lambda source: source.get_server().reply(source, 'USTBL 消息同步已启用')
        )
    )

    def poll_messages():
        last_heartbeat = 0.0
        while PLUGIN_STATE['running']:
            try:
                ready, reason = _config_ready(config)
                if not ready:
                    if PLUGIN_STATE.get('last_config_warning') != reason:
                        server.logger.warning(f'USTBL 消息同步未启动: {reason}')
                        PLUGIN_STATE['last_config_warning'] = reason
                    time.sleep(10)
                    continue
                now = time.monotonic()
                if not PLUGIN_STATE.get('cursor_primed'):
                    _prime_cursor(server, config)
                    PLUGIN_STATE['cursor_primed'] = True
                    continue
                _debug(server, config, f'开始轮询 server_id={config["server_id"]}, after_id={PLUGIN_STATE["last_id"]}')
                messages = _request(
                    config,
                    'GET',
                    f"/api/mcdr/messages/{int(config['server_id'])}?after_id={PLUGIN_STATE['last_id']}",
                )
                messages = _message_list(messages)
                _debug(server, config, f'轮询成功，收到 {len(messages)} 条消息')
                broadcast_count = 0
                for item in messages:
                    if not isinstance(item, dict):
                        _debug(server, config, f'忽略非对象消息: {item!r}')
                        continue
                    try:
                        message_id = int(item['id'])
                    except (KeyError, TypeError, ValueError):
                        _debug(server, config, f'忽略没有有效 id 的消息: {item!r}')
                        continue
                    _debug(server, config, f'消息 id={item.get("id")}, source_type={item.get("source_type")!r}, sender={item.get("sender")!r}')
                    if item.get('source_type') != 'ustbl':
                        PLUGIN_STATE['last_id'] = max(PLUGIN_STATE['last_id'], message_id)
                        continue
                    text = f"[USTBL] {item.get('sender', 'USTBL')}: {item.get('content', '')}"
                    server.say(text)
                    PLUGIN_STATE['last_id'] = max(PLUGIN_STATE['last_id'], message_id)
                    broadcast_count += 1
                    server.logger.info(f'收到 USTBL 消息并广播到 Minecraft: {text}')
                    _debug(server, config, f'已广播到 Minecraft: {text!r}')
                if messages:
                    _debug(server, config, f'本轮实际广播 {broadcast_count} 条，last_id={PLUGIN_STATE["last_id"]}')
            except (socket.timeout, TimeoutError, urllib.error.URLError) as exc:
                server.logger.warning(f'USTBL 消息同步请求超时，请检查 api_base、服务器网络和 vUSTB 部署状态: {exc}')
            except Exception as exc:
                server.logger.warning(f'USTBL 消息同步失败: {exc}')
            try:
                if ready and now - last_heartbeat >= max(5, int(config.get('heartbeat_interval', 30))):
                    _request(config, 'POST', f"/api/mcdr/heartbeat/{int(config['server_id'])}", {'status': _server_status(server)})
                    last_heartbeat = now
                    _debug(server, config, '心跳上报成功')
            except Exception as exc:
                server.logger.warning(f'USTBL 心跳上报失败: {exc}')
            time.sleep(max(1, int(config.get('poll_interval', 3))))

    thread = threading.Thread(target=poll_messages, name='ustbl-message-sync', daemon=True)
    PLUGIN_STATE['thread'] = thread
    thread.start()
    _debug(server, config, '轮询线程已启动')


def on_unload(server: PluginServerInterface):
    config = PLUGIN_STATE.get('config')
    if config and config.get('server_id') and config.get('token'):
        try:
            status = _server_status(server)
            status['online'] = False
            _request(config, 'POST', f"/api/mcdr/heartbeat/{int(config['server_id'])}", {'status': status})
        except Exception as exc:
            server.logger.debug(f'USTBL 离线状态上报失败: {exc}')
    PLUGIN_STATE['running'] = False
    thread = PLUGIN_STATE.get('thread')
    if thread is not None and thread.is_alive():
        thread.join(timeout=2)


@new_thread('ustbl_message_sync_chat')
def on_info(server: PluginServerInterface, info: Info):
    sender = str(getattr(info, 'player', '') or '').strip()
    content = str(getattr(info, 'content', '') or '').strip()
    if not sender or not bool(getattr(info, 'is_player', False)):
        match = re.search(r'<([^>]+)>\s*(.*)', content)
        if match:
            sender, content = match.group(1).strip(), match.group(2).strip()
    if not sender or not content:
        if bool(getattr(info, 'is_player', False)):
            server.logger.warning(f'USTBL 服务器消息未能解析玩家聊天: {getattr(info, "raw_content", content)!r}')
        return
    config = PLUGIN_STATE.get('config') or _load_config(server)
    if not config.get('server_id') or not config.get('token'):
        return
    try:
        _request(
            config,
            'POST',
            f"/api/mcdr/messages/{int(config['server_id'])}",
            {'sender': sender, 'content': content},
        )
        server.logger.info(f'已同步 Minecraft 消息到 USTBL: {sender}: {content}')
    except Exception as exc:
        server.logger.warning(f'USTBL 服务器消息上报失败: {exc}')
