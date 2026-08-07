//! ZCode CLI 入口，同时是所有 worker 子进程的 host 二进制。
//!
//! # 启动顺序是契约的一部分
//!
//! 见 `rule://zcode-architecture`。四步，顺序不可交换：
//!
//! 1. **Windows 专线程大栈**（仅 `cfg(windows)`）——必须最早，后面每一步都在这个栈上跑。
//! 2. `declare_worker_host_entry()`——worker selector 分发依赖它解析出的重入路径。
//! 3. **worker selector 分发**——必须早于 `Cli::parse()`：`__zcode_worker_*` 不是 clap
//!    认识的参数，让 clap 先看到就会以 usage error 退出。
//! 4. 常规命令解析与分发。
//!
//! # 为什么 `main` 不是 `#[tokio::main]`
//!
//! 第 1 步要求进程主线程只做一件事：把真正的工作 spawn 到一个 8 MiB 栈的线程上。
//! `#[tokio::main]` 会把 runtime 建在主线程上，那个栈就用不上了。第 2 步同理必须在
//! runtime 之外完成——它读 `current_exe()` 并写进程级 `OnceLock`，跟异步无关，
//! 放进 runtime 只会让"它必须在一切之前"这条约束变得不可见。

use std::process::ExitCode;

use anyhow::{Context as _, Result};

/// Windows 主线程的替代栈大小。
///
/// 抄源 jcode `src/main.rs:83-85`，连同它成立的前提一起：Windows PE 的主线程栈保留量
/// 远小于 Unix（默认 1 MiB），CLI 与 provider 装配路径在 tokio 接管前就能吃穿它，
/// 得到**不可恢复**的 `STATUS_STACK_OVERFLOW`（栈溢出在 Windows 上不是 panic，
/// 捕获不到，进程直接消失）。
///
/// 用专线程而不是 `/STACK` 链接器参数：后者是 crate 级的，会波及本 workspace 里
/// 每一个二进制目标，包括只跑几十行的辅助二进制。
#[cfg(windows)]
const WINDOWS_MAIN_STACK_SIZE: usize = 8 * 1024 * 1024;

#[cfg(windows)]
fn main() -> Result<ExitCode> {
    let worker = std::thread::Builder::new()
        .name("zcode-main".to_owned())
        .stack_size(WINDOWS_MAIN_STACK_SIZE)
        .spawn(bootstrap)
        .context("启动 zcode-main 线程失败")?;

    match worker.join() {
        Ok(result) => result,
        // 子线程 panic 时原样重放，保留原始 backtrace；这里吞掉会让崩溃变成一个
        // 没有栈的匿名错误。`resume_unwind` 不是 `panic!`，不受 `clippy::panic` 约束。
        Err(payload) => std::panic::resume_unwind(payload),
    }
}

#[cfg(not(windows))]
fn main() -> Result<ExitCode> {
    bootstrap()
}

/// 建 runtime 之前必须完成的同步准备，然后进入异步主流程。
fn bootstrap() -> Result<ExitCode> {
    // 必须早于任何参数解析：worker selector 分发依赖它解析出的重入路径。
    zcode_utils::env::declare_worker_host_entry().context("声明 worker host 入口失败")?;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("构建 tokio runtime 失败")?;

    runtime.block_on(zcode::run())
}
