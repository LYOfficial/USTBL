# 本地依赖安全补丁

以下库通过主项目的 `[patch.crates-io]` 使用本地源码，保留原版本号和许可证。不要将它们替换为同版本的未修复注册表包。

## glib 0.18.5

- 来源：<https://crates.io/crates/glib/0.18.5>，MIT 许可证。
- 原始 `.crate` SHA-256：`233daaf6e83ae6a12a52055f568f9d7cf4671dabb78ff9560ab6da230ce00ee5`。
- Tauri 2 的 Linux GTK 3 依赖仍要求 glib 0.18，不能直接换成 API 不兼容的 0.20。
- 为修复 [GHSA-wrw7-89jp-8q8g](https://github.com/advisories/GHSA-wrw7-89jp-8q8g)，完整回移 [上游 PR #1343](https://github.com/gtk-rs/gtk-rs-core/pull/1343) 的两行修复：`VariantStrIter::impl_get` 的输出指针声明为可变，并传递 `&mut p`。
- 上游补丁提交：`b5a4071e439bef2b5eea76c3aa25e5ae84839e34`。
- 保留上游源码、测试和许可证；CI 在 Linux 上以 release 优化运行 `variant_iter::tests`，覆盖正向和反向迭代，避免优化后出现未定义行为。
- 待 Tauri 的 GTK 依赖兼容已修复的 glib 版本后，删除本地补丁并更新锁文件。

## mc-server-status 1.0.0

- 来源：<https://crates.io/crates/mc-server-status/1.0.0>，MIT 许可证。
- 原始 `.crate` SHA-256：`852b74807c953dbdc84cda9f3a2399ec7ca441bdb4750f6d9efb60ffb2b0cdb8`。
- 公开 API 保持一致；依赖 `trust-dns-resolver 0.23.2` 改为其后继项目 `hickory-resolver 0.26.3`，并适配新的异步解析器构建接口，保留原来的 Google DNS 服务器配置。
- 新的依赖链使用 idna 1.x，移除 [GHSA-h97m-ww89-6jmq](https://github.com/advisories/GHSA-h97m-ww89-6jmq) 涉及的 idna 0.4。
- 同时避开旧版 Hickory 的 [DNSSEC 校验错误处理问题](https://github.com/advisories/GHSA-5j98-2g5x-46v6) 和 [DNS 编码 CPU 耗尽问题](https://github.com/advisories/GHSA-q2qq-hmj6-3wpp)。
- 移除未随本地副本提供的示例目标；保留源码、README 和许可证。
- 待上游发布包含该修复的版本后，删除本地补丁并更新锁文件。

## phf_generator 0.8.0

- 来源：<https://crates.io/crates/phf_generator/0.8.0>，MIT 许可证。
- 原始 `.crate` SHA-256：`17367f0cc86f2d25802b2c26ee58a7b23faeccf78a396094c13dced0d0182526`。
- Tauri 的 HTML 解析器通过 selectors 0.24 使用旧版 PHF 生成器，该版本仍依赖 rand 0.7，无法仅更新锁文件来移除旧版 rand。
- 将 rand 依赖提高到 0.8.6，移除 [GHSA-cq8v-f236-94qc](https://github.com/advisories/GHSA-cq8v-f236-94qc) 涉及的 rand 0.7；保留原 PHF 算法和公开接口。
- 不包含上游基准测试及其 criterion 依赖；许可证取自上游源码提交 `81c7cc5b48649108428671d3b8ad151f6fbdb359` 的 `LICENSE`。
- 生成器使用固定种子，生成出的哈希表必须经过无碰撞查找回归测试；Tauri 的构建也会实际执行此生成器。
- 待上游依赖链不再使用 phf_generator 0.8 后，删除本地补丁并更新锁文件。
