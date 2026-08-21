![USTBL](https://socialify.git.ci/USTB-SkyCode/USTBL/image?description=1&font=Inter&forks=1&language=1&logo=https%3A%2F%2Foss.1n.hk%2Fimage%2F08af3985-64a0-4ec0-9de7-bf82ce447513%2Fed5858e426bde933.png&name=1&owner=1&stargazers=1&theme=Auto)

# USTB Servers Launcher 启动器

> 根据 [@SJMCL](https://github.com/UNIkeEN/SJMCL) 项目开发！

## 功能特性

* **平台支持**：当前仅支持 Windows 10/11。
* **高效的实例管理**：支持多个游戏目录与实例，集中管理所有实例资源（如存档、模组、资源包、光影包、截图等）与设置。
* **便捷的资源下载**：支持从 CurseForge 与 Modrinth 等源下载游戏客户端、模组加载器、各类游戏资源与整合包。
* **多账户系统支持**：内置 Microsoft 登录与第三方认证服务器支持，兼容 Yggdrasil Connect 的 OAuth 登录流程规范提案。
* **像素北科账号**：账号页可通过 OAuth 设备流登录 [像素北科 vUSTB](https://www.ustb.world)，显示头像、用户名、用户组并同步全部 Minecraft 角色；注销会同时移除对应的像素北科游戏角色。
* **共享实例**：可浏览像素北科维护的共享实例并同步模组。更新只删除共享档案标记为 `deleted` 的模组、补齐缺失的 `used` 模组，不会干预本地的其他模组；超级管理员、管理员、平台管理员和服务器管理员可编辑共享模组档案。
* **深度链接集成**：可与外部网站与工具集联动，支持通过系统深度链接、桌面快捷方式一键启动实例等便捷功能。

> 注意：部分功能可能受地区、运行平台或程序分发类型限制。

### v0.4.0 更新

- 像素北科从普通第三方皮肤站升级为启动器账号系统，网站账户与 Minecraft 启动角色独立保存、联动注销。
- 移除 MUA 用户中心与 LittleSkin 的默认第三方皮肤站预设；仍可按需手动添加其他认证服务器。
- 实例页新增“共享实例”，支持查看、绑定本地实例、同步与按差异更新共享模组。
- 具备像素北科 Minecraft 管理权限的用户可在共享实例中上传或标记删除模组。

### 技术栈

[![Tauri](https://img.shields.io/badge/Tauri-v2-FFC131?style=for-the-badge&logo=tauri&logoColor=white&labelColor=24C8DB)](https://tauri.app/)
[![Next JS](https://img.shields.io/badge/next.js-000000?style=for-the-badge&logo=nextdotjs&logoColor=white)](https://nextjs.org/)
[![Chakra UI](https://img.shields.io/badge/chakra_ui-v2-38B2AC?style=for-the-badge&logo=chakraui&logoColor=white&labelColor=319795)](https://v2.chakra-ui.com/)

## 开始使用

开始使用 USTBL，只需在 [GitHub Releases](https://github.com/USTB-SkyCode/USTBL/releases) 获取所有版本，包括周期性构建。

USTBL 目前支持以下平台：

| 平台    | 系统版本            | 架构               | 提供的的分发类型                              |
|---------|---------------------|--------------------|--------------------------------------------|
| Windows | 7 及以上           | `aarch64`, `i686`, `x86_64`  | 安装版 `.exe`，便携版 `.exe` |

### Windows 7

如果您需要在 Windows 7 运行 USTBL，请先 [下载 Microsoft Edge WebView2 运行时](https://developer.microsoft.com/zh-cn/microsoft-edge/webview2#download) 并安装之，推荐选择“常青引导程序”。


## 开发与贡献

首先克隆本项目并安装前端依赖：

```bash
git clone https://github.com/USTB-SkyCode/USTBL.git
npm install
```

使用开发模式运行：

```bash
npm run tauri dev
```
