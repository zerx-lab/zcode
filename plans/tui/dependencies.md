# 依赖选型

## 最终清单

```toml
[dependencies]
# 排版原语 + Buffer/Widget。不使用它的 Terminal（见 modules.md）
ratatui = { version = "0.30", default-features = false, features = [
    "crossterm",
    "scrolling-regions",            # DECSTBM
    "unstable-backend-writer",      # backend_mut() -> &mut impl Write
    "unstable-rendered-line-info",  # widget 渲染后行数，用于高度测量
    "unstable-widget-ref",          # WidgetRef，按引用渲染
] }
crossterm = { version = "0.29", features = ["bracketed-paste", "event-stream"] }
unicode-width = "0.2"
unicode-segmentation = "1"
textwrap = "0.16"

[target.'cfg(windows)'.dependencies]
windows-sys = { version = "0.60", features = ["Win32_Foundation", "Win32_System_Console"] }

[target.'cfg(unix)'.dependencies]
libc = "0.2"

[dev-dependencies]
vt100 = "0.16"
insta = "1"
```

## 逐项理由

| crate | 用途 | 不可替代的原因 |
|---|---|---|
| `ratatui` | `Buffer` / `Line` / `Span` / `Paragraph` / `Widget` / cell diff | 排版与 cell 差分免费;自己写是纯浪费。`Terminal` 必须 fork,但其余部分完全可用 |
| `crossterm` | raw mode、事件流、`impl Write` | 见下节的后端决策 |
| `windows-sys` (win) | 自己启用 VT,不经 crossterm 的 `supports_ansi()` | crossterm 的 VT 启用藏在 `Once` 副作用里,不可靠(详见 `platform.md`) |
| `libc` (unix) | `kill(0, SIGTSTP)`、`tcflush` | job control 必需。**不需要 `signal-hook`**:`SIGTSTP` 停住进程,`SIGCONT` 后从下一行继续,无需 handler |
| `unicode-width` | 列宽 | `len()` / `chars().count()` 会算错。unicode-rs 官方 |
| `unicode-segmentation` | grapheme 切分 | 同上 |
| `textwrap` | ANSI 感知换行 | 可自写 ~150 行替代,不急 |
| `vt100` (dev) | 解析自己发出的字节,校验整屏网格 | codex 用同一个做 DECSTBM 测试;实现了滚动区 |
| `insta` (dev) | 快照 | codex 同款 |

## ratatui feature 说明

四个 unstable feature 全部存在于 0.30.2(`ratatui-0.30.2/Cargo.toml:88-138`),且 `unstable` 是它们的聚合 feature。

codex 的生产配置(`codex-rs/tui/Cargo.toml:82-87`)开的正是这四个,可直接对照。

`unstable-backend-writer` 是关键:有了它就能在 ratatui 框架内**合法**拿到 `&mut impl Write` 直接发 escape,不必绕过 Backend 导致 back buffer 与真实屏幕失同步。

## 后端决策:全平台 crossterm

### 结论

单一后端,零平台分叉。

### 支撑数据

| 论据 | 证据 |
|---|---|
| Unix 上 crossterm 无能力短板 | Kitty 协议完整:`event.rs:292-527` 的 `KeyboardEnhancementFlags` / `PushKeyboardEnhancementFlags` / CSI-u / 媒体键 / `KeyEventKind`;focus change `EnableFocusChange`;bracketed paste |
| 三平台量产验证 | codex 全平台单一后端(`codex-rs/tui/Cargo.toml:71`),macOS 是其主力平台 |
| Windows 组合键限制与后端无关 | `supports_keyboard_enhancement()` 在 Windows 上硬编码 `Ok(false)`(`sys/windows.rs:75-77`);termina 面对的是同一堵墙(ConPTY 编不出 VT 就丢弃) |
| resize 已被抽象 | `Event::Resize` 统一了 SIGWINCH 与 `WINDOW_BUFFER_SIZE_EVENT` |
| 输出侧不需要后端能力 | escape 全自己生成,后端只需 `impl Write` |

### 维护风险与缓解

| crate | 最新版 | crates.io 最后发版 | 距今 | GitHub 最后 commit | 近期下载 |
|---|---|---|---|---|---|
| ratatui | 0.30.2 | 2026-06-19 | 48d | 1d | 15.7M |
| ratatui-core | 0.1.2 | 2026-06-19 | 48d | — | 9.2M |
| ratatui-widgets | 0.3.2 | 2026-06-19 | 48d | — | 8.9M |
| **crossterm** | 0.29.0 | 2025-04-05 | **487d** | **4d** | 40.9M |
| termina | 0.3.3 | 2026-05-30 | 68d | — | 1.9M |
| unicode-width | 0.2.2 | 2025-10-06 | 303d | — | 148.2M |
| unicode-segmentation | 1.13.3 | 2026-06-01 | 65d | — | 114.4M |
| textwrap | 0.16.2 | 2025-03-03 | 520d | — | 51.1M |
| vt100 | 0.16.2 | 2025-07-12 | 390d | — | 3.1M |
| insta | 1.48.0 | 2026-06-11 | 55d | — | 22.5M |

