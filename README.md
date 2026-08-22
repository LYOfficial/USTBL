<p align="center">
  <img src="./public/images/icons/Logo_128x128.png" width="128" alt="USTBL logo" />
</p>

<h1 align="center">USTBL</h1>

<p align="center">为 USTB Servers 与 Minecraft 玩家准备的桌面启动器。</p>

<p align="center">
  <a href="https://github.com/LYOfficial/USTBL/releases"><img src="https://img.shields.io/github/v/release/LYOfficial/USTBL?display_name=tag&label=Release" alt="Latest release" /></a>
  <a href="https://github.com/LYOfficial/USTBL/blob/main/LICENSE"><img src="https://img.shields.io/github/license/LYOfficial/USTBL" alt="License" /></a>
  <a href="https://github.com/LYOfficial/USTBL/issues"><img src="https://img.shields.io/github/issues/LYOfficial/USTBL" alt="Open issues" /></a>
</p>

<p align="center">
  <a href="#下载与安装">下载</a> ·
  <a href="#快速开始">快速开始</a> ·
  <a href="#主要功能">主要功能</a> ·
  <a href="#获得帮助">获得帮助</a> ·
  <a href="docs/development.md">开发文档</a>
</p>

## 下载与安装

请从 [GitHub Releases](https://github.com/LYOfficial/USTBL/releases) 下载最新稳定版。当前正式分发面向 Windows 10/11，发布页会按架构提供安装版和便携版：

| 包类型           | 适用场景                                                   |
| ---------------- | ---------------------------------------------------------- |
| `*_setup.exe`    | 推荐大多数用户使用；由安装程序完成安装与卸载。             |
| `*_portable.exe` | 无需安装；适合放在移动存储设备或不希望写入安装目录的场景。 |

请选择与系统架构对应的构建（常见电脑为 `x86_64`；Windows on ARM 请选择 `aarch64`）。下载完成后双击运行即可。若 Windows 提示缺少 WebView2，请先安装 [Microsoft Edge WebView2 Runtime](https://developer.microsoft.com/microsoft-edge/webview2/) 并重新启动启动器。

> 不确定文件是否完整时，可使用发布页提供的 SHA-256 校验值，或在 PowerShell 中运行 `Get-FileHash <文件路径> -Algorithm SHA256`。

## 快速开始

1. **准备 Java**：首次启动后打开「设置 → Java」，确认启动器识别到适合游戏版本的 Java；也可以手动添加本地路径或下载 Java。
2. **添加账户**：在「账户」中添加 Microsoft、离线或第三方认证服务器账户。像素北科用户可通过设备码登录 vUSTB 账户，并同步可用的 Minecraft 角色。
3. **创建或导入实例**：在「实例」中选择「新建 / 导入」，选取 Minecraft 版本和加载器，或导入已有整合包。
4. **安装内容**：在实例详情中下载或导入 Mod、资源包、光影包、存档等资源；开始前可调整内存、Java 和游戏参数。
5. **启动游戏**：回到实例概览，选择要使用的游戏角色并点击启动。启动失败时可从启动器打开日志或导出崩溃信息。

## 主要功能

- **实例与资源管理**：集中管理多个游戏目录和实例，以及各实例的 Mod、存档、资源包、光影包、截图和服务器列表。
- **安装、导入与更新**：创建指定 Minecraft 版本与加载器的实例；浏览并下载 Modrinth 资源，导入常见整合包，并支持 Mod 更新检查。
- **账户系统**：支持 Microsoft、离线和第三方 Yggdrasil 认证账户；可导入其他启动器的账户信息。
- **像素北科集成**：通过 OAuth 设备流登录 [像素北科 vUSTB](https://www.ustb.world)，查看账户资料并同步 Minecraft 角色。
- **共享实例**：浏览、绑定和同步像素北科维护的共享实例；同步只管理共享清单中的 Mod，不会删除其他本地 Mod。
- **启动体验**：检测 Java 与游戏文件、显示启动日志和错误信息，并支持创建桌面快捷方式及 `ustbl://` 深度链接。
- **个性化与多语言**：提供主题、背景、下载与游戏参数等设置，并内置多语言界面。

## 获得帮助

- 使用问题或功能建议请先搜索 [已有 Issue](https://github.com/LYOfficial/USTBL/issues)，再按模板提交新的 Issue。
- 测试构建可能出现在 [Releases](https://github.com/LYOfficial/USTBL/releases) 或项目工作流中；它们适合体验新功能，不建议用于重要存档。
- 提交问题时请附上系统版本、启动器版本、复现步骤，以及必要的日志或崩溃报告；请先移除账号令牌、路径和其他隐私信息。

## 开发与贡献

USTBL 使用 Next.js、React、TypeScript、Tauri v2 和 Rust 构建。贡献者请阅读 [开发与技术文档](docs/development.md)，其中包含环境要求、项目架构、调试、构建、本地化和发布流程。

本项目基于 [SJMCL](https://github.com/UNIkeEN/SJMCL) 开发；分发修改版本前请同时阅读 [GPL-3.0](LICENSE) 与 [附加许可条款](LICENSE.EXTRA)。

## 许可证

本项目采用 [GPL-3.0](LICENSE) 许可证，并附带 [额外条款](LICENSE.EXTRA)。
