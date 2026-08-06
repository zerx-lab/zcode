//! 启动时判定一次、全程只读的输出能力（[`OutputCaps`]），以及 Windows VT 启用。
//!
//! 能力判定与 mode 施加严格分离（`plans/tui/README.md` 五条不变量第 5 条）：
//! [`apply_output_modes`] 只管往 console 里写 mode 位，不下任何能力结论；
//! [`OutputCaps::probe`] 才是唯一综合各路信号给出结论的地方，且只在启动时调一次、
//! 全程缓存，不在渲染路径反复探测终端。

use std::io::IsTerminal;

#[cfg(windows)]
use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
#[cfg(windows)]
use windows_sys::Win32::System::Console::{
    ENABLE_PROCESSED_OUTPUT, ENABLE_VIRTUAL_TERMINAL_PROCESSING, GetConsoleMode, GetStdHandle,
    STD_ERROR_HANDLE, STD_OUTPUT_HANDLE, SetConsoleMode,
};

/// 启动时判定一次、全程只读的输出能力。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputCaps {
    /// 整条 ANSI emit 是否可用（光标相对移动、SGR、行级重写）；
    /// `false` 表示完全不进 inline TUI，退化为纯 `stdout` 顺序打印。
    pub interactive_output: bool,
    /// ED3（`CSI 3J`，全量擦除 scrollback）是否可用。任何 multiplexer 下恒为 `false`。
    ///
    /// 只管 ED3 一件事：mux 底下 DECSTBM 照常使用，两者不能绑同一个开关
    /// （`oh-my-pi/packages/tui/src/tui.ts:12-13` 的原始教训——mux 里 ED3 不安全，
    /// 但滚动区仍然可用）。
    pub scrollback_purge: bool,
}

impl OutputCaps {
    /// 探测一次输出能力：先尝试施加 Windows VT 模式，再综合 TTY / ANSI / mux 三路信号
    /// 给出结论。调用方应当只在启动时调一次并缓存结果，全程当只读值使用。
    #[must_use]
    pub fn probe() -> Self {
        // 失败只记日志、不传播：即便 VT 没能力设，也要让后续判定按“未启用”走完，
        // 而不是直接终止启动——重定向到管道/文件时这是完全正常的路径。
        if let Err(err) = apply_output_modes() {
            tracing::warn!(%err, "设置 Windows console 输出模式失败");
        }

        let is_tty = std::io::stdout().is_terminal();

        #[cfg(windows)]
        let ansi_ok = vt_confirmed()
            // Git Bash / MSYS 自己解析 ANSI，WinAPI 开不了它们的 VT；但 Rust std 的
            // `is_terminal()` 在 Windows 上已经识别 MSYS/Cygwin pty
            // （std `sys/io/is_terminal/windows.rs`），所以这里只需再信一次 TERM，
            // 不会把 `myapp > out.txt` 误判成可交互（下面 `is_tty` 已经是 false）。
            || std::env::var("TERM").is_ok_and(|term| term != "dumb");
        #[cfg(not(windows))]
        let ansi_ok = true;

        let in_mux = detect_mux(
            std::env::var("TERM").ok().as_deref(),
            std::env::var_os("TMUX").is_some(),
            std::env::var_os("TMUX_PANE").is_some(),
            is_zellij(),
        );

        derive_caps(is_tty, ansi_ok, in_mux)
    }

    /// 两项能力全关：非 TTY、重定向、CI。
    #[must_use]
    pub const fn plain() -> Self {
        Self {
            interactive_output: false,
            scrollback_purge: false,
        }
    }
}

/// `probe()` 的纯逻辑核心，从 IO/env 判定中剥离出来便于表驱动测试。
///
/// `interactive_output = is_tty && ansi_ok`：用 AND 而不是 crossterm
/// `ansi_support.rs` 那种裸 OR，是刻意收紧——裸 OR 没有 tty 门，重定向到文件且
/// `TERM` 已设时会把它误判成支持 ANSI。
fn derive_caps(is_tty: bool, ansi_ok: bool, in_mux: bool) -> OutputCaps {
    let interactive_output = is_tty && ansi_ok;
    OutputCaps {
        interactive_output,
        // mux 只禁 ED3；不能因为 in_mux 就顺手也关了 interactive_output——DECSTBM
        // 在 tmux/screen 下依然可用，见 `plans/tui/platform.md` 能力矩阵一节。
        scrollback_purge: interactive_output && !in_mux,
    }
}

