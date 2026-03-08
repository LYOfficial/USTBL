![USTBL](https://socialify.git.ci/USTB-SkyCode/USTBL/image?description=1&font=Inter&forks=1&language=1&logo=https%3A%2F%2Foss.1n.hk%2Fimage%2F08af3985-64a0-4ec0-9de7-bf82ce447513%2Fed5858e426bde933.png&name=1&owner=1&stargazers=1&theme=Auto)

# USTB Servers Launcher 启动器

> 根据 [@SJMCL](https://github.com/UNIkeEN/SJMCL) 项目开发！

## 功能特性

* **跨平台支持**：兼容 Windows 10/11、macOS 与 Linux。
* **高效的实例管理**：支持多个游戏目录与实例，集中管理所有实例资源（如存档、模组、资源包、光影包、截图等）与设置。
* **便捷的资源下载**：支持从 CurseForge 与 Modrinth 等源下载游戏客户端、模组加载器、各类游戏资源与整合包。
* **多账户系统支持**：内置 Microsoft 登录与第三方认证服务器支持，兼容 Yggdrasil Connect 的 OAuth 登录流程规范提案。
* **深度链接集成**：可与外部网站与工具集联动，支持通过系统深度链接、桌面快捷方式一键启动实例等便捷功能。

> 注意：部分功能可能受地区、运行平台或程序分发类型限制。

### 技术栈

[![Tauri](https://img.shields.io/badge/Tauri-v2-FFC131?style=for-the-badge&logo=tauri&logoColor=white&labelColor=24C8DB)](https://tauri.app/)
[![Next JS](https://img.shields.io/badge/next.js-000000?style=for-the-badge&logo=nextdotjs&logoColor=white)](https://nextjs.org/)
[![Chakra UI](https://img.shields.io/badge/chakra_ui-v2-38B2AC?style=for-the-badge&logo=chakraui&logoColor=white&labelColor=319795)](https://v2.chakra-ui.com/)

## 开始使用

开始使用 SJMCL，只需前往 [官网](https://mc.sjtu.cn/sjmcl/en) 下载最新版即可。

您也可以在 [GitHub Releases](https://github.com/UNIkeEN/SJMCL/releases) 获取所有版本，包括周期性构建。

SJMCL 目前支持以下平台：

| 平台    | 系统版本            | 架构               | 提供的的分发类型                              |
|---------|---------------------|--------------------|--------------------------------------------|
| Windows | 7 及以上           | `aarch64`, `i686`, `x86_64`  | 安装版 `.exe`，便携版 `.exe` |
| macOS   | 10.15 及以上        | `aarch64`, `x86_64` | `.app`，`.dmg`                   |
| Linux   | webkit2gtk 4.1 (如 Ubuntu 22.04) | `aarch64`, `x86_64` | `.AppImage`, `.deb`, `.rpm`, 便携版二进制文件 |

了解更多功能与常见问题，请参阅 [用户文档](https://mc.sjtu.cn/sjmcl/zh/docs)。

### Windows 7

如果您需要在 Windows 7 运行 SJMCL，请先 [下载 Microsoft Edge WebView2 运行时](https://developer.microsoft.com/zh-cn/microsoft-edge/webview2#download) 并安装之，推荐选择“常青引导程序”。

<details>
<summary><h3>从命令行安装</h3></summary>

<details>
<summary><h4>Arch Linux</h4></summary>

SJMCL 已上传至 Arch Linux 用户仓库（AUR）。您可以使用常见的 [AUR 助手](https://wiki.archlinux.org/title/AUR_helpers) 安装：

```bash
yay -S sjmcl-bin
```

如不使用 AUR 助手，您也可以手动安装：

```bash
git clone https://aur.archlinux.org/sjmcl-bin.git
cd sjmcl-bin
makepkg -si
```

</details>
</details>

## 开发与贡献

首先克隆本项目并安装前端依赖：

```bash
git clone git@github.com:UNIkeEN/SJMCL.git
npm install
```

使用开发模式运行：

```bash
npm run tauri dev
```