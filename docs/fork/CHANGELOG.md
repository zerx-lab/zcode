# Fork 变更记录

上游 `packages/*/CHANGELOG.md` 属上游所有，fork 侧改动只记在这里。

## [Unreleased]

### Added

- zcode 品牌字标：终端块字符版 `packages/coding-agent/src/modes/components/brand-logo.ts`（`ZCODE_LOGO`，12×5，仅用 `█ ▀ ▄` 以保证 setup splash 2x 放大不发虚）；SVG 版 `brand/logo/zcode-mark.svg`（品牌渐变）与 `brand/logo/zcode-mark-mono.svg`（深底单色）。

### Changed

- `welcome.ts` 的 `PI_LOGO` 改为从 `brand-logo.ts` 取值（1 行 import + 1 行赋值）。名字保持 `PI_LOGO`，welcome 盒 / setup splash / outro / wizard header 四个消费点零改动。

### 同步

- 补丁栈 rebase 到 `upstream/main` @ `45e12e5bb`（193 个上游提交）。冲突 1 处：`crates/pi-natives/src/desktop/linux/wayland/portal.rs` — 上游把 `token_path(name)` 拆成 `omp_state_dir()` + `token_path`，品牌改动收敛为 `base.join("zcode")`。`chore: apply biome formatting to frontmatter.ts` 已进上游，rebase 自动丢弃。
- 从品牌 commit 中剔除误提交的构建产物 `packages/natives/native/embedded-addons.win32-x64.tar.gz`（27 MB）与被 `embed:native` 填充的 `packages/natives/native/embedded-addon.js`，后者恢复为上游 null stub——否则开发态 loader 会误判为 compiled-binary 模式。
