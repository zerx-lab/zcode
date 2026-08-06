# 调研过程与关键发现

记录得出结论的路径,以及途中被推翻的假设。避免后来者重走弯路。

## 问题演进

1. "类似 pi.dev 那种保留在终端里的 TUI 是 ratatui 的什么模式" → `Viewport::Inline`
2. "已进入缓冲区的 UI 如何改变形态(折叠/展开 diff)" → 初判"scrollback 不可变,只能在 viewport 内折叠"
3. "但 oh-my-pi 的 ctrl+o 可以展开收起所有缓冲区内的内容" → **推翻上一条**:scrollback 可以整体擦除后重放
4. "框内窗口实时滚动如何做到,不可能全量重绘吧" → 固定高度 + in-window diff
5. "完全自研匹配 oh-my-pi 是否可行,可行就用 ratatui" → 可行,但要 fork `Terminal`
6. "有更合适的 crate 搭配吗,要求依赖持续更新" → 调研 termina / termwiz / teleprint
7. "需要支持 Windows,helix 如何在 Windows 支持" → **决定性发现**:helix 在 Windows 上不用 termina
8. "三平台都要支持" → 全平台 crossterm,零后端分叉

## 被推翻的假设

### 假设 1:scrollback 绝对不可变

**推翻依据**:`CSI 3 J`(ED3,xterm "Erase Saved Lines")可以擦除已保存的滚动历史。`crossterm::terminal::ClearType::Purge` 映射到它(`crossterm/src/terminal.rs:277-278, 347`)。

oh-my-pi 的 `ctrl+o` 正是"擦掉整个 scrollback + 从 home 重放整个 transcript"。

修正后的表述:**日常增量 append,用户手势时才 ED3 全量重放。** 且 divergence rebuild 默认关闭——内容漂移时宁可留 stale row。

### 假设 2:`C ≤ B` 是不变量

**错误。** `docs/tui-core-renderer.md:52-53` 明确:unpinned 帧的 commit end 是 `max(C, W)`,**不受 B 约束**。B 之后仍可变的行离开窗口后会作为 frozen snapshot 提交,所以 C 可以越过 B。

只有 pinned region 才把可变尾部留在 viewport 内(`tui.ts:3093` 的 `liveRegionPinned` 分支)。

正确分区是三段而非两段,见 `architecture.md`。

### 假设 3:"不是全量重绘"

**过度简化。** sliding tail 会让框内几乎每行都变,整个 live box 都要重写;逻辑上每帧仍完整重建整帧。

$L$ 恒定省下的是 $O(\text{history})$,不是 $O(\text{box})$。ratatui 是同一个模型:`ratatui-core/src/terminal/render.rs:43-47` 要求 callback 必须完整渲染整帧,再只写 buffer diff。

### 假设 4:"不要用 ratatui"

**过头。** 正确的切法是留 `Buffer` / `Widget` / `Line` / `Style`,只换掉 `Terminal`。codex 的 `custom_terminal.rs` 文件头保留了 ratatui 的 MIT 声明,就是这个做法。

而且 codex 开的四个 unstable feature 精确解决了"ratatui 做不到"的每一条,特别是 `unstable-backend-writer`——有了它就能在框架内合法拿到 writer 直发 escape,不必绕过 Backend 导致 back buffer 失同步。

### 假设 5:后端选 termina

**推翻依据(两条,都是实证)**:

a. termina 作者自己的编辑器 helix 在 Windows 上编译期切回 crossterm(`helix-term/src/application.rs:47-61`)。

b. termina 的 Windows VT 输入路径有静默丢键(`termina/src/parse/windows.rs:54-58`):ConPTY 编不出 VT 的组合键以 `uChar == 0` 到达并被 `continue` 丢弃。

