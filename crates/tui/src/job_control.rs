//! Unix 专用：`Ctrl-Z` 挂起（`SIGTSTP`）与 `SIGCONT` 恢复后的 inline viewport 重锚定。
//!
//! macOS / Linux 上 `Ctrl-Z` 挂起是用户预期行为，但 inline TUI 让它变复杂：挂起再恢复
//! 之间，shell 会往终端写 job-control 状态（`[1]+ Stopped ...`）和被恢复的命令行，
//! 绘制时缓存的光标行因此失效，viewport 必须重新锚定，否则下一帧会画到错误的行上。
//!
//! `SIGTSTP` 会直接停住进程，`SIGCONT` 恢复后代码从 `libc::kill` 调用的下一行继续
//! 同步执行，因此不需要注册 signal handler（也不需要 `signal-hook`）。
//!
//! # 抄录来源
//!
//! `codex-rs/tui/src/tui/job_control.rs`（挂起/恢复顺序与三个非显然点）与
//! `codex-rs/tui/src/tui.rs:339-348,376-382`（raw mode 重同步、stdin 冲刷）。
//!
//! # 刻意省略：alternate screen
//!
//! codex 的 `job_control.rs:65-72` 在挂起时会退出 alt-scroll/alt screen 并记 `RestoreAlt`，
//! 恢复时再重新进入。本仓的 transcript 始终画在 normal screen（不进 alternate screen，
//! 换取原生 scrollback/选择复制，见 crate 根文档），因此没有"挂起时正处于 alt screen"
//! 这个状态需要处理，这里不搬那段记账逻辑。

use std::io::{self, Write};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::Backend;
use ratatui::layout::{Position, Rect};

use crate::terminal::Terminal;
use crate::terminal_probe;

/// `Ctrl-Z`：事件流拦截层用它判断是否应该触发 [`suspend`]，而不是把按键继续往下发。
pub const SUSPEND_KEY: KeyEvent = KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL);

/// 挂起当前进程组并在 `SIGCONT` 恢复后重锚定 inline viewport。
///
/// **调用方必须已经暂停 crossterm 事件流**：第 6 步会调用
/// [`terminal_probe::cursor_position`] 重新探测光标，而那次探测会直接从终端输入流里
/// 吃掉字节；如果事件流还在跑，探针和后台输入 reader 会抢同一份 stdin，回复字节被吞
/// 或者被误当成按键喂给上层（`plans/tui/platform.md` 第 2 节）。
///
/// 执行顺序（每一步都有对应的失败模式，顺序不能打乱）：
/// 1. 把光标移到 inline viewport 的锚点行并显示出来，让 shell 的 job-control 输出接在
///    正确位置，而不是叠在 viewport 内容上面。
/// 2. 退出 raw mode，回到 shell 期望的 canonical 终端状态。
/// 3. 发 `SIGTSTP` 给整个进程组，真正让进程停下。
/// 4. 恢复后强制让 crossterm 的 raw-mode 缓存状态与内核 termios 重新同步。
/// 5. 冲掉挂起期间在终端侧排队的 stdin 字节。
/// 6. 重新探测光标位置并重锚定 viewport（恢复后光标行不可信）。
/// 7. 隐藏光标，回到 TUI 的常态。
pub fn suspend<B>(terminal: &mut Terminal<B>) -> io::Result<()>
where
    B: Backend<Error = io::Error> + Write,
{
    // 1. `last_known_cursor_pos` 是绘制时缓存的"光标应该在哪一行"，扮演的角色等价于
    //    codex `SuspendContext::suspend_cursor_y`。挂起前把光标显式移过去、显示出来，
    //    这样 shell 打印的 job-control 状态行会紧跟在 TUI 最后一次绘制的内容之后
    //    （codex job_control.rs:72-74）；不做这一步，光标可能还停在上一次绘制时的
    //    某个内部位置，挂起提示会叠在 viewport 内容中间。
    let anchor = terminal.last_known_cursor_pos();
    terminal.set_cursor_position(Position { x: 0, y: anchor.y })?;
    terminal.show_cursor()?;

    // 2. 退出 raw mode：SIGTSTP 之后控制权交还给 shell 的 job 控制台，raw mode 下的
    //    字节级无回显输入在那期间没有意义。
    crossterm::terminal::disable_raw_mode()?;

    // 3. pid 传 0，即发给调用者所在的整个进程组，不是只发给当前进程：如果本进程还
    //    fork/spawn 了子进程（比如正在跑的 shell 命令），只 kill(getpid()) 会漏掉它们，
    //    子进程继续占用终端、干扰挂起（codex job_control.rs:200-207 的 `libc::kill(0, ..)`)。
    //    不需要注册 signal handler：SIGTSTP 直接停住进程，SIGCONT 恢复后从这一行的
    //    下一行继续同步执行。
    // SAFETY: `kill(2)` 只发送信号，不访问/修改任何 Rust 侧内存；pid=0 是 POSIX 文档化的
    // "发给调用者所在的进程组" 语义，返回值只用于错误检查。
    #[allow(unsafe_code)]
    let killed = unsafe { libc::kill(0, libc::SIGTSTP) };
    if killed != 0 {
        return Err(io::Error::last_os_error());
    }

    // 4. 见 `reapply_raw_mode_after_resume` 的文档：shell 可能在 SIGCONT 之后才把这个
    //    job 保存的 termios 还回去，如果这和我们自己的 `enable_raw_mode()` 竞速，
    //    crossterm 会继续以为 raw mode 已经开着，而终端其实回到了 canonical、回显模式。
    reapply_raw_mode_after_resume()?;

    // 5. 冲掉挂起期间在终端侧排队的 stdin 字节（比如用户在 job 被 Stopped 时误按的键），
    //    避免它们在恢复后被当成 TUI 输入重放（codex tui.rs:368-382）。冲刷失败不是致命
    //    错误——最坏情况只是有几个陈旧字节没被清掉，不值得让整个 resume 失败。
    // SAFETY: `tcflush` 只操作内核维护的 tty 队列，`STDIN_FILENO` 是进程标准输入的
    // 固定描述符，不涉及内存安全。
    #[allow(unsafe_code)]
    let flushed = unsafe { libc::tcflush(libc::STDIN_FILENO, libc::TCIFLUSH) };
    if flushed != 0 {
        tracing::warn!(error = %io::Error::last_os_error(), "恢复挂起后冲刷 stdin 队列失败");
    }

    // 6. 恢复后光标行不可信：shell 在 `fg` 之后写了 job-control 状态和被恢复的命令行，
    //    第 1 步缓存的位置已经失效，必须用 CSI 6n 重新探测（codex job_control.rs:78-90）。
    //    这一步之所以能安全消费交错的 focus report 和光标响应而不与后台输入 reader
    //    竞争，前提正是本函数文档开头写的"调用方必须已经暂停事件流"。
    match terminal_probe::cursor_position(terminal_probe::DEFAULT_TIMEOUT) {
        Ok(Some(pos)) => {
            terminal.set_last_known_cursor_pos(pos);
            // 用新查询的屏幕尺寸而不是挂起前缓存的值：挂起期间用户完全可能调整过
            // 终端窗口大小，沿用旧尺寸会算出一个越界的 viewport。
            let screen_size = terminal.screen_size()?;
            let area = reanchor_viewport(terminal.viewport_area(), pos, screen_size.height);
            terminal.set_viewport_area(area);
            terminal.invalidate_viewport();
        }
        Ok(None) => {
            tracing::debug!("恢复后未能探测到终端光标位置，沿用挂起前的 viewport");
        }
        Err(err) => {
            tracing::debug!(error = %err, "恢复后探测终端光标位置失败，沿用挂起前的 viewport");
        }
    }

    // 7. 探测/重锚定都是同步完成的终端操作，走到这里已经回到 TUI 的常态：光标默认
    //    隐藏，由渲染路径（`Frame::set_cursor_position`）按需重新显示。
    terminal.hide_cursor()
}

