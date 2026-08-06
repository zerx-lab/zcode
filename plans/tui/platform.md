# 三平台支持

目标:macOS / Linux / Windows。开发环境:Windows。

后端全平台统一为 crossterm(依据见 `dependencies.md`)。平台相关代码集中在**四处**,合计约 580 行。

| 分叉 | 位置 | 平台 | 行数 |
|---|---|---|---|
| VT 启用 | `caps.rs` | Windows only | ~120 |
| job control(Ctrl-Z suspend) | `job_control.rs` | Unix only | ~210 |
| 光标位置探测(`CSI 6n`) | `terminal_probe.rs` | Unix only | ~250 |
| 历史注入模式 | `insert_history.rs` 内的 per-batch 决策 | 跨平台(按 env 事实) | ~10 |

第三处是第二处的硬依赖:`job_control.rs:83` 调 `terminal_probe::cursor_position` 重锚定 viewport,缺它 Ctrl-Z 无法落地。

第四处**不是**平台分叉,也**不是**启动时能力,是每次注入时的运行时决策。列在此处因为它常被误当成平台能力。

---

## 1. Windows:VT 启用

### 问题

emit 层通过 `unstable-backend-writer` 直写裸字节。conhost 默认不解析 ANSI,必须先设 `ENABLE_VIRTUAL_TERMINAL_PROCESSING`。

crossterm 里唯一启用 VT 的地方是 `ansi_support.rs:16` 的私有函数 `enable_vt_processing()`,其唯一调用点是 `supports_ansi():39`,且被 `Once` 守卫——副作用只发生一次。

`command.rs:34-36` 的默认 `is_ansi_code_supported()` 会调 `supports_ansi()`;`:122-123` / `:289-291` 只在返回 false 时才走 `execute_winapi()`。

**因此:自定义 Command 若 override `is_ansi_code_supported() -> true`,会短路掉 crossterm 唯一的 VT 启用副作用。** 在未启用 VT 的 conhost 上,DECSTBM / ED3 会被当字面字节输出。

而"反正别的标准 crossterm command 会顺带启用它"是不可靠的隐式依赖:顺序不确定,且本架构可能一个标准 command 都不用。

### 解法:独立启用,与 crossterm 解耦

抄源 `codex-rs/tui/src/tui.rs:1137-1177`,在 `set_modes()` 第一行调用(`tui.rs:217-218`)。

```rust
/// 只施加 console mode，不返回能力结论。
#[cfg(windows)]
fn apply_output_modes() -> io::Result<()> {
    for h in [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        let handle = unsafe { GetStdHandle(h) };
        if handle == INVALID_HANDLE_VALUE || handle == 0 {
            continue;                                   // 无句柄
        }
        let mut mode = 0;
        if unsafe { GetConsoleMode(handle, &mut mode) } == 0 {
            continue;                                   // 非 console（管道/重定向）
        }
        let want = ENABLE_PROCESSED_OUTPUT | ENABLE_VIRTUAL_TERMINAL_PROCESSING;
        if mode & want == want {
            continue;                                   // 已开，幂等
        }
        if unsafe { SetConsoleMode(handle, mode | want) } == 0 {
            return Err(io::Error::last_os_error());      // 只有这里才 Err
        }
    }
    Ok(())
}

#[cfg(not(windows))]
fn apply_output_modes() -> io::Result<()> { Ok(()) }
```

要点:

- **`GetConsoleMode` 失败必须 `continue` 而非 `Err`**(`codex tui.rs:1154-1156`)。句柄被重定向成 pipe/file 时没有 console mode 可设,但写字节仍然合法。若返回 `Err`,`myapp | tee log.txt` 直接启动失败。
- **STDERR 也要设**(`:1173-1174`)。
- `ENABLE_PROCESSED_OUTPUT` 与 `ENABLE_VIRTUAL_TERMINAL_PROCESSING` 必须一起设(`:1158`),只设后者会让 `\n` / `\r` 处理不一致。
- **不止启动时调一次。** codex 在每个发裸 escape 的入口都重新确保:`tui.rs:298`(restore)、`:942`、`:1003/1022/1044`(图片)、`:1073`。理由是外部子进程可能改了 console mode。
- restore 路径要收集错误但**继续执行**(`:298` 的 `let mut first_error = ...err();`)——退出时要尽力还原终端。

### 能力判定必须独立

`apply_output_modes() -> Ok(())` **不证明 VT 已启用**(无效句柄和非 console 都返回 Ok)。判定要回读:

