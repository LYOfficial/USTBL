# 资源加速开发与验证

GitHub 加速由本地 HTTP/CONNECT 代理和 vUSTB 中转组成。配置与认证检查通过后才修改 Windows 系统代理；接管前持久化原设置，停止或正常退出时恢复，下次启动也尝试恢复崩溃遗留设置。若其他程序已改变代理，不覆盖其新设置。

IPv6 免流将 FreeV6 的 WARP MASQUE 接入、28 个 IPv6 节点和 mihomo TUN 移植到 Rust。核心首次使用时下载固定版本并校验摘要；凭据目录限制访问，控制接口仅监听本机并使用随机密钥，子进程由 Windows Job Object 管理。来源与许可证见 README 和 `docs/licenses/free-v6-MIT.txt`。

## 本地检查

```powershell
node --test scripts/tests/*.test.cjs
npx tsc --noEmit
npx eslint src --max-warnings=0
npm run build
cargo test --locked --manifest-path src-tauri/Cargo.toml --lib
```

真实核心配置、节点控制与生命周期测试需要明确指定官方 mihomo 可执行文件：

```powershell
$env:USTBL_TEST_MIHOMO = 'C:\path\to\mihomo-windows-amd64-compatible.exe'
cargo test --locked --manifest-path src-tauri/Cargo.toml --lib real_core_accepts_masque_config_and_authenticated_node_control
```

该测试关闭 TUN，不修改真实网络路由。GitHub 部署测试 `deployed_proxy_start_and_stop` 默认忽略，只有配置官方构建凭据且确认中转可用后才显式执行。

## 桌面验证

- 在实际 Tauri 主窗口检查设置页、资源加速页与托盘，连续切换页面后确认没有重复监听、白屏或未处理异常。
- 验证轮询能更新托盘等外部操作产生的状态；未保存的校园网绕过规则应保持不变。启动、停止和节点检测期间不允许重复提交。
- 比较测试前后的完整 Windows 代理快照；覆盖正常停止、正常退出、启动失败和异常终止后的下次启动恢复。
- 在管理员权限和可用公网 IPv6 下，验证启动后 IPv6 出口、节点切换、校园网域名/CIDR 绕过，以及停止和退出后的 TUN 清理。校园网计费需要结合实际网络政策验证。

## 当前验证范围

已验证真实 mihomo 配置加载、认证控制接口、节点查询/选择、停止清理，以及非管理员拒绝启动；本地代理测试覆盖 HTTP 请求体、CONNECT 非标准端口、隧道预读数据、认证失败和请求头上限。实际主窗口导航与正常退出已验证。

网络复核确认，本机使用 ULA IPv6 地址并经路由器地址转换访问 IPv6 外网。启动检查接受具有 IPv6 路由的公网或 ULA 源地址，随后仍需验证真实 WARP 通道，避免误拒绝 IPv6 地址转换网络。实际网络路由测试 `current_network_has_an_ipv6_route` 默认忽略，可显式运行；该测试不启用 TUN。

进程未以管理员运行，因此未完成校园网免流或真实 TUN 路由测试。当前 vUSTB 公网代理端口连接超时。GitHub 加速的桌面联网检查还触发了 Windows Defender 的 `Behavior:Win32/LummaStealer.CER!MTB` 检测，事件记录确认测试进程被终止；未改变防病毒设置，尚不能确定检测原因或声称全链路验证通过。应在发行前核查构建与第三方依赖，并通过安全软件厂商的正常分析流程核实检测。