/// 判定是否处于终端 multiplexer 之下：`TMUX` / `TMUX_PANE` / Zellij 任一存在，
/// 或 `TERM` 以 `screen` 开头（`screen`、经典 `tmux` 的 `TERM` 值惯例）。
fn detect_mux(term: Option<&str>, tmux: bool, tmux_pane: bool, zellij: bool) -> bool {
    tmux || tmux_pane || zellij || term.is_some_and(|term| term.starts_with("screen"))
}

/// 只施加 Windows console 的输出模式（`ENABLE_PROCESSED_OUTPUT` |
/// `ENABLE_VIRTUAL_TERMINAL_PROCESSING`），不返回能力结论——`Ok(())` 不代表 VT
/// 已经生效：无效句柄和非 console（管道/文件重定向）场景都会静默跳过而不是报错，
/// 因为它们本就没有 console mode 可设，但写字节仍然合法（`zcode | tee log.txt`
/// 不该因为这个直接启动失败）。只有 `SetConsoleMode` 真正调用且失败时才 `Err`。
/// 非 Windows 平台恒返回 `Ok(())`。
///
/// **调用时机**：在任何裸转义字节发出之前调用一次；此后只要 spawn 过外部子进程
/// （它可能悄悄改了 console mode），必须重新调用一次。这是 Windows VT 启用独立于
/// crossterm、在任何 escape 之前生效的不变量（`plans/tui/README.md` 五条不变量
/// 第 4 条），能力判定见 [`OutputCaps::probe`] 里独立的回读确认。
#[cfg(windows)]
pub fn apply_output_modes() -> std::io::Result<()> {
    for which in [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        let handle = std_handle(which);
        let Some(mode) = console_mode(handle) else {
            // 无句柄，或非 console（管道/重定向）——没有 mode 可设，这不是错误。
            continue;
        };
        let want = ENABLE_PROCESSED_OUTPUT | ENABLE_VIRTUAL_TERMINAL_PROCESSING;
        if mode & want == want {
            continue; // 已经开着，幂等返回。
        }
        set_console_mode(handle, mode | want)?;
    }
    Ok(())
}

/// 只施加 Windows console 的输出模式，不返回能力结论。非 Windows 平台恒为 `Ok(())`——
/// 详细语义与调用时机见 Windows 版本的文档。
#[cfg(not(windows))]
pub fn apply_output_modes() -> std::io::Result<()> {
    Ok(())
}

/// 取标准句柄（`STD_OUTPUT_HANDLE` / `STD_ERROR_HANDLE`）。
#[cfg(windows)]
#[allow(unsafe_code)]
fn std_handle(which: u32) -> HANDLE {
    // SAFETY: `which` 只会是本模块内两个 WinAPI 文档化的标准句柄常量之一，
    // `GetStdHandle` 对它们的调用总是合法；失败时返回 `INVALID_HANDLE_VALUE`
    // 而不是悬垂指针，调用方（`console_mode`）会先判它再解引用。
    unsafe { GetStdHandle(which) }
}

/// 读取 `handle` 当前的 console mode；句柄无效或非 console（管道/重定向）
/// 时返回 `None`，不是错误。
#[cfg(windows)]
#[allow(unsafe_code)]
fn console_mode(handle: HANDLE) -> Option<u32> {
    if handle == INVALID_HANDLE_VALUE || handle.is_null() {
        return None;
    }
    let mut mode: u32 = 0;
    // SAFETY: 上面已经排除了无效句柄；`&mut mode` 是本函数栈帧上存活到调用结束的
    // 可写 `u32`，满足 `GetConsoleMode` 输出参数“指向有效可写内存”的要求。
    if unsafe { GetConsoleMode(handle, &raw mut mode) } == 0 {
        // 句柄有效但取不到 mode：典型场景是重定向到管道/文件，根本不是 console。
        return None;
    }
    Some(mode)
}

