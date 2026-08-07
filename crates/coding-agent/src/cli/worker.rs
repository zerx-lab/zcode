//! 隐藏的 worker selector 分发。
//!
//! # 为什么必须早于 `Cli::parse()`
//!
//! `__zcode_worker_*` 不是 clap 认识的参数。让 clap 先看到它，得到的是一句
//! "unexpected argument" 加 usage 文本，而不是启动 worker——而 worker 是被父进程
//! 用管道拉起来的，那段 usage 文本会直接污染父子之间的协议流。
//!
//! # 为什么 worker 重入 CLI 入口而不是各自一个 `[[bin]]`
//!
//! 见 `rule://zcode-architecture` 的「Worker 子进程契约」：早期方案是每个 worker 声明
//! 独立 `[[bin]]` 目标，导致 `cargo install` 与打包脚本必须始终同步两套目标列表，
//! 漏掉一个就在安装后**静默失败**。现在统一走 `zcode_utils::env::worker_host_entry()`
//! 返回的同一个二进制。
//!
//! # 目前没有任何 worker
//!
//! 本模块因此只做一件有实际作用的事：**识别 selector 前缀并给出可诊断的失败**。
//! 这不是占位——它把「新旧二进制混用时子进程收到 clap usage 文本」这个静默故障
//! 变成一条指名道姓的错误。
//!
//! 第一个 worker 落地时：`dispatch` 变成 `async` 并返回能表达"命中并已跑完 + 退出码"的
//! 类型（现在没有任何 selector 能命中，`async` 与永不构造的变体都只是噪音），同时补
//! 进程内 fallback 与同级 smoke 测试——三者缺一不可。

use anyhow::{Result, bail};

/// worker selector 的公共前缀。
const SELECTOR_PREFIX: &str = "__zcode_worker_";

/// 若 `argv[1]` 是 worker selector 就执行它；否则原样返回，调用方继续走常规命令解析。
///
/// # Errors
/// selector 形如 worker 但没有对应实现时返回错误——这通常意味着父进程与子进程
/// 是两个版本的二进制，静默忽略只会让父进程等一个永远不会来的握手。
pub(crate) fn dispatch(argv: &[String]) -> Result<()> {
    let Some(selector) = argv.get(1) else {
        return Ok(());
    };
    if !selector.starts_with(SELECTOR_PREFIX) {
        return Ok(());
    }

    bail!(
        "未知的 worker selector `{selector}`。\
         这通常说明拉起本进程的父进程与本二进制不是同一个版本；\
         请确认 PATH 上没有残留的旧 zcode。"
    )
}

#[cfg(test)]
mod tests {
    use super::dispatch;

    fn argv(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn plain_invocation_is_not_a_worker() {
        dispatch(&argv(&["zcode", "run", "hello"])).expect("普通调用必须原样放过");
    }

    #[test]
    fn bare_invocation_is_not_a_worker() {
        dispatch(&argv(&["zcode"])).expect("裸调用必须原样放过");
    }

    /// 认不出的 selector 必须报错而不是当作普通参数放过去：放过去的后果是
    /// clap 打一段 usage 到 stdout，而父进程正在那条管道上等协议帧。
    #[test]
    fn unknown_selector_is_a_diagnosable_error() {
        let error =
            dispatch(&argv(&["zcode", "__zcode_worker_nope"])).expect_err("未知 selector 必须失败");
        let text = error.to_string();
        assert!(
            text.contains("__zcode_worker_nope"),
            "错误要点名 selector：{text}"
        );
        assert!(text.contains("版本"), "错误要指出最可能的原因：{text}");
    }
}
