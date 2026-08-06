//! ZCode CLI 入口，同时是所有 worker 子进程的 host 二进制。
//!
//! 启动顺序是契约的一部分（见 `rule://zcode-architecture`）：先把自己声明为 worker host，
//! 再分发隐藏的 worker selector，最后才进入常规命令解析。**selector 分发表尚未落盘**——
//! 目前没有任何 worker，第一个 worker 落地时必须同时补上 selector 分支、进程内 fallback
//! 与同级 smoke 测试，并插在 `Cli::parse()` 之前。

use anyhow::{Context, Result};
use clap::{CommandFactory, Parser};

/// ZCode —— 终端里的 coding agent。
#[derive(Debug, Parser)]
#[command(name = "zcode", version, about, long_about = None)]
struct Cli {}

fn main() -> Result<()> {
    // 必须早于任何参数解析：worker selector 分发依赖它解析出的重入路径。
    zcode_utils::env::declare_worker_host_entry().context("声明 worker host 入口失败")?;

    let _cli = Cli::parse();

    // 子命令尚未落盘：无参数运行时打印帮助。CLI 执行后即退出，不与 TUI/协议共享 stdout，
    // 因此这里允许直接写 stdout（见 `rule://rust-quality` 的日志边界）。
    Cli::command().print_help().context("输出帮助信息失败")?;
    println!();
    Ok(())
}
