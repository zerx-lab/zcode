//! 交互式终端判据与阻塞式行读取。
//!
//! 全模块只有一处 [`is_interactive`] 判据；别处一律调用它，不重新发明标准
//! （对照 jcode 的三处不一致判据：TUI 看 stdin+stdout、外部 auth 看
//! stdin+stderr、上色看 stdout——见 `crate::render` 模块文档）。

use std::io::{self, IsTerminal as _};

use crossterm::event::{Event as TermEvent, KeyCode};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};

/// 判断当前进程是否处于交互式终端。
///
/// 判据固定为 **stdin ∧ stderr 都是 TTY**：
/// - 只看 stdin 不够：stdin 是终端但 stderr 被重定向时，用户看不到审批/stdin
///   提示，读 stdin 仍然会造成"看似卡住"的观感；
/// - 只看 stdout 更不对：`zcode run ... > out.txt` 是本模块要支持的正常用法，
///   stdout 重定向不代表用户不在场。
pub(super) fn is_interactive() -> bool {
    io::stdin().is_terminal() && io::stderr().is_terminal()
}

/// 从终端读一行，跑在阻塞线程池上（`crossterm`/`std::io::Stdin::read_line`
/// 都是阻塞 API，不能直接在 async 上下文里调用）。
///
/// `mask` 为真时按密码处理：进 raw mode 逐键收集、不回显；回车提交，`Esc`
/// 清空已输入内容并提交空串（对应「取消这次输入」）。
///
/// 调用方必须先确认 [`is_interactive`]——本函数不重复检查；非交互环境下
/// 阻塞读取会让"绝不无限挂起"这条约束落空。
pub(super) async fn read_terminal_line(mask: bool) -> io::Result<String> {
    match tokio::task::spawn_blocking(move || read_terminal_line_blocking(mask)).await {
        Ok(result) => result,
        Err(join_error) => Err(io::Error::other(join_error)),
    }
}

fn read_terminal_line_blocking(mask: bool) -> io::Result<String> {
    if mask {
        read_masked_line()
    } else {
        let mut buf = String::new();
        io::stdin().read_line(&mut buf)?;
        Ok(buf.trim_end_matches(['\n', '\r']).to_owned())
    }
}

/// 密码式读取：raw mode 关掉本地回显，逐个按键事件收集。
fn read_masked_line() -> io::Result<String> {
    enable_raw_mode()?;
    let outcome = read_masked_line_inner();
    // 无论读取是否成功都要恢复 cooked mode，否则进程退出后用户的终端会卡在
    // 无回显状态——恢复失败没有更好的兜底，原样上抛。
    disable_raw_mode()?;
    outcome
}

fn read_masked_line_inner() -> io::Result<String> {
    let mut buf = String::new();
    loop {
        if let TermEvent::Key(key) = crossterm::event::read()? {
            match key.code {
                KeyCode::Enter => break,
                KeyCode::Esc => {
                    buf.clear();
                    break;
                }
                KeyCode::Char(c) => buf.push(c),
                KeyCode::Backspace => {
                    buf.pop();
                }
                _ => {}
            }
        }
    }
    Ok(buf)
}
