# USTBL MCDR 消息插件

`mcdr_ustbl_message_sync.py` 适用于 MCDR 服务器，监听玩家聊天并通过 vUSTB 写入服务器消息流，同时轮询来自 USTBL 的消息并使用 `tellraw` 广播到服务器。

部署前需要：

1. 在 vUSTB 管理后台的 Minecraft → 消息管理确认服务器地址和端口，启用消息并生成该服务器专用 Token。
3. 将 `mcdreforged.plugin.json` 与 `mcdr_ustbl_message_sync.py` 放入 MCDR 插件目录并安装插件。
4. 首次运行后编辑 `config/ustbl_message_sync/ustbl_message_sync.json`：填写 `api_base`、管理后台对应的 `server_id`、同一个服务器 Token，并按需调整 `poll_interval` 与 `heartbeat_interval`。

如果要兼容旧的统一 Token，也可以在 vUSTB 服务环境设置 `MCDR_MESSAGE_TOKEN`，但服务器专用 Token 优先。

插件只转发纯文本，服务端消息会在 vUSTB 保存三天后自动过期；插件还会定期上报在线状态、Minecraft 版本、监听地址、端口、进程 ID 和最近心跳时间，供管理后台查看。MCDR 卸载插件时会主动上报离线状态。

好友消息通过 vUSTB 的 Redis 发布订阅实时转发，vUSTB 不建立好友聊天记录；启动器通过持续事件流接收消息，进程断开后消息不会补存。