```rust
#[cfg(windows)]
fn vt_confirmed() -> bool {
    let handle = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
    if handle == INVALID_HANDLE_VALUE || handle == 0 { return false; }
    let mut mode = 0;
    unsafe { GetConsoleMode(handle, &mut mode) } != 0
        && mode & ENABLE_VIRTUAL_TERMINAL_PROCESSING != 0
}

fn probe() -> OutputCaps {
    if let Err(err) = apply_output_modes() {
        tracing::warn!(%err, "failed to set console output mode");
    }

    let is_tty = std::io::stdout().is_terminal();

    #[cfg(windows)]
    let ansi_ok = vt_confirmed()
        // Git Bash/MSYS 自己解析 ANSI，WinAPI 开不了 VT
        || std::env::var("TERM").is_ok_and(|t| t != "dumb");
    #[cfg(not(windows))]
    let ansi_ok = true;

    let interactive_output = is_tty && ansi_ok;
    let in_mux = std::env::var_os("TMUX").is_some()
        || std::env::var_os("TMUX_PANE").is_some()
        || std::env::var_os("ZELLIJ").is_some()
        || std::env::var("TERM").is_ok_and(|t| t.starts_with("screen"));

    OutputCaps {
        interactive_output,
        scrollback_purge: interactive_output && !in_mux,
    }
}
```

`is_tty && ansi_ok` 用 AND 是正确的,不会抵消 Git Bash 的 `TERM` fallback:Rust std 的 `is_terminal()` 在 Windows 上已经识别 MSYS/Cygwin pty。

`std/src/sys/io/is_terminal/windows.rs:21-22, 67-69`:

```rust
// handle_is_console 失败后：
msys_tty_on(handle)
// ...
let is_msys = name.starts_with("msys-") || name.starts_with("cygwin-");
let is_pty  = name.contains("-pty");
is_msys && is_pty
```

所以 Git Bash 下 `is_tty == true`、`vt_confirmed() == false`、`TERM` fallback 生效,结果 true。而 `myapp > out.txt` 被正确判为 false。

这比 crossterm 的裸 OR **更严格**:`ansi_support.rs:39-40` 没有 tty 门,重定向到文件且 `TERM` 已设时它会误判为支持 ANSI。

### 自定义 Command 的 override

VT 由上面独立保证之后,自定义 Command 才可以 override(抄 `codex insert_history.rs:345-349`):

```rust
impl Command for SetScrollRegion {
    fn write_ansi(&self, f: &mut impl fmt::Write) -> fmt::Result {
        write!(f, "\x1b[{};{}r", self.0.start, self.0.end)
    }
    #[cfg(windows)]
    fn execute_winapi(&self) -> std::io::Result<()> {
        Err(std::io::Error::other("DECSTBM has no WinAPI equivalent"))
    }
    #[cfg(windows)]
    fn is_ansi_code_supported(&self) -> bool { true }
}
```

`execute_winapi` 返回 `Err` 而非 `panic!`(codex 用的是 panic,helix 在 `helix-tui/src/backend/crossterm.rs:461-464` 用 `Err`,后者更好)。

这个 override 与 VT 启用解决的是**不同问题**:前者告诉 crossterm "别走 WinAPI 分支",后者保证终端会解析 ANSI。两者都需要。

---

## 2. Unix:job control(Ctrl-Z suspend)

macOS / Linux 上 `Ctrl-Z` 挂起是用户预期。inline TUI 使这件事变复杂:恢复后光标行已被 shell 的 job-control 输出改变,viewport 必须重锚定。

抄源 `codex-rs/tui/src/tui/job_control.rs`(211 行)。

### 链路

| 跳 | 位置 |
|---|---|
| `SUSPEND_KEY = ctrl(Char('z'))` | `job_control.rs:25` |
| 事件流截获,暂停 broker | `tui/event_stream.rs:256-258` |
| `SuspendContext::suspend()` | `job_control.rs:64-99` |
| 若在 alt screen:退出 alt-scroll + alt screen,记 `RestoreAlt` | 同上 `:65-72` |
| 光标移到缓存的 inline 行并显示 | 同上 `:73-74` |
| `suspend_process()`:`restore()` + `stderr::pause()` + `libc::kill(0, SIGTSTP)` | `:200-207` |
| 恢复后 `stderr::resume()` | 同上 |
| `reapply_raw_mode_after_resume()` | `job_control.rs:76` -> `tui.rs:344-348` |
| `terminal_probe::cursor_position(DEFAULT_TIMEOUT)` 重探测光标 | `job_control.rs:83` -> `terminal_probe.rs:240-244` |
| 用探测结果重锚定 inline viewport | `job_control.rs:84` |

### 三个非显然点