补充:后来发现这堵墙与后端无关——crossterm 的 `supports_keyboard_enhancement()` 在 Windows 上硬编码 `Ok(false)`(`sys/windows.rs:75-77`)。两个后端面对同一个终端能力限制,所以它既不构成选 termina 的理由,也不构成弃用的理由。真正的理由是 a。

### 假设 6:DECSTBM 让 ledger 变得不必要

**错误。** DECSTBM 只优化"把新历史注入 scrollback 且不碰 live viewport",**无法更新已提交行**。

要完整匹配 oh-my-pi 的 ctrl+o 语义,以下一样不能省:完整 transcript model、C/W/B 三量、frozen snapshot 语义、pinned region、手势触发的 ED3 全量重放。

**DECSTBM 让追加变便宜,不让改写变可能。**

### 假设 7:override `is_ansi_code_supported() -> true` 就够

**错误,且是 blocker。** crossterm 全库唯一启用 `ENABLE_VIRTUAL_TERMINAL_PROCESSING` 的地方是 `ansi_support.rs:16` 的私有 `enable_vt_processing()`,唯一调用点是 `supports_ansi():39`,带 `Once` 守卫。

override 会短路它 → VT 永不启用 → conhost 把 escape 当字面字节输出。

codex 的正确做法是三段式:独立启用(`tui.rs:1137-1177`)→ 在 `set_modes()` 第一行调用(`:217-218`)→ 之后 override 才安全(`insert_history.rs:345-349`)。

### 假设 8:`ansi_escapes` 单个 bool 门控一切

**错误,两处。**

a. `ensure_virtual_terminal_processing() -> Ok(())` 不能当能力探针:`:1149-1156` 对无效句柄和 `GetConsoleMode` 失败都返回 Ok。重定向输出会被误判为可发控制序列。判定必须回读 `GetConsoleMode` 确认 bit。

b. 一个 bool 不能同时管 DECSTBM 和 ED3:mux 只禁 ED3(`oh-my-pi tui.ts:12-13`),DECSTBM 照用。codex 在 tmux 下走 `Standard`(即 DECSTBM),只对 Zellij 特判。

### 假设 9:`scroll_regions` 是启动时能力

**错误。** `codex-rs/tui/src/tui.rs:908-913` 每个 history batch 独立选模式:`is_zellij && wrap_policy == Terminal → ZellijRaw`,否则 `Standard`。同一会话内 wrap policy 会变,做成启动时 bool 会走错路径。

### 假设 10:平台分叉只有 `caps.rs`

**遗漏,且第一次修补仍不完整。** Unix 上 `Ctrl-Z` suspend 是用户预期,需要 `job_control.rs`(~210 行,`#[cfg(unix)]`)+ `libc` 依赖。

但补上 `job_control.rs` 之后仍然漏了它的**硬依赖** `terminal_probe.rs`(~250 行,同样 unix-only):`job_control.rs:83` 调 `terminal_probe::cursor_position` 重锚定 viewport,缺它整个第 3 期不成立。该模块有独立契约(event stream 必须暂停、100ms 预算、失败 `Ok(None)`、dup fd + `O_NONBLOCK` + `poll`、复用 startup parser 以免泄漏 focus report),不能当作"顺手调一下 crossterm 的 `position()`"。

所以平台相关代码是四处约 580 行,不是最初说的"只有 caps.rs 120 行",也不是第一次修补后的"330 行"。

### 假设 11:`is_tty && ansi_ok` 会抵消 Git Bash 的 TERM fallback

**未成立。** Rust std 的 `is_terminal()` 在 Windows 上已经识别 MSYS/Cygwin pty(`std/.../is_terminal/windows.rs:21-22, 67-69`:检查 pipe 名是否 `msys-`/`cygwin-` 前缀且含 `-pty`)。

所以 Git Bash 下 `is_tty == true`,`TERM` fallback 正常生效。而 `myapp > out.txt` 被正确判为 false——这比 crossterm 的裸 OR 更严格,后者没有 tty 门,重定向到文件且 `TERM` 已设时会误判。

