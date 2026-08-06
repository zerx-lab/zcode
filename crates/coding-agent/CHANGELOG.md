# Changelog

格式与归属标注规则见 `rule://zcode-workflow`。

## [Unreleased]
### Breaking Changes

- 项目许可证从 `MIT OR Apache-2.0` 切换为 `AGPL-3.0-only`。

### Added

- CLI 入口骨架：启动时声明 worker host 入口，`--version` / `--help` 可用，无参数时打印帮助。

### Changed

- MSRV 从 1.92 提到 **1.95**（workspace `rust-version`，全体成员继承）。下限由依赖决定：
  `libsqlite3-sys 0.38` 的 build script 用 `cfg_select!`（Rust 1.95 稳定），且它自己没声明
  `rust-version`，Cargo 拦不住——继续写 1.92 只会让 CI 的 MSRV job 在编译期炸。
