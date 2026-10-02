# USTBL 发布手册

## 正常发版

1. 更新版本号：`npm run version bump 0.4.5`，然后执行 `npm run version check`。
2. 复制 `.github/release-notes/TEMPLATE.md`，命名为 `.github/release-notes/v0.4.5.md`，写好文案。HTML 注释不会被发布，Full Changelog 链接会自动追加。
3. 版本号和文案随 PR 一起合入 `main`，合入后自动开始构建，约 25 分钟。
4. 打开 Actions → Release 查看进度。完成后，运行摘要里有草稿链接。
5. 在 Releases 页面打开草稿，核对文案和下面 3 个文件，确认无误后点 Publish。
6. 在 vUSTB 后台 `/admin/launcher` 点"同步 GitHub Releases"，需要的话再点"同步至云盘"。

固定文件：

| 文件 | 说明 |
| --- | --- |
| `USTBL_<版本>_x64-setup.exe` | 安装版 |
| `USTBL_<版本>_windows_x86_64_portable.exe` | 便携版 |
| `SHA256SUMS.txt` | 上面两个文件的 SHA-256 |

## 发布前要改文案或代码

- **只改文字**：直接在草稿页面编辑即可，不用重新构建。
- **需要重新构建**（改了代码，或者想以仓库里的文案文件为准）：修改 `v<版本>.md` 并合入 `main`。新构建成功后会替换旧草稿，失败时旧草稿保留。

## 发布后

- 已发布版本的文案直接在 Release 页面编辑。修改对应的 md 文件不会触发任何操作。
- 已发布的版本不会被重新构建。要修复问题，请发新版本。

## 手动运行

Actions → Release → Run workflow（只能选默认分支）：

- `version`：不带 v 的版本号，必须和 `package.json` 一致。
- `previous_tag`：可以不填，默认取最近的 `v*` tag，用于生成 Full Changelog 链接和提交列表。

没有 `v<版本>.md` 时，文案改为列出本次包含的提交（提交号 + 标题）。已有同版本草稿时会替换，版本已发布时直接失败。

"Re-run all jobs" 会用原来的提交和参数把那次运行再跑一遍，只适合网络等原因导致的偶然失败。

## 常见失败原因

| 报错 | 处理 |
| --- | --- |
| `does not match package.json version` | 文件名或输入的版本号和 `package.json` 不一致 |
| `Multiple release notes files changed` | 一次推送只改动一个版本的文案文件 |
| `Tag vX already exists` | 该版本已发布，请发新版本 |
| `must be built from the default branch` | 只能在 `main` 上运行 |
| `USTBL_CURSEFORGE_API_KEY secret is required` | 配置上面的 Secret |

## CI 检查

每次推送和 PR 都会运行 CI：版本一致性、ESLint、翻译键、脚本测试、前端构建、`cargo test`，以及改动 Rust 文件的格式检查。本地提交时 husky 也会做同样的检查，CI 失败时先在本地复现。