(数据采集于 2026-08-06,crates.io API)

**crossterm 是"开发活跃但不发版"**:crates.io 停滞 487 天,GitHub 4 天前还有 commit,master 攒了 16 个月未发布改动。

缓解措施:

1. 依赖面收窄到 `event` + `terminal::{enable_raw_mode, disable_raw_mode, size}` + `impl Write`。这些 API 多年未变。
2. escape 全自己发,不依赖它的 `Command` 抽象(除自定义的 DECSTBM/ED3 两个)。
3. 真出事只需换输入层,渲染层零改动。

`textwrap`(520d)和 `vt100`(390d)发版慢但 API 冻结、无 CVE 面。极致新鲜度可自写换行(基于 `unicode-width` + `unicode-segmentation`,约 150 行)。

## 被排除的方案

### termina

由 helix-editor 维护,定位是 "a cross between Crossterm and TermWiz with a **lower level API which exposes escape codes** to consuming applications"。类型化 CSI 很吸引人:

- `escape/csi.rs:802-803` — `Cursor::SetTopAndBottomMargins`(DECSTBM),且 `top==1 && bottom==MAX` 自动优化为 `\x1b[r`
- `escape/csi.rs:1175-1176` — `Edit::EraseInDisplay(EraseInDisplay::EraseScrollback)`(ED3)

省掉自己实现 Command 的活。**但排除,依据有二:**

**a. termina 的作者自己的编辑器在 Windows 上不用它。**

`helix-term/src/application.rs:47-61` 是编译期硬分叉:

```rust
#[cfg(all(not(windows), not(feature = "integration")))]
type TerminalBackend = TerminaBackend;                     // Unix
#[cfg(all(windows, not(feature = "integration")))]
type TerminalBackend = CrosstermBackend<std::io::Stdout>;  // Windows
```

配套:`helix-tui/src/backend/mod.rs:17-20` 是 `#[cfg(all(feature = "termina", windows))] mod crossterm;` — 开了 termina,Windows 仍编译 crossterm backend。`helix-term/Cargo.toml:93` 声明 `[target.'cfg(windows)'.dependencies] crossterm = "0.28"`。

**b. Windows 输入有静默丢键机制。**

`termina/src/parse/windows.rs:54-58`:

```rust
let byte = unsafe { record.uChar.AsciiChar } as u8;
// The zero byte is sent when the input record is not VT.
if byte == 0 {
    continue;                    // 静默丢弃
}
```

ConPTY 无法编码成 VT 字节的组合键以 `uChar == 0` 到达,被直接丢掉。这就是 README 表格里 `Extended key events: VTE Mode = terminal-dependent` 的实现机制。

补充说明(避免误传):

- `windows-legacy` feature **不是**取舍开关。`InputReaderMode::Vte` 是默认(`parse/windows.rs:24, 30-32`),feature 只是编译进 legacy 解析器并暴露 `PlatformTerminal::with_mode()`(`terminal/windows.rs:415-416`)让你运行时选。
- 丢 bracketed paste 发生在**选** `InputReaderMode::Legacy` 时,不是开 feature 时。
- `parse/windows.rs:49-53` 的注释 "This skips 'down's" 与代码 `if record.bKeyDown == 0 { continue; }` 矛盾——实际跳的是 key-up。注释写反,不影响功能。

**保留后路**:架构上把后端隔离在输入层,未来若 Unix 侧想换 termina,渲染层无需改动。但不为此在第一版引入双后端。

### termwiz

wezterm 的终端库。依赖树过重(整个 wezterm 生态),crates.io 最后发版 503 天前。有 `Surface` + diff 概念,但同样是固定尺寸网格,不解决我们的核心问题。

### tui-scrollview / tui-scrollbar

viewport **内部**的虚拟滚动,与本模型正交。我们的滚动是终端原生的。

### teleprint

`AnswerDotAI/teleprint`,Python,"Transcript-first terminal UI"。设计笔记极有价值(见 `research.md`),但语言不匹配。

其 README 的判断值得记录:

> Every AI CLI is currently hand-rolling a private version of this core.

Rust 生态中确实不存在现成 crate 实现这个模型。codex 的做法就是 fork ratatui 的 Terminal 并自己写 1070 行 `insert_history.rs`。