/// 让 crossterm 缓存的 raw-mode 状态与内核 termios 重新对齐。
///
/// shell 可能在进程收到 `SIGCONT` 之后才把这个 job 保存的 termios 还回去；如果这与我们
/// 自己调用 `enable_raw_mode()` 竞速，crossterm 会继续以为 raw mode 已经启用，但终端
/// 其实已经被 shell 交接回 canonical、回显模式。这里先 `disable_raw_mode()` 清空
/// crossterm 内部缓存的状态，再 `enable_raw_mode()` 重新设置一次，让内核状态成为
/// shell 完成交接之后唯一权威的来源（结论照抄 codex `tui.rs:336-343` 的注释）。
fn reapply_raw_mode_after_resume() -> io::Result<()> {
    crossterm::terminal::disable_raw_mode()?;
    crossterm::terminal::enable_raw_mode()
}

/// 用重新探测到的光标位置重算 inline viewport 的锚点行；只改 `y`，`x`/宽高不变。
///
/// `area.height.saturating_sub(1)` 是"锚点行到 viewport 底部"之间的行数：把新探测到的
/// 光标行减去这个值，就是 viewport 应该从哪一行开始画。`saturating_sub` 保证结果不会
/// 在 `u16` 上因为减法下溢绕回一个巨大的正数（即"不能出现负 y"）；再用 `min` 夹到
/// `[0, screen_height - area.height]` 上界，防止 viewport 顶部超出屏幕底部——探测到的
/// 光标行在终端尺寸发生过变化、或者内容异常时可能给出让人意外的行号。
fn reanchor_viewport(area: Rect, cursor: Position, screen_height: u16) -> Rect {
    let offset_from_bottom = area.height.saturating_sub(1);
    let new_top = cursor.y.saturating_sub(offset_from_bottom);
    let max_top = screen_height.saturating_sub(area.height);
    Rect {
        y: new_top.min(max_top),
        ..area
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reanchors_to_cursor_row_when_cursor_is_mid_screen() {
        let area = Rect::new(0, 10, 80, 5);
        let cursor = Position { x: 0, y: 15 };
        let result = reanchor_viewport(area, cursor, 24);
        assert_eq!(result.y, 11); // 15 - (5 - 1)
        assert_eq!((result.x, result.width, result.height), (0, 80, 5));
    }

    #[test]
    fn clamps_to_zero_when_cursor_is_near_top() {
        let area = Rect::new(0, 10, 80, 5);
        let cursor = Position { x: 0, y: 2 };
        let result = reanchor_viewport(area, cursor, 24);
        assert_eq!(result.y, 0);
    }

    #[test]
    fn clamps_to_screen_bottom_when_cursor_overflows() {
        let area = Rect::new(0, 10, 80, 5);
        let cursor = Position { x: 0, y: 30 };
        let result = reanchor_viewport(area, cursor, 24);
        assert_eq!(result.y, 19); // 24 - 5，不能让 viewport 底部越过屏幕
    }
}
