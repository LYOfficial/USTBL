# USTBL 开发与技术文档

> 面向参与 USTBL 开发、构建和发布的贡献者。普通用户请从 [README](../README.md) 的下载与使用说明开始。

## 技术概览

| 层级       | 技术与职责                                                                   |
| ---------- | ---------------------------------------------------------------------------- |
| 桌面运行时 | [Tauri v2](https://v2.tauri.app/) 负责原生窗口、系统能力、打包和前后端通信。 |
| 前端       | Next.js 15 的静态导出模式、React 18、TypeScript 与 Chakra UI。               |
| 后端       | Rust 2021，处理账户、实例、资源下载、游戏启动、配置与本地文件。              |
| 构建与发布 | npm、Cargo、Tauri CLI、GitHub Actions 和 NSIS（Windows 安装包）。            |

应用的数据流如下：

```text
页面 / 组件 → Context 与 Service → Tauri invoke → Rust command / domain helper → 文件、进程与网络服务
```

前端仅作为静态资源构建到 `out/`；Tauri 在开发时启动 Next.js，在生产构建时将静态资源打包进应用。

## 仓库结构

| 路径                        | 说明                                                        |
| --------------------------- | ----------------------------------------------------------- |
| `src/pages/`                | Next.js 页面与路由。                                        |
| `src/components/`           | 可复用界面、模态框和交互组件。                              |
| `src/contexts/`             | 全局状态与跨页面交互。                                      |
| `src/services/`             | 前端对 Tauri `invoke` 命令的类型化封装。                    |
| `src/models/`、`src/enums/` | 前端模型、枚举和 mock 数据。                                |
| `src/locales/`              | 界面翻译文件。                                              |
| `src-tauri/src/`            | Rust 代码；按账户、实例、资源、启动、任务和配置等领域组织。 |
| `src-tauri/capabilities/`   | Tauri 权限声明。                                            |
| `src-tauri/tauri.conf.json` | 应用窗口、深度链接、打包目标和资源配置。                    |
| `scripts/`                  | 版本、本地化、游戏元数据、安装器和发布辅助脚本。            |
| `.github/workflows/`        | Windows 构建、夜间构建、发布和数据更新自动化。              |

## 环境要求

- Node.js LTS 与 npm（CI 使用 Node.js LTS）。
- Rust stable，且版本不低于 `1.91.0`（见 `src-tauri/Cargo.toml`）。
- Windows 开发环境需要 Microsoft C++ Build Tools 与对应的 Windows SDK；建议通过 [Visual Studio Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/) 安装“Desktop development with C++”。
- 用于运行桌面界面的 [Microsoft Edge WebView2 Runtime](https://developer.microsoft.com/microsoft-edge/webview2/)。
- 需要制作便携包或运行部分发布脚本时，安装 Python 3。

在 PowerShell 中检查环境：

```powershell
node --version
npm --version
rustc --version
cargo --version
```

## 本地开发

克隆项目并安装锁定的前端依赖：

```bash
git clone https://github.com/LYOfficial/USTBL.git
cd USTBL
npm ci
```

### 可选环境变量

`src-tauri/build.rs` 会在本地构建时读取 `.env`；没有 `.env` 时会回退到 `.env.template`。如需安装 CurseForge 整合包，请复制模板并填写 API Key：

```powershell
Copy-Item .env.template .env
```

```dotenv
USTBL_CURSEFORGE_API_KEY="your-key"
```

以 `USTBL_` 开头的变量会在编译期传入 Rust 二进制。不要将个人密钥提交到仓库；发布包含此类变量的构建前，也应确认它们适合公开分发。

### 启动调试环境

```bash
npm run tauri dev
```

该命令会启动 Next.js 开发服务器，并打开 Tauri 桌面窗口。`npm run dev` 只启动前端服务器，适合单独调整页面样式；涉及 `invoke`、文件系统、账户或游戏启动的功能必须在 Tauri 环境中验证。

## 构建与检查

在提交前至少执行：

```bash
npm run lint
npm run build
```

构建 Windows 桌面程序：

```bash
npm run tauri build
```

指定 Rust 目标三元组时，将参数透传给 Tauri CLI：

```bash
npm run tauri build -- --target x86_64-pc-windows-msvc
```

构建产物位于 `src-tauri/target/<target>/release/`，NSIS 安装程序位于对应的 `bundle/nsis/` 目录。Windows CI 同时产出安装版和便携版；便携版会由 `scripts/release/bundle_portable_assets.py` 注入所需资源。

常用维护命令：

| 命令                             | 用途                                        |
| -------------------------------- | ------------------------------------------- |
| `npm run lint`                   | 运行 ESLint 与 Prettier 规则。              |
| `npm run build`                  | 执行 Next.js 静态导出。                     |
| `npm run version check`          | 检查前端、Tauri 与 Cargo 的版本一致性。     |
| `npm run version bump <version>` | 更新项目版本；提交前仍应运行版本检查。      |
| `npm run locale diff en`         | 检查英文翻译键与基准翻译的差异。            |
| `npm run locale diff zh-Hans`    | 检查简体中文翻译键与基准翻译的差异。        |
| `npm run assets:installer`       | 在 Windows 上重新生成 NSIS 安装器图像资源。 |

## 功能开发约定

### 新增一个前后端能力

1. 在 `src-tauri/src/<domain>/` 中实现模型、辅助逻辑与 `commands.rs` 命令。
2. 将命令注册到 `src-tauri/src/lib.rs` 的 `tauri::generate_handler!`。
3. 若使用了新的 Tauri 权限或插件能力，更新 `src-tauri/capabilities/` 中的声明。
4. 在 `src/services/` 新增或扩展对应的 TypeScript 服务，用 `invoke` 调用 Rust 命令，并通过现有 `responseHandler` 统一处理响应。
5. 在页面、组件与 Context 中消费服务；为加载、空状态、取消和失败路径提供界面反馈。
6. 使用 `npm run tauri dev` 实测完整调用链，并运行 lint 与构建检查。

不要在 React 组件中直接散落原生调用或复杂业务逻辑；优先保持“页面 / 组件 → service → Rust command”的边界。涉及删除、覆盖或启动外部进程的操作，应在前端提供明确确认，并在 Rust 端验证输入路径和状态。

### 本地化

界面文案放在 `src/locales/*.json`，新增键时应同步更新至少英文与简体中文，并运行相应的 `npm run locale diff` 命令。避免把面向用户的长文案硬编码在组件内。

### 格式与提交

- 使用仓库中的 `.prettierrc` 和 `.eslintrc.json`；项目采用 2 空格缩进、双引号与分号。
- Husky 与 lint-staged 会在提交时检查变更的 TypeScript、Rust、脚本与翻译文件。
- 保持提交聚焦：不要把自动生成数据、格式化和功能改动混在同一提交，除非它们不可分割。
- 修改版本或安装器资源前，先阅读相关脚本与 GitHub Actions 工作流，避免前端、Cargo 和 Tauri 版本不一致。

## 发布与自动化

- `Win-Build.yml` 为 x86、x64 和 Windows ARM64 构建应用与便携包。
- `nightly.yml` 维护夜间构建；`release.yml` 汇总产物并创建草稿 Release。
- `update_version_list.yml` 与 `update_mod_data.yml` 会通过 Pull Request 更新 Minecraft 版本和 Mod 数据。

不要在本地提交工作流自动生成的结果前忽略其来源、范围或许可要求。发布前应在干净工作区完成版本检查、前端构建和目标平台的 Tauri 构建。

## 贡献与许可

请先搜索 [Issues](https://github.com/LYOfficial/USTBL/issues)，再按模板报告问题或提出功能建议。提交 Pull Request 前，请说明问题背景、实现范围与手动验证方式。

USTBL 基于 [SJMCL](https://github.com/UNIkeEN/SJMCL) 开发，采用 [GPL-3.0](../LICENSE) 并附加 [额外许可条款](../LICENSE.EXTRA)。分发修改版本前，必须满足两份许可文件中的要求，包括必要的来源说明与命名限制。
