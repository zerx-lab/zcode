//! `zcode`：主 CLI 应用，同时是所有 worker 子进程的 host 二进制。
//!
//! # 这个 crate 是装配层
//!
//! 它是唯一同时接线**客户端渲染栈**（`zcode-tui` / `ratatui` / `crossterm`）与
//! **运行时**（`zcode-agent` / `zcode-ai` / `zcode-catalog`）的 crate，因此是
//! `.omp/checks/dep-boundary.check.ts` 规则 2/3 的天然豁免对象（脚本注释 `:31-33`）。
//! 别的 crate 出现同样的双向依赖就是泄漏，这里不是。
//!
//! # 只有一条执行路径
//!
//! `plans/runtime-boundary/README.md:195` 已裁决：headless 与 TUI **共用同一条执行路径**。
//! daemon 在就连它；不在就用 [`zcode_utils::transport::stream_pair`] 把自己接上同一个
//! [`host::client::handle_client`]——不开真 socket，也不多一套执行路径。
//!
//! 同文档 `:179` 把 jcode 的 headless 绕行（`src/cli/commands.rs:2362-2415`）明确列为
//! **不抄**，理由是两套执行路径 = 两套 bug。任何"headless 直接建 `AgentRuntime` 跑"的
//! 改动都违反这条既定契约。
//!
//! # 分层
//!
//! | 模块 | 职责 |
//! | --- | --- |
//! | [`cli`] | clap 命令面、worker selector 分发、启动顺序 |
//! | [`config`] | 两层 TOML 配置的发现、合并与覆盖 |
//! | [`workspace`] | 路径解析与越界防护——全 crate 唯一入口 |
//! | [`prompt`] | system prompt 装配、`AGENTS.md` 发现、首条消息环境上下文 |
//! | [`model`] | 目录 + 凭据 → `Arc<dyn Provider>` |
//! | [`tools`] | 八个内置工具与注册表装配 |
//! | [`host`] | session 表、`handle_client`、wire 互转、daemon 生命周期 |
//! | [`render`] | headless 事件消费与终端输出 |
//! | [`app`] | TUI 应用层 |
//!
//! # 可见性约定
//!
//! 模块一律 `pub(crate)`，跨模块共享的项也一律 `pub(crate)`。本 crate 的 lib target
//! 只是为了让单元测试与 `main.rs` 共用同一份代码，**不是对外 API**——`pub` 只留给
//! [`run`] 一个。

pub(crate) mod app;
pub(crate) mod cli;
pub(crate) mod config;
pub(crate) mod host;
pub(crate) mod model;
pub(crate) mod prompt;
pub(crate) mod render;
pub(crate) mod tools;
pub(crate) mod workspace;

/// 端到端冒烟：唯一一处把整条装配链串起来跑的测试，放在 `src/` 而不是 `tests/`——
/// 集成测试目录够不到 `pub(crate)`，而本 crate 的内部项一个都不该为了测试放宽可见性
/// （`rule://rust-quality`）。
#[cfg(test)]
mod smoke;

/// 进程主流程：worker selector 分发 → 解析 argv → 分发到子命令。
///
/// 调用方必须**先**调用 `zcode_utils::env::declare_worker_host_entry()`——worker 重入
/// 路径依赖它，且它必须在 tokio runtime 之外完成（见 `main.rs` 的模块文档）。
///
/// 隐藏的 `__zcode_worker_*` selector 分发由 [`cli::worker::dispatch`] 在本函数内部、
/// `Cli::parse()` **之前**完成：clap 不认识这些参数，先让它看到就会以 usage error 退出。
///
/// # Errors
/// 子命令自身的失败原样上抛，由 `main` 渲染成 `Error: ...` 并以 1 退出。
///
/// 成功路径返回 [`std::process::ExitCode`]：headless 需要区分「turn 失败」(1) 与
/// 「被取消」(130)，这两种都不是 `Err`——它们是**正常完成的运行**，只是结果不同。
/// 用 `Err` 表达会让 `anyhow` 在 stderr 再打印一次已经报告过的内容。
pub async fn run() -> anyhow::Result<std::process::ExitCode> {
    cli::run().await
}