## 关键发现

### 1. oh-my-pi 的设计动机:删掉不可观测变量上的猜测

`docs/tui-core-renderer.md:28-36` 是整份文档的核心论点:

> 没有任何跨终端 API 能查询 viewport 滚动位置——DECSLRM 无此能力,probe 会撒谎,POSIX 根本没有 API。旧引擎试图**猜**何时可以安全重写 native scrollback,而在这个不可观测变量上的每一种策略选择,都只是在几个失败家族之间来回交换:yank ↔ flash ↔ corruption ↔ invisible-until-resize。

新引擎的解法是彻底删掉猜测:scrollback 无条件 append-only,渲染器"never needs to know whether the user has scrolled away from the tail"。

**这是最有价值的一条经验。** 直接翻译成两个禁令:不写"检测用户是否滚上去了"的逻辑;普通增量帧只用相对光标移动(历史注入是唯一豁免,但豁免有条件,且两条注入路径的保护机制不同——详见 `architecture.md` 不变量 3 与第 4 节对比表)。

### 2. codex 的 DECSTBM 技巧优于 oh-my-pi 的 insert_before

oh-my-pi 的 `insert_before` 需要重画 viewport;codex 用滚动区做到 viewport 零字节重写(`insert_history.rs:217-245`)。

`insert_history.rs:236` 的注释把契约写死:"insert_history_lines should be cursor-position-neutral :)"

### 3. 固定高度是流式框能低成本滚动的唯一原因

不是什么聪明的增量算法,就是把 tail window 高度钉进 viewport(`render-utils.ts:222-225`)。

违反它的代价有案可查:`CHANGELOG.md:2342` 记录了框超出 viewport 时,可变尾部滚到 commit window 之上,每帧提交一份新快照,往 scrollback 堆了几十条重复 banner。

### 4. teleprint 的判断:每个 AI CLI 都在手搓这个核心

`AnswerDotAI/teleprint` README:

> Prior art that validates the block concept without occupying this slot: Warp (blocks, but by being the terminal emulator), Ink's Static/dynamic split (same committed/live division, no interactivity in the committed region, React-centered), Textual's inline mode (the repaint mechanics, but everything stays inside the app region and clears on exit). The strongest demand evidence: Claude Code (Ink/React) and codex (ratatui/Rust) independently converged on this exact surface grammar — print-through transcript, gutter-marked block types, repainted input-plus-status tail. **Every AI CLI is currently hand-rolling a private version of this core.**

它的核心不变量值得记录:

> There is one history — the printed transcript — and anything that scrolls is a view of it. Never a parallel world.

结论:Rust 生态没有现成 crate 实现这个模型,自研是唯一路径。可抄的是 codex(Rust,同构)和 oh-my-pi(TS,ledger 数学更完整)。

### 5. crossterm 的状态:开发活跃但不发版

crates.io 停滞 487 天(0.29.0 @ 2025-04-05),GitHub 4 天前还有 commit。master 攒了 16 个月未发布改动。

这是选型时必须知道的事实,但不构成否决理由——我们的依赖面窄(event + raw mode + `impl Write`),且 escape 自己发。

## 未解风险

| 风险 | 状态 | 缓解 |
|---|---|---|
| macOS Terminal.app 的 DECSTBM + scrollback 行为 | 未找到缺陷报告;codex 在 macOS 量产,若有问题早已爆发 | 真机烟测;`ZTUI_NO_SCROLL_REGION=1` 逃生舱 |
| "滚动区含 row 0 才进 scrollback"非标准强制 | `ratatui-core/src/backend.rs:382-383` 承认是经验行为 | 同上 |
| Zellij 之外是否还有终端不约束 soft-wrap 续行 | 未知 | 逃生舱 + 收集黑名单 |
| crossterm 长期不发版 | 已知 | 依赖面窄化;后端隔离在输入层 |