**a. `libc::kill(0, SIGTSTP)` 的 pid 是 0,即整个进程组。** 单发给自己会漏掉管道里的兄弟进程。

**b. 恢复后必须 `disable_raw_mode()` 再 `enable_raw_mode()`**(`tui.rs:344-348`)。注释解释了竞态:

> A shell may restore the job's saved termios after the process receives `SIGCONT`. When that races with `set_modes`, crossterm still believes raw mode is enabled even though the terminal has returned to canonical, echoing mode. Clearing crossterm's saved state before enabling raw mode again makes the kernel state authoritative once the shell has completed its handoff.

即:crossterm 缓存了"raw mode 已启用"的状态,但 shell 在 `SIGCONT` 后把终端还成了 canonical。必须先 disable 清掉 crossterm 的缓存,再 enable。

**c. 恢复后光标行不可信,必须重探测**(`job_control.rs:78-90`)。shell 在 `fg` 后写了 job-control 状态和恢复的命令行,缓存的 `suspend_cursor_y` 已失效。codex 用 `terminal_probe::cursor_position(DEFAULT_TIMEOUT)`(即 `CSI 6n` DSR 查询)重新拿。

注释指出这是安全的:事件流在 `suspend()` 返回前保持暂停,所以 probe 可以安全消费交错的 focus 报告和光标位置响应,不会与后台输入 reader 竞争。

另有 `tui.rs:376-382` 的 `libc::tcflush(STDIN_FILENO, TCIFLUSH)` 冲掉 stdin 队列。

### `terminal_probe.rs` 的独立契约

`cursor_position` 不是"顺手调一下 crossterm 的 `position()`"。crossterm 的公开实现会与 event stream 争抢 stdin,所以 codex 自己写了一个 966 行的 probe 模块。我们只需要其中的 `cursor_position` 路径(`terminal_probe.rs:62-244`),不要 startup probe 与 OSC 10/11 颜色查询。

必须一起抄过来的约束:

| 约束 | 位置 | 后果 |
|---|---|---|
| **只能在 event stream 缺席或暂停时运行** | `:10`、`:269-271`("short, exclusive probe windows") | 否则 probe 与输入 reader 争抢 stdin,回复字节被吞或泄漏成按键 |
| 超时预算 `DEFAULT_TIMEOUT = 100ms` | `:18` | 不支持 DSR 的终端只付一次有界等待 |
| 超时/无响应返回 `Ok(None)`,不是 `Err` | `:284-289` | 不支持 DSR 不是错误;调用方保留旧值即可 |
| dup stdin/stdout 作为首选路径,失败回退 `/dev/tty` | `:82-83, 95-115` | 回复被投递到 crossterm 读的同一个输入流,必须用复制的 fd 以免清理时动到进程 stdio |
| **只**把 reader 设 `O_NONBLOCK`,Drop 时恢复原 file status flags | `:66-67, 128` | 否则退出后进程 stdio 处于非阻塞态,后续读取行为异常 |
| 用 `libc::poll` 等待可读,而非阻塞读 | `:171-189` | 有界等待的唯一正确实现 |
| 复用 startup parser 找响应,而不是假定第一段字节就是答案 | `:236-239` | resume 时终端可能先发 focus report 再发光标响应;不复用 parser 会把 focus 序列泄漏成按键 |

最后一条容易漏。`:237-238` 的原话:"Resume can emit a focus report immediately before the cursor-position response."

Windows 上不需要这个模块——没有 Ctrl-Z,inline viewport 初始化时用 crossterm 的 `position()` 即可(那时 event stream 还没启动,不存在争抢)。

### 不需要 signal handler

`SIGTSTP` 会停住进程,`SIGCONT` 恢复后代码从 `kill` 的下一行继续执行。所以整个流程是同步的,不需要注册 signal handler。

codex 因此**不用 `signal-hook`**,只用 `libc`(`codex-rs/tui/Cargo.toml:127-128`):

```toml
[target.'cfg(unix)'.dependencies]
libc = { workspace = true }
```

Windows 上无对应功能,`job_control.rs` 整个模块 `#[cfg(unix)]`。

---

## 3. per-batch:历史注入模式

**这不是启动时能力。** `codex-rs/tui/src/tui.rs:908-913` 每个 history batch 独立决策:

```rust
let mode = if is_zellij && batch.wrap_policy == HistoryLineWrapPolicy::Terminal {
    InsertHistoryMode::ZellijRaw
} else {
    InsertHistoryMode::Standard          // DECSTBM
};
```

依据 `insert_history.rs:51-53`:

> Zellij does not constrain soft-wrapped continuation rows to Codex's scroll region, so its raw path appends history through the terminal and reserves blank rows for the next viewport draw.

即:只有"Zellij 环境" **且** "本次 batch 用 terminal-wrap 策略"时才降级。同一会话内 wrap policy 可以变,所以把它做成启动时的 `scroll_regions: bool` 会走错路径。

`is_zellij` 是 env 事实(`ZELLIJ` / `ZELLIJ_SESSION_NAME` / `ZELLIJ_VERSION`,见 `pets/image_protocol.rs:117-119`),启动时读一次即可;但**模式选择**必须在注入点做。

`ZellijRaw` 路径(`insert_history.rs:164-193`)不使用滚动区,但**仍然绝对定位**(`:169` / `:183`)。它的安全性来自另一组条件,抄的时候一个都不能少:

| 条件 | 位置 |
|---|---|
| 先 `clear_after_position(area.as_position())` 清 viewport | `:167` |
| 在**同一 draw pass** 内完整替换 viewport | `:165-166` 的注释要求 |
| 打印历史行(首行不加前导 `\r\n`) | `:170-175` |
| 预留 `area.height` 个空行 + `Clear(UntilNewLine)`,让历史紧贴 composer | `:180-182` |
| 绝对 `MoveTo` 恢复光标(cursor-position-neutral) | `:183` |
| 重算 `viewport_top` 并 `set_viewport_area` | `:185-192` |

`:165-166` 的原话解释了为什么必须先清:

> The existing viewport is immediately replaced in the same draw pass. **Clear it before terminal scrolling can move composer contents into scrollback.**

少了清 viewport 这一步,终端滚动会把 composer(输入框)内容推进 scrollback。

另一处差异:`ZellijRaw` 配 `HistoryLineWrapPolicy::Terminal`,把换行交给终端以保留 soft-wrap 元数据,使终端选择复制能拿到原始源文本(`:51-52`);`Standard` 用 `PreWrap`,自己预先 wrap。

### tmux 不降级

tmux 检测(`TMUX` / `TMUX_PANE`)在 codex 里只用于 OSC52 passthrough 和图片协议,**从不影响 insert_history**。tmux 走 `Standard`,即 DECSTBM。

### 逃生舱

`ZTUI_NO_SCROLL_REGION=1` 强制走非滚动区路径。作用点在 **per-batch 选择处**,不放进启动 caps:

```rust
fn insert_mode(is_zellij: bool, no_scroll_region: bool, wrap: WrapPolicy) -> InsertHistoryMode {
    if no_scroll_region || (is_zellij && wrap == WrapPolicy::Terminal) {
        InsertHistoryMode::Raw
    } else {
        InsertHistoryMode::Standard
    }
}
```

真机上发现某终端不照做时,不改代码就能绕过,同时收集黑名单。

---

## 能力矩阵

| 环境 | `interactive_output` | DECSTBM(per-batch) | `scrollback_purge` | Ctrl-Z |
|---|---|---|---|---|
| Windows Terminal | 是 | 是 | 是 | 不适用 |
| Windows conhost(现代) | 是 | 是 | 是 | 不适用 |
| Git Bash / MSYS | 是 | 是 | 是 | 不适用 |
| macOS Terminal.app | 是 | 是 | 是 | 是 |
| macOS iTerm2 / Ghostty | 是 | 是 | 是 | 是 |
| Linux xterm/gnome/kitty/alacritty | 是 | 是 | 是 | 是 |
| tmux / screen | 是 | **是** | 否 | 是 |
| Zellij | 是 | 仅 non-terminal-wrap | 否 | 是 |
| CI / 管道 / 重定向 | 否 | 不适用 | 不适用 | 不适用 |

`interactive_output == false` 时**整条 TUI 关闭**,退化为纯 `println!` 顺序打印。不做部分降级:emit 层直写裸字节,无 VT 时相对光标移动同样不被解析;而"改走 crossterm Command 吃 WinAPI 回退"会丢掉 DECSTBM / ED3 / 精确行级控制,等于维护第二套渲染器。

## macOS 特有注意

`crossterm::terminal::supports_keyboard_enhancement()`:

| 平台 | 实现 |
|---|---|
| Unix | `sys/unix.rs:188-190` — 真实探测(发 `CSI ?u` 等回复) |
| Windows | `sys/windows.rs:75-77` — 硬编码 `Ok(false)` |

Terminal.app 不支持 Kitty 键盘协议,所以复杂组合键受限。这是终端能力问题,与后端选择无关(Windows 上同理:ConPTY 编不出 VT 的键会被丢弃)。启用增强前应先探测。
