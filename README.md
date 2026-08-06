# ZCode — Agents Harness

终端里的 coding agent，Rust 实现。

> **当前状态：架构骨架。** workspace、闸门与多平台编排已就绪；CLI 只有 `--help` / `--version`，
> 各能力 crate 已划定边界但尚未落实现。

## 快速开始

工具链版本由 `rust-toolchain.toml` 固定，rustup 会自动切换到对应版本（不需要手动 `rustup override`）。

```bash
cargo run -p zcode -- --help
```

## Workspace 布局

`crates/` 下九个成员，版本、依赖与 lint 全部由根 `Cargo.toml` 集中声明（成员只写
`xxx.workspace = true`）。职责边界与导入约束以 `.omp/rules/zcode-architecture.md` 为准。

| 路径 | 包名 | 职责 |
| --- | --- | --- |
| `crates/coding-agent/` | `zcode` | 主 CLI，同时是所有 worker 子进程的 host 二进制 |
| `crates/agent/` | `zcode-agent` | Agent 运行时：工具调用循环与会话状态 |
| `crates/ai/` | `zcode-ai` | 多提供商 LLM 客户端（流式） |
| `crates/catalog/` | `zcode-catalog` | 模型目录、提供商描述符、模型身份识别 |
| `crates/tui/` | `zcode-tui` | 终端 UI，transcript 落在原生 scrollback |
| `crates/text/` | `zcode-text` | 性能关键的文本 / 图像 / grep |
| `crates/schema/` | `zcode-schema` | JSON Schema 惰性编译与校验 |
| `crates/stats/` | `zcode-stats` | 本地可观测性仪表盘 |
| `crates/utils/` | `zcode-utils` | 共享基础设施（日志、流、进程、临时文件） |

## 平台支持

| 目标 | CI 覆盖 |
| --- | --- |
| `x86_64-unknown-linux-gnu` / `x86_64-pc-windows-msvc` / `aarch64-apple-darwin` | clippy + 测试（runner 原生执行；`macos-latest` 是 arm64） |
| `x86_64-apple-darwin` / `aarch64-pc-windows-msvc` / `aarch64-unknown-linux-gnu` / `x86_64-unknown-linux-musl` | `cargo check`（交叉编译检查，不链接） |

## 开发

闸门命令（check / clippy / fmt / nextest / doctest / deny / machete）以 `AGENTS.md` 的命令表为
唯一事实来源；代码约束见 `.omp/rules/`。CI 逐条执行同样的闸门，外加 MSRV 与跨平台 check。

## 许可

双许可：[MIT](LICENSE-MIT) 或 [Apache-2.0](LICENSE-APACHE)，任选其一。
