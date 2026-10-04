# USTBL MCDR 消息插件

`ustbl_message_sync.py` 适用于 MCDR 服务器，监听玩家聊天并通过 vUSTB 写入服务器消息流，同时轮询来自 USTBL 的消息并使用 `tellraw` 广播到服务器。

部署前需要：

1. 先在 vUSTB 管理后台的 Minecraft → 服务器管理创建或维护服务器；再进入消息管理选择已有服务器，确认服务器 ID、启用消息并生成该服务器专用 Token。消息管理不会重复创建服务器。
2. 将 `USTBLMessageSync-v1.2.0.mcdr` 直接复制到 MCDR 的 `plugins` 目录，然后在 MCDR 中重载插件。
3. 首次运行会自动生成 `config/USTBLMessageSync/USTBLMessageSync.json`：填写 `api_base`、管理后台对应的 `server_id`、同一个服务器 Token，并按需调整 `poll_interval`、`heartbeat_interval` 与 `request_timeout`。配置目录和文件名都固定为 `USTBLMessageSync`；`api_base` 可填写 `https://www.ustb.world` 或带 `/api` 的地址，插件会自动规范化。

如果使用源码方式，请把 `ustbl_message_sync.py` 复制到 `plugins`；源码文件名必须与插件 ID 一致，文件内部已经包含 `PLUGIN_METADATA`，不需要再单独放 JSON。建议使用 `.mcdr` 包以确保插件清单与源码版本一致。

配置示例：

```json
{
  "api_base": "https://www.ustb.world",
  "server_id": 12,
  "token": "在消息管理中生成的 Token",
  "poll_interval": 3,
  "heartbeat_interval": 30,
  "request_timeout": 20,
  "debug_logging": true
}
```

开启 `debug_logging` 后，MCDR 日志会记录插件加载、每轮轮询、接口返回数量、收到的消息以及实际广播结果；这样可以区分 Token、server_id、网络和 Minecraft 广播环节的问题。

插件同时发送 `X-MCDR-Token`、`X-USTBL-MCDR-Token` 和 Bearer Authorization，兼容会清理自定义请求头的反向代理。Token 会自动去除首尾空格；收到 401 时会明确提示检查 Token 是否复制完整以及 `server_id` 是否匹配。插件会独立执行轮询和心跳，即使轮询失败也会继续上报心跳。玩家聊天支持 MCDR 的玩家事件字段和常见的 `<玩家> 内容` 格式，无法解析时会写入警告日志，不再静默丢弃。

如果要兼容旧的统一 Token，也可以在 vUSTB 服务环境设置 `MCDR_MESSAGE_TOKEN`，但服务器专用 Token 优先。

插件只转发纯文本，服务端消息会在 vUSTB 保存三天后自动过期；插件还会定期上报在线状态、Minecraft 版本、监听地址、端口、进程 ID 和最近心跳时间，供管理后台查看。MCDR 卸载插件时会主动上报离线状态。

每次 MCDR/Minecraft 启动或插件重载时，插件会先静默定位服务器消息流的最新 ID，不会把启动前的历史消息重新广播到 Minecraft；启动完成后只接收新产生的 USTBL 消息。USTBL 和 vUSTB 管理页面仍可查看保留期内的完整消息。

好友消息通过 vUSTB 的 Redis 发布订阅实时转发，vUSTB 不建立好友聊天记录；启动器通过持续事件流接收消息，进程断开后消息不会补存。