/// 写入 `handle` 的 console mode；失败时返回 `Err(last_os_error)`。
#[cfg(windows)]
#[allow(unsafe_code)]
fn set_console_mode(handle: HANDLE, mode: u32) -> std::io::Result<()> {
    // SAFETY: `handle` 已经过 `console_mode` 验证为有效 console 句柄；`mode` 是
    // 由已读到的旧值与目标位按位或组合出的合法 `CONSOLE_MODE`，`SetConsoleMode`
    // 不保留该值的指针，调用结束后不再被引用。
    if unsafe { SetConsoleMode(handle, mode) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// 回读 console mode 确认 VT 是否真正生效。[`apply_output_modes`] 返回 `Ok(())`
/// **不证明** VT 已启用——无效句柄和非 console 场景都会静默放行，必须专门回读判定。
#[cfg(windows)]
fn vt_confirmed() -> bool {
    console_mode(std_handle(STD_OUTPUT_HANDLE))
        .is_some_and(|mode| mode & ENABLE_VIRTUAL_TERMINAL_PROCESSING != 0)
}

/// `ZELLIJ` / `ZELLIJ_SESSION_NAME` / `ZELLIJ_VERSION` 任一存在。启动时读一次即可，
/// 但**模式选择**必须在注入点做，见 `insert_history::insert_history_mode`——
/// 同一会话内 wrap policy 会变，缓存成启动能力会走错路径。
#[must_use]
pub fn is_zellij() -> bool {
    std::env::var_os("ZELLIJ").is_some()
        || std::env::var_os("ZELLIJ_SESSION_NAME").is_some()
        || std::env::var_os("ZELLIJ_VERSION").is_some()
}

/// 逃生舱 `ZTUI_NO_SCROLL_REGION=1`：强制走非滚动区的历史注入路径。
///
/// 作用点在 per-batch 选择处（`insert_history::insert_history_mode`），**故意**不进
/// [`OutputCaps`]——它不是终端能力事实，而是运维手段：真机上发现某个终端不遵守
/// DECSTBM 时，不改代码就能绕过，同时把它记进黑名单。
#[must_use]
pub fn force_no_scroll_region() -> bool {
    std::env::var("ZTUI_NO_SCROLL_REGION").is_ok_and(|value| value == "1")
}

#[cfg(test)]
mod tests {
    use super::{OutputCaps, derive_caps, detect_mux};

    #[test]
    fn derive_caps_matrix() {
        let cases = [
            // (is_tty, ansi_ok, in_mux) -> (interactive_output, scrollback_purge)
            ((true, true, false), (true, true)),
            ((true, true, true), (true, false)),
            ((true, false, false), (false, false)),
            ((false, true, false), (false, false)),
            ((false, false, true), (false, false)),
        ];
        for ((is_tty, ansi_ok, in_mux), (interactive_output, scrollback_purge)) in cases {
            let got = derive_caps(is_tty, ansi_ok, in_mux);
            assert_eq!(
                got,
                OutputCaps {
                    interactive_output,
                    scrollback_purge
                },
                "derive_caps({is_tty}, {ansi_ok}, {in_mux})"
            );
        }
    }

    #[test]
    fn plain_is_all_off() {
        assert_eq!(
            OutputCaps::plain(),
            OutputCaps {
                interactive_output: false,
                scrollback_purge: false
            }
        );
    }

    #[test]
    fn detect_mux_matrix() {
        let cases = [
            // (term, tmux, tmux_pane, zellij) -> in_mux
            ((None, false, false, false), false),
            ((Some("xterm-256color"), false, false, false), false),
            ((Some("screen"), false, false, false), true),
            ((Some("screen-256color"), false, false, false), true),
            ((None, true, false, false), true),
            ((None, false, true, false), true),
            ((None, false, false, true), true),
            // TERM 里含 "tmux" 但不以 "screen" 开头、且没设 TMUX：不算 mux，
            // 必须真的看到 TMUX/TMUX_PANE/ZELLIJ 这类 env 事实。
            ((Some("tmux-256color"), false, false, false), false),
        ];
        for ((term, tmux, tmux_pane, zellij), expected) in cases {
            assert_eq!(
                detect_mux(term, tmux, tmux_pane, zellij),
                expected,
                "detect_mux({term:?}, {tmux}, {tmux_pane}, {zellij})"
            );
        }
    }
}
