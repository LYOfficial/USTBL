"""MCDR plugin: relay server chat and USTBL launcher messages."""

import json
import re
import threading
import time
import urllib.request

from mcdreforged.api.all import *


PLUGIN_METADATA = {
    'id': 'ustbl_message_sync',
    'version': '1.0.0',
    'name': 'USTBL Message Sync',
    'description': '通过 vUSTB 同步服务器与 USTBL 纯文本消息',
    'author': 'USTBL Team',
}

CONFIG_FILE = 'ustbl_message_sync.json'
DEFAULT_CONFIG = {
    'api_base': 'https://www.ustb.world',
    'server_id': 0,
    'token': '',
    'poll_interval': 3,
    'heartbeat_interval': 30,
}
PLUGIN_STATE = {'running': False, 'thread': None, 'config': None, 'last_id': 0}


def _request(config, method, path, payload=None):
    data = None if payload is None else json.dumps(payload).encode('utf-8')
    request = urllib.request.Request(
        config['api_base'].rstrip('/') + path,
        data=data,
        method=method,
        headers={
            'Content-Type': 'application/json',
            'X-MCDR-Token': config['token'],
            'User-Agent': 'USTBL-MCDR/1.0',
        },
    )
    with urllib.request.urlopen(request, timeout=10) as response:
        return json.loads(response.read().decode('utf-8'))


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


def on_load(server: PluginServerInterface, old):
    config = server.load_config_simple(CONFIG_FILE, default_config=DEFAULT_CONFIG)
    PLUGIN_STATE.update(running=True, config=config, last_id=0)
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
                now = time.monotonic()
                if config.get('server_id') and config.get('token') and now - last_heartbeat >= max(5, int(config.get('heartbeat_interval', 30))):
                    _request(
                        config,
                        'POST',
                        f"/api/mcdr/heartbeat/{int(config['server_id'])}",
                        {'status': _server_status(server)},
                    )
                    last_heartbeat = now
                messages = _request(
                    config,
                    'GET',
                    f"/api/mcdr/messages/{int(config['server_id'])}?after_id={PLUGIN_STATE['last_id']}",
                )
                for item in messages:
                    PLUGIN_STATE['last_id'] = max(PLUGIN_STATE['last_id'], int(item['id']))
                    if item.get('source_type') != 'ustbl':
                        continue
                    server.say(f"[USTBL] {item['sender']}: {item['content']}")
            except Exception as exc:
                server.logger.warning(f'USTBL 消息同步失败: {exc}')
            time.sleep(max(1, int(config.get('poll_interval', 3))))

    thread = threading.Thread(target=poll_messages, name='ustbl-message-sync', daemon=True)
    PLUGIN_STATE['thread'] = thread
    thread.start()


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
    match = re.search(r'<([^>]+)>\s*(.*)', info.content)
    if not match:
        return
    config = PLUGIN_STATE.get('config') or server.load_config_simple(
        CONFIG_FILE, default_config=DEFAULT_CONFIG
    )
    if not config.get('server_id') or not config.get('token'):
        return
    try:
        _request(
            config,
            'POST',
            f"/api/mcdr/messages/{int(config['server_id'])}",
            {'sender': match.group(1), 'content': match.group(2).strip()},
        )
    except Exception as exc:
        server.logger.warning(f'USTBL 服务器消息上报失败: {exc}')
