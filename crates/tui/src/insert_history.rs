//! DECSTBM 历史注入：把已定稿的行写进终端原生 scrollback，位置在 viewport 上方。
//!
//! 这是全 crate 唯一允许在增量路径里做绝对光标定位的模块（不变量 3，见 `lib.rs` 顶部文档）。
//! 抄源：`codex-rs/tui/src/insert_history.rs`（结构、字段名、注释出处见各函数文档）。
//!
//! 两条路径、各自的保护机制、为什么不能混用，见 [`insert_history_lines`] 的文档。

use std::fmt;
use std::io::{self, Write};
use std::ops::Range;

use crossterm::Command;
use crossterm::cursor::{MoveDown, MoveTo, MoveToColumn, RestorePosition, SavePosition};
use crossterm::queue;
use crossterm::style::{
    Attribute as CAttribute, Color as CColor, Colors, Print, SetAttribute, SetBackgroundColor,
    SetColors, SetForegroundColor,
};
use crossterm::terminal::{Clear, ClearType};
use ratatui::backend::{Backend, IntoCrossterm};
use ratatui::style::{Color, Modifier};
use ratatui::text::{Line, Span};

use crate::terminal::Terminal;
use crate::wrap;

/// 历史行在写进 scrollback 之前的换行策略。
///
/// 两种策略服务不同的 [`InsertHistoryMode`]：`Standard` 配 `PreWrap`，`ZellijRaw` 配
/// `Terminal`（见 `plans/tui/platform.md:265`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryLineWrapPolicy {
    /// 本 crate 用 [`wrap::wrap_line`] 预先按显示宽度硬换行，续行落在 grapheme 边界。
    PreWrap,
    /// 保持原始行不动，把折行完全交给终端：保留 soft-wrap 元数据，让终端框选复制能拿到
    /// 未被硬断开的原始源文本。
    Terminal,
}

/// 历史注入使用的转义序列策略；由 [`insert_history_mode`] 逐 batch 计算，绝不是启动期常量。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertHistoryMode {
    /// DECSTBM：用成对的 [`SetScrollRegion`] / [`ResetScrollRegion`] 把影响锁在 viewport 之上。
    Standard,
    /// 不用滚动区，靠"先清 viewport、同一 draw pass 内完整重画"代替。
    ZellijRaw,
}

/// per-batch 计算历史注入应该走哪条转义序列路径。
///
/// **这不是启动时能力，绝不能缓存成 `scroll_regions: bool`。** 同一会话内
/// [`HistoryLineWrapPolicy`] 会逐 batch 切换（流式输出用 `Terminal` 保留 soft-wrap 元数据，
/// 定稿后可能改回 `PreWrap`），若在启动时把 mode 定死，wrap 策略变化时就会走错保护路径
/// （`plans/tui/platform.md:228-246`）。
///
/// 决策规则（`plans/tui/platform.md:276-282`，源自 `codex-rs/tui/src/tui.rs:908-913`）：
///
/// - `no_scroll_region`（逃生舱 `ZTUI_NO_SCROLL_REGION=1`，调用方从
///   [`crate::caps::force_no_scroll_region`] 读取）为真时，无条件降级到 `ZellijRaw`。
/// - 否则只有 **Zellij 环境**（`is_zellij`）**且**本 batch 用 `HistoryLineWrapPolicy::Terminal`
///   时才降级——Zellij 不会把 soft-wrap 续行约束在调用方的 DECSTBM 滚动区内，继续走
///   `Standard` 会把续行写到滚动区之外，污染 viewport。
/// - **tmux 不降级。** tmux 检测（`TMUX`/`TMUX_PANE`）只影响 OSC52/图片协议 passthrough，
///   从不影响这里；`is_zellij == false` 时无论 wrap policy 是什么恒定返回 `Standard`，
///   DECSTBM 照用。
#[must_use]
pub fn insert_history_mode(
    is_zellij: bool,
    no_scroll_region: bool,
    wrap_policy: HistoryLineWrapPolicy,
) -> InsertHistoryMode {
    if no_scroll_region || (is_zellij && wrap_policy == HistoryLineWrapPolicy::Terminal) {
        InsertHistoryMode::ZellijRaw
    } else {
        InsertHistoryMode::Standard
    }
}

/// 把 `lines` 写进 scrollback，位置在当前 viewport 上方；返回实际推进的物理行数。
///
/// 两条路径的保护机制完全不同，**不可混用**：在 `ZellijRaw` 里加滚动区、或在 `Standard`
/// 里省掉清 viewport，都会把 composer（输入框）内容冲进 scrollback
/// （源自 `codex-rs/tui/src/insert_history.rs:165-166` 的注释）。
///
/// | | [`InsertHistoryMode::Standard`] | [`InsertHistoryMode::ZellijRaw`] |
/// |---|---|---|
/// | 滚动区 | [`SetScrollRegion`] / [`ResetScrollRegion`] 成对，锁定影响范围 | 不用 |
/// | viewport | 一个字节不动 | 先 `clear_after_position` 清掉，同一 draw pass 内完整重画 |
/// | 绝对 `MoveTo` | 有 DECSTBM 保护 | 没有滚动区保护，靠"先清后重画"代替 |
/// | cursor-position-neutral | 是 | 是 |
/// | 换行元数据 | 预先按 `PreWrap` 硬换行 | 保留 `Terminal` 策略，交终端软换行 |
///
/// 共同点只有 cursor-position-neutral：两条路径结束时都把光标放回调用前的
/// `last_known_cursor_pos`，用的是 `MoveTo` 而不是 `Terminal::set_cursor_position`——后者会
/// 更新 `Terminal` 自己跟踪的光标位置，让"存/恢复"这一对不再是唯一权威
/// （源自 `codex-rs/tui/src/insert_history.rs:233-236` 的注释）。
///
/// # `Standard`：DECSTBM（`codex-rs/tui/src/insert_history.rs:193-245`）
///
/// 1. viewport 未触底时先用 `\x1bM`（RI，Reverse Index）在受限滚动区内把 viewport 向下推，
///    腾出等于本批行数（不超过屏幕剩余空间）的空间；已触底则不推，直接在原地写。
/// 2. `SetScrollRegion(1..area.top())`——**1-based，必须包含 row 0**。硬约束
///    （`ratatui-core/src/backend.rs:340-343`）：滚动区含 row 0 时滚出的行才会被拷进
///    scrollback，不含 row 0 则直接丢弃；且这条行为是"终端事实上的实现"而非标准强制
///    （`:382-383`），所以真机烟测不可省。
/// 3. `MoveTo(0, cursor_top)` 把光标放到滚动区末行。
/// 4. 逐行 `Print("\r\n")` + `write_history_line`。
/// 5. `ResetScrollRegion` + `MoveTo(last_cursor_pos)` 恢复光标，两条命令必须成对。
///
/// # `ZellijRaw`：不用滚动区（`codex-rs/tui/src/insert_history.rs:163-192`）
///
/// Zellij 不把 soft-wrap 续行约束在调用方的滚动区内，所以走一条完全不同的路径：先
/// `clear_after_position` 清 viewport（防止终端滚动把 composer 内容推进 scrollback），
/// 绝对定位写历史（首行不加前导 `\r\n`），再预留 `area.height` 个空行让历史紧贴 composer
/// 上方，最后恢复光标并重算 viewport 顶部。
pub fn insert_history_lines<B>(
    terminal: &mut Terminal<B>,
    lines: &[Line<'_>],
    mode: InsertHistoryMode,
    wrap_policy: HistoryLineWrapPolicy,
) -> io::Result<u16>
where
    B: Backend<Error = io::Error> + Write,
{
    // 空批次直接短路：不碰 backend、不动 viewport、不计入 note_history_rows_inserted。
    // 这一步不是照抄 codex——codex 对空切片仍会跑完整套 DECSTBM 序列（净效果是空操作，
    // 但字节确实发出了）。这里选择更保守的"零输入零副作用"，代价可忽略，行为更好验证。
    if lines.is_empty() {
        return Ok(0);
    }

    // 几何取 `Terminal` 缓存的屏幕尺寸，不在这里重新查后端。两个理由：
    // 一是不能有静默 fallback（`.omp/RULES.md:9`）——`size()` 出错时退成 0×0 会按空屏
    // 重算 viewport，把活跃区顶到 row 0；二是本批的行数与提交计划是调用方按
    // `autoresize()` 刚缓存的那个尺寸算出来的，中途重新查一次会让注入的几何与账本脱节。
    let screen_size = terminal.last_known_screen_size();
    let mut area = terminal.viewport_area();
    let mut should_update_area = false;
    let last_cursor_pos = terminal.last_known_cursor_pos();

    let wrap_width = usize::from(area.width.max(1));
    let mut wrapped: Vec<Line<'static>> = Vec::with_capacity(lines.len());
    let mut wrapped_rows: usize = 0;
    for line in lines {
        wrapped_rows += wrap::line_rows(line, wrap_width);
        match wrap_policy {
            HistoryLineWrapPolicy::Terminal => wrapped.push(wrap::line_to_static(line)),
            // codex 在这里还有一条 URL 特判分支（line_contains_url_like /
            // line_has_mixed_url_and_non_url_tokens），依赖它自己的 `wrapping` 模块，本仓没有
            // 移植。不抄的后果：超长 URL 在 PreWrap 下会在 wrap_width 处被硬断行，选中复制拿到
            // 的是断成两截的碎片，终端也可能因此认不出这是一个可点击链接。等 zcode-text 有等价
            // 能力（URL token 识别）时再补这条特判，现在先保证行为正确、不做半吊子适配。
            HistoryLineWrapPolicy::PreWrap => wrapped.extend(wrap::wrap_line(line, wrap_width)),
        }
    }
    let wrapped_lines = u16::try_from(wrapped_rows).unwrap_or(u16::MAX);

    // DECSTBM 的几何前提：viewport 上方必须**至少有一行**。
    // `area.top() == 0`（活跃区占满屏幕，例如 full paint 后 viewport 落在 row 0）时
    // `SetScrollRegion(1..0)` 会发出 `CSI 1;0 r` —— DECSTBM 非法参数，多数终端整条忽略，
    // 于是紧接着的 `MoveTo(0, cursor_top)` + `\r\n` 就直接打进活跃区里。
    //
    // 改道非滚动区路径而不是报错：那条路径的两个附加条件在这里天然成立
    // （先 `clear_after_position` 清 viewport、调用方在同一 draw pass 内完整重画它），
    // 所以它是 `plans/tui/architecture.md:163-173` 对比表里唯一可用的另一套保护机制。
    // **这不是能力降级**，是几何决定的路径选择，与 `insert_history_mode` 的 env 判定正交。
    let mode = if mode == InsertHistoryMode::Standard && area.top() == 0 {
        tracing::debug!("viewport 顶在 row 0，滚动区无处安放，改走非滚动区注入");
        InsertHistoryMode::ZellijRaw
    } else {
        mode
    };

    match mode {
        InsertHistoryMode::ZellijRaw => {
            // viewport 会在同一 draw pass 内被完整替换。必须在终端滚动把 composer 内容推进
            // scrollback 之前清掉它——少了这一步就是 bug（`insert_history.rs:165-166`）。
            terminal.clear_after_position(area.as_position())?;
            let writer = terminal.backend_mut();
            queue!(writer, MoveTo(0, area.top()))?;
            for (index, line) in wrapped.iter().enumerate() {
                if index > 0 {
                    queue!(writer, Print("\r\n"))?;
                }
                write_history_line(writer, line, wrap_width)?;
            }

            // 写原始源文本让终端自己软换行，保留了 soft-wrap 元数据。预留 area.height 个空行，
            // 让历史紧贴 composer 上方，即使本批比可见历史区还高也不会露出旧内容。
            for _ in 0..area.height {
                queue!(writer, Print("\r\n"), Clear(ClearType::UntilNewLine))?;
            }
            // 绝对定位恢复光标，没有 DECSTBM 保护——安全性完全来自上面"先清后重画"这一对条件。
            queue!(writer, MoveTo(last_cursor_pos.x, last_cursor_pos.y))?;

            let viewport_top = area
                .top()
                .saturating_add(wrapped_lines)
                .min(screen_size.height.saturating_sub(area.height));
            if area.y != viewport_top {
                area.y = viewport_top;
                should_update_area = true;
            }
        }
        InsertHistoryMode::Standard => {
            let writer = terminal.backend_mut();
            let cursor_top = if area.bottom() < screen_size.height {
                // viewport 未触底：先把它往下推，腾出不超过屏幕剩余空间的历史空间。
                let scroll_amount = wrapped_lines.min(screen_size.height - area.bottom());
                let top_1based = area.top().saturating_add(1);
                queue!(writer, SetScrollRegion(top_1based..screen_size.height))?;
                queue!(writer, MoveTo(0, area.top()))?;
                for _ in 0..scroll_amount {
                    // RI（Reverse Index）：光标停在滚动区顶行时向上滚一行，等价于在滚动区内
                    // 把内容整体下移一行——这是"腾空间"而非"写内容"。
                    queue!(writer, Print("\x1bM"))?;
                }
                queue!(writer, ResetScrollRegion)?;

                let cursor_top = area.top().saturating_sub(1);
                area.y = area.y.saturating_add(scroll_amount);
                should_update_area = true;
                cursor_top
            } else {
                area.top().saturating_sub(1)
            };

            // 把滚动区限制在"屏幕顶部到 viewport 顶部"这一段：往这段区域打印新行时，
            // 只有这段会滚动，viewport 本身不受影响。光标放在滚动区末行，从那里开始写。
            queue!(writer, SetScrollRegion(1..area.top()))?;

            // 用 MoveTo 而非 Terminal::set_cursor_position：避免动到 Terminal 自己跟踪的
            // last_known_cursor_pos，让存/恢复这一对成为唯一权威——insert_history_lines
            // 必须是 cursor-position-neutral（`insert_history.rs:233-236`）。
            queue!(writer, MoveTo(0, cursor_top))?;

            for line in &wrapped {
                queue!(writer, Print("\r\n"))?;
                write_history_line(writer, line, wrap_width)?;
            }

            queue!(writer, ResetScrollRegion)?;
            queue!(writer, MoveTo(last_cursor_pos.x, last_cursor_pos.y))?;
        }
    }

    // cursor-position-neutral 的**账本侧**。两条路径最后都发了绝对 MoveTo 回到进函数时的
    // 位置，但非滚动区路径中途经过 `clear_after_position`，那是一次真实的绝对定位、会改写
    // `last_known_cursor_pos`。不写回的话账本停在 viewport 顶端而真实光标在别处，
    // 下一帧的相对定位就整屏偏移。
    terminal.set_last_known_cursor_pos(last_cursor_pos);

    if should_update_area {
        terminal.set_viewport_area(area);
    }
    terminal.note_history_rows_inserted(wrapped_lines);

    Ok(wrapped_lines)
}

/// 写一条已经完成 wrap 的历史行：先清掉可能残留的续行内容，再按行级 + span 级样式输出
/// SGR 序列与文本。
///
/// 调用方契约（这里刻意不做，因为不同调用方的需求不同）：
/// - 不发任何光标定位序列——写完后光标停在本行内容之后，去哪由调用方决定；
/// - 不加前导或尾随的 `\r\n`——是否换行、换到哪由调用方决定。
///
/// `pub(crate)` 而非私有：`emit::Emitter::full_paint` 在 ED2 清屏后从 home 重放整条
/// committed prefix 时，同样需要逐行输出带样式的历史文本。那条路径不需要 DECSTBM（屏幕刚被
/// 清过，没有 viewport 需要保护），但必须复用这里的 SGR / 清行约定——否则会长出第二套
/// "行怎么渲染"的规则，两条路径的输出就可能不一致。
pub(crate) fn write_history_line<W: Write>(
    writer: &mut W,
    line: &Line<'_>,
    wrap_width: usize,
) -> io::Result<()> {
    let physical_rows = u16::try_from(wrap::line_rows(line, wrap_width)).unwrap_or(u16::MAX);
    if physical_rows > 1 {
        // 多物理行：先存光标，逐行下移清掉可能残留的旧续行内容（比如上一次这里写的是更长的
        // 一行），再恢复光标到本行起点，让接下来的 SGR + 文本输出从干净的续行开始。
        queue!(writer, SavePosition)?;
        for _ in 1..physical_rows {
            queue!(writer, MoveDown(1), MoveToColumn(0))?;
            queue!(writer, Clear(ClearType::UntilNewLine))?;
        }
        queue!(writer, RestorePosition)?;
    }

    let fg: CColor = line
        .style
        .fg
        .map_or(CColor::Reset, IntoCrossterm::into_crossterm);
    let bg: CColor = line
        .style
        .bg
        .map_or(CColor::Reset, IntoCrossterm::into_crossterm);
    queue!(writer, SetColors(Colors::new(fg, bg)))?;
    queue!(writer, Clear(ClearType::UntilNewLine))?;

    let merged_spans = merge_line_style(line);
    write_spans(writer, merged_spans.iter())
}

/// 把行级 style 合进每个 span：**行级打底、span 覆盖**。
///
/// 不合并的话行级样式（比如 blockquote 的整行绿色）不会体现在 ANSI 输出里——
/// [`write_spans`] 只看 span 自己的 style。
///
/// **方向不能反。** `Style::patch(self, other)` 是 `other.fg.or(self.fg)`
/// （`ratatui-core/src/style.rs:471-474`），参数一侧优先，所以必须是
/// `line.style.patch(span.style)`。这与 viewport 一侧 `Line` 的渲染语义一致
/// （先铺行样式，再让 span 样式盖上去）。codex `insert_history.rs:318-321` 写的是
/// `span.style.patch(line.style)`，照抄会让历史里的 span 级颜色被行级颜色吞掉，
/// 同一行在 viewport 和 scrollback 里显示成两种颜色。
fn merge_line_style<'a>(line: &'a Line<'_>) -> Vec<Span<'a>> {
    line.spans
        .iter()
        .map(|span| Span {
            style: line.style.patch(span.style),
            content: span.content.clone(),
        })
        .collect()
}

/// DECSTBM（Set Top and Bottom Margins），`CSI {start};{end} r`。`Range<u16>` 是 1-based
/// `[start, end)`：`end` 通常传屏幕总行数，表示滚动区下边界到屏幕最后一行。
///
/// 硬约束（`ratatui-core/src/backend.rs:340-343`）：滚动区**必须包含 row 0**才能让滚出的行
/// 进 scrollback，不含 row 0 则终端直接丢弃这些行——这不是标准强制的行为，是"终端事实上的
/// 实现"（`backend.rs:382-383`），[`insert_history_lines`] 用 `1..N` 而非任意区间正是为此，
/// 真机烟测不可省。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetScrollRegion(pub Range<u16>);

impl Command for SetScrollRegion {
    fn write_ansi(&self, f: &mut impl fmt::Write) -> fmt::Result {
        write!(f, "\x1b[{};{}r", self.0.start, self.0.end)
    }

    #[cfg(windows)]
    fn execute_winapi(&self) -> io::Result<()> {
        // 本仓 clippy::panic = deny：codex 原文这里是 panic!(...)，改成返回错误，
        // 让调用方决定如何处理（helix 的 crossterm backend 对同类命令也是这么做的，
        // helix-tui/src/backend/crossterm.rs:461-464）。
        Err(io::Error::other(
            "SetScrollRegion 只有 ANSI 表示，不支持 WinAPI 执行路径",
        ))
    }

    #[cfg(windows)]
    fn is_ansi_code_supported(&self) -> bool {
        // 这个 override 会短路 crossterm 唯一的 VT 启用副作用：`supports_ansi()` 内部用 Once
        // 守卫，第一次调用时顺带尝试 `enable_vt_processing()`（crossterm ansi_support.rs:39）。
        // 跳过它意味着 VT 必须已经由别处启用——前提是 `caps::apply_output_modes()` 已经独立
        // 走过 WinAPI 启用（platform.md:132-152）。两者解决的是不同问题：那边负责"控制台真的
        // 支持 VT"，这里只是不想让 DECSTBM 白白触发一次可能有副作用的探测。都需要，缺一个都
        // 不对。
        true
    }
}

/// `CSI r`：把滚动区重置回整个屏幕。必须与 [`SetScrollRegion`] 成对出现——这是不变量 3 对
/// [`InsertHistoryMode::Standard`] 的附加要求。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResetScrollRegion;

impl Command for ResetScrollRegion {
    fn write_ansi(&self, f: &mut impl fmt::Write) -> fmt::Result {
        write!(f, "\x1b[r")
    }

    #[cfg(windows)]
    fn execute_winapi(&self) -> io::Result<()> {
        Err(io::Error::other(
            "ResetScrollRegion 只有 ANSI 表示，不支持 WinAPI 执行路径",
        ))
    }

    #[cfg(windows)]
    fn is_ansi_code_supported(&self) -> bool {
        // 理由同 SetScrollRegion::is_ansi_code_supported。
        true
    }
}

/// 计算把 `from` 变成 `to` 需要发出的 `SetAttribute` 序列差分。
///
/// 刻意与 `terminal.rs` 里的另一份同名逻辑重复，不跨模块共享：那份服务的是
/// `Buffer`/`Cell` 级别的整屏 diff 渲染（每帧全量对比两张缓冲区），这份服务的是历史行
/// 直写（逐 span 顺序输出，没有 Cell 网格可比较）。两者的输入形状不同（Cell 对 vs. Style
/// 对），共享会被迫抽象出一层不必要的间接，得不偿失。
struct ModifierDiff {
    from: Modifier,
    to: Modifier,
}

impl ModifierDiff {
    fn queue<W>(self, mut w: W) -> io::Result<()>
    where
        W: Write,
    {
        let removed = self.from - self.to;
        if removed.contains(Modifier::REVERSED) {
            queue!(w, SetAttribute(CAttribute::NoReverse))?;
        }
        if removed.contains(Modifier::BOLD) {
            queue!(w, SetAttribute(CAttribute::NormalIntensity))?;
            if self.to.contains(Modifier::DIM) {
                queue!(w, SetAttribute(CAttribute::Dim))?;
            }
        }
        if removed.contains(Modifier::ITALIC) {
            queue!(w, SetAttribute(CAttribute::NoItalic))?;
        }
        if removed.contains(Modifier::UNDERLINED) {
            queue!(w, SetAttribute(CAttribute::NoUnderline))?;
        }
        if removed.contains(Modifier::DIM) {
            queue!(w, SetAttribute(CAttribute::NormalIntensity))?;
        }
        if removed.contains(Modifier::CROSSED_OUT) {
            queue!(w, SetAttribute(CAttribute::NotCrossedOut))?;
        }
        if removed.contains(Modifier::SLOW_BLINK) || removed.contains(Modifier::RAPID_BLINK) {
            queue!(w, SetAttribute(CAttribute::NoBlink))?;
        }

        let added = self.to - self.from;
        if added.contains(Modifier::REVERSED) {
            queue!(w, SetAttribute(CAttribute::Reverse))?;
        }
        if added.contains(Modifier::BOLD) {
            queue!(w, SetAttribute(CAttribute::Bold))?;
        }
        if added.contains(Modifier::ITALIC) {
            queue!(w, SetAttribute(CAttribute::Italic))?;
        }
        if added.contains(Modifier::UNDERLINED) {
            queue!(w, SetAttribute(CAttribute::Underlined))?;
        }
        if added.contains(Modifier::DIM) {
            queue!(w, SetAttribute(CAttribute::Dim))?;
        }
        if added.contains(Modifier::CROSSED_OUT) {
            queue!(w, SetAttribute(CAttribute::CrossedOut))?;
        }
        if added.contains(Modifier::SLOW_BLINK) {
            queue!(w, SetAttribute(CAttribute::SlowBlink))?;
        }
        if added.contains(Modifier::RAPID_BLINK) {
            queue!(w, SetAttribute(CAttribute::RapidBlink))?;
        }

        Ok(())
    }
}

/// 按顺序输出一串 span：SGR 属性差分 + 颜色变化时才发 `SetColors`，最后统一 reset。
fn write_spans<'a, I>(mut writer: &mut impl Write, content: I) -> io::Result<()>
where
    I: IntoIterator<Item = &'a Span<'a>>,
{
    let mut fg = Color::Reset;
    let mut bg = Color::Reset;
    let mut last_modifier = Modifier::empty();
    for span in content {
        let mut modifier = Modifier::empty();
        modifier.insert(span.style.add_modifier);
        modifier.remove(span.style.sub_modifier);
        if modifier != last_modifier {
            ModifierDiff {
                from: last_modifier,
                to: modifier,
            }
            .queue(&mut writer)?;
            last_modifier = modifier;
        }

        let want_foreground = span.style.fg.unwrap_or(Color::Reset);
        let want_background = span.style.bg.unwrap_or(Color::Reset);
        if want_foreground != fg || want_background != bg {
            let cfg: CColor = want_foreground.into_crossterm();
            let cbg: CColor = want_background.into_crossterm();
            queue!(writer, SetColors(Colors::new(cfg, cbg)))?;
            fg = want_foreground;
            bg = want_background;
        }

        queue!(writer, Print(span.content.clone()))?;
    }

    queue!(
        writer,
        SetForegroundColor(CColor::Reset),
        SetBackgroundColor(CColor::Reset),
        SetAttribute(crossterm::style::Attribute::Reset),
    )
}

#[cfg(test)]
mod tests {
    use ratatui::backend::{ClearType, WindowSize};
    use ratatui::buffer::Cell;
    use ratatui::layout::{Position, Rect, Size};
    use ratatui::style::Style;

    use super::*;

    /// 用 `vt100::Parser` 做真解析的测试后端：`Write::write` 既喂给 parser（还原屏幕状态），
    /// 也记录进 `written`（让测试能直接断言发出的转义字节，比如滚动区是否成对）。
    /// 抄 `codex-rs/tui/src/insert_history.rs:918-981` 附近测试的思路，但不复用它包一层
    /// `CrosstermBackend` 的写法——直接持有 `vt100::Parser`，字节双写更直观。
    struct VT100Backend {
        parser: vt100::Parser,
        size: Size,
        written: Vec<u8>,
    }

    impl VT100Backend {
        fn new(width: u16, height: u16) -> Self {
            Self {
                parser: vt100::Parser::new(height, width, 0),
                size: Size::new(width, height),
                written: Vec::new(),
            }
        }

        fn screen(&self) -> &vt100::Screen {
            self.parser.screen()
        }

        fn written_str(&self) -> String {
            String::from_utf8_lossy(&self.written).into_owned()
        }
    }

    impl Write for VT100Backend {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.written.extend_from_slice(buf);
            self.parser.process(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Backend for VT100Backend {
        type Error = io::Error;

        fn draw<'a, I>(&mut self, _content: I) -> io::Result<()>
        where
            I: Iterator<Item = (u16, u16, &'a Cell)>,
        {
            Ok(())
        }

        fn hide_cursor(&mut self) -> io::Result<()> {
            Ok(())
        }

        fn show_cursor(&mut self) -> io::Result<()> {
            Ok(())
        }

        fn get_cursor_position(&mut self) -> io::Result<Position> {
            let (row, col) = self.screen().cursor_position();
            Ok(Position::new(col, row))
        }

        fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
            let position = position.into();
            write!(
                self,
                "\x1b[{};{}H",
                position.y.saturating_add(1),
                position.x.saturating_add(1)
            )
        }

        fn clear(&mut self) -> io::Result<()> {
            write!(self, "\x1b[2J")
        }

        fn clear_region(&mut self, clear_type: ClearType) -> io::Result<()> {
            let code = match clear_type {
                ClearType::All => "2J",
                ClearType::AfterCursor => "0J",
                ClearType::BeforeCursor => "1J",
                ClearType::CurrentLine => "2K",
                ClearType::UntilNewLine => "0K",
            };
            write!(self, "\x1b[{code}")
        }

        fn size(&self) -> io::Result<Size> {
            Ok(self.size)
        }

        fn window_size(&mut self) -> io::Result<WindowSize> {
            Ok(WindowSize {
                columns_rows: self.size,
                pixels: Size::new(0, 0),
            })
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }

        fn scroll_region_up(&mut self, region: Range<u16>, line_count: u16) -> io::Result<()> {
            if line_count == 0 {
                return Ok(());
            }
            write!(
                self,
                "\x1b[{};{}r\x1b[{line_count}S\x1b[r",
                region.start.saturating_add(1),
                region.end,
            )
        }

        fn scroll_region_down(&mut self, region: Range<u16>, line_count: u16) -> io::Result<()> {
            if line_count == 0 {
                return Ok(());
            }
            write!(
                self,
                "\x1b[{};{}r\x1b[{line_count}T\x1b[r",
                region.start.saturating_add(1),
                region.end,
            )
        }
    }

    /// 统计 `written` 里出现的 `SetScrollRegion` 序列数（`CSI <digits>;<digits> r`），
    /// 与 `ResetScrollRegion`（字面量 `CSI r`）分开计数，不依赖 regex 依赖。
    fn count_set_scroll_region(written: &str) -> usize {
        written
            .split("\x1b[")
            .skip(1)
            .filter(|segment| {
                let mut saw_digit_before_semi = false;
                let mut saw_semi = false;
                let mut saw_digit_after_semi = false;
                for c in segment.chars() {
                    match c {
                        d if d.is_ascii_digit() && !saw_semi => saw_digit_before_semi = true,
                        ';' if !saw_semi => saw_semi = true,
                        d if d.is_ascii_digit() && saw_semi => saw_digit_after_semi = true,
                        'r' => {
                            return saw_digit_before_semi && saw_semi && saw_digit_after_semi;
                        }
                        _ => return false,
                    }
                }
                false
            })
            .count()
    }

    #[test]
    fn mode_selection_downgrades_only_for_zellij_terminal_wrap() {
        // tmux（is_zellij = false）恒定 Standard，不管 wrap policy 是什么。
        assert_eq!(
            insert_history_mode(false, false, HistoryLineWrapPolicy::PreWrap),
            InsertHistoryMode::Standard
        );
        assert_eq!(
            insert_history_mode(false, false, HistoryLineWrapPolicy::Terminal),
            InsertHistoryMode::Standard
        );
        // Zellij + PreWrap：不降级，续行已经被本仓预先切好，DECSTBM 依然安全。
        assert_eq!(
            insert_history_mode(true, false, HistoryLineWrapPolicy::PreWrap),
            InsertHistoryMode::Standard
        );
        // Zellij + Terminal：唯一的降级组合。
        assert_eq!(
            insert_history_mode(true, false, HistoryLineWrapPolicy::Terminal),
            InsertHistoryMode::ZellijRaw
        );
        // 逃生舱：无条件降级，即使不是 Zellij。
        assert_eq!(
            insert_history_mode(false, true, HistoryLineWrapPolicy::PreWrap),
            InsertHistoryMode::ZellijRaw
        );
    }

    #[test]
    fn standard_mode_pairs_scroll_region_and_restores_cursor() {
        let width = 20u16;
        let height = 10u16;
        let backend = VT100Backend::new(width, height);
        // viewport 钉在屏幕最后一行、已触底：这批注入不会移动 area，方便断言只关注字节配对。
        let viewport = Rect::new(0, height - 1, width, 1);
        let cursor_pos = Position::new(4, height - 1);
        let mut terminal = Terminal::with_cursor_position(backend, cursor_pos).expect("terminal");
        terminal.set_viewport_area(viewport);

        let lines = vec![Line::from("alpha"), Line::from("beta")];
        insert_history_lines(
            &mut terminal,
            &lines,
            InsertHistoryMode::Standard,
            HistoryLineWrapPolicy::PreWrap,
        )
        .expect("insert history");

        let written = terminal.backend().written_str();
        let reset_count = written.matches("\x1b[r").count();
        let set_count = count_set_scroll_region(&written);
        assert_eq!(
            set_count, reset_count,
            "SetScrollRegion 与 ResetScrollRegion 必须成对: {written:?}"
        );
        assert!(set_count >= 1, "至少应该发出一组滚动区: {written:?}");

        let expected_restore = format!(
            "\x1b[{};{}H",
            cursor_pos.y.saturating_add(1),
            cursor_pos.x.saturating_add(1)
        );
        assert!(
            written.ends_with(&expected_restore),
            "insert_history_lines 必须以恢复 last_cursor_pos 的 MoveTo 结尾: {written:?}"
        );
    }

    /// 行级样式打底、span 样式覆盖——与 viewport 一侧 `Line` 的渲染语义一致。
    ///
    /// 断言打在合并后的 `Style` 上而不是发出的 SGR 字节：crossterm 尊重 `NO_COLOR`，
    /// 设了该环境变量时颜色序列会渲染成空参数（`ESC[;m`），字节级断言会随环境飘。
    #[test]
    fn span_style_overrides_line_style_in_history() {
        let line = Line::from(vec![
            Span::raw("quoted"),
            Span::styled("hot", Style::default().fg(Color::Red)),
        ])
        .style(Style::default().fg(Color::Green).bg(Color::Black));

        let merged = merge_line_style(&line);
        assert_eq!(merged.len(), 2);

        // 没有自己 fg 的 span 继承行级绿色。
        assert_eq!(merged[0].style.fg, Some(Color::Green));
        // 有自己 fg 的 span 保住红色——方向写反这里会变成 Green。
        assert_eq!(merged[1].style.fg, Some(Color::Red));
        // 两个 span 都继承行级 bg（span 都没设 bg）。
        assert_eq!(merged[0].style.bg, Some(Color::Black));
        assert_eq!(merged[1].style.bg, Some(Color::Black));
    }

    /// 活跃区占满屏幕（`area.top() == 0`）时不能走 DECSTBM：`SetScrollRegion(1..0)`
    /// 会发出 `CSI 1;0 r`，非法参数被终端忽略，后续的历史行就直接打进活跃区。
    #[test]
    fn full_screen_viewport_never_emits_an_invalid_scroll_region() {
        let width = 24u16;
        let height = 6u16;
        let backend = VT100Backend::new(width, height);
        // viewport 占满整屏：上方一行都不剩。
        let viewport = Rect::new(0, 0, width, height);
        let cursor_pos = Position::new(0, 0);
        let mut terminal = Terminal::with_cursor_position(backend, cursor_pos).expect("terminal");
        terminal.set_viewport_area(viewport);

        let lines = vec![Line::from("history-a"), Line::from("history-b")];
        let rows = insert_history_lines(
            &mut terminal,
            &lines,
            InsertHistoryMode::Standard,
            HistoryLineWrapPolicy::PreWrap,
        )
        .expect("insert history");
        assert_eq!(rows, 2);

        let written = terminal.backend().written_str();
        assert!(
            !written.contains("\x1b[1;0r"),
            "绝不能发出非法的 CSI 1;0 r: {written:?}"
        );
        assert_eq!(
            count_set_scroll_region(&written),
            0,
            "顶在 row 0 时整条 DECSTBM 路径都不该走: {written:?}"
        );

        // 改道后仍然满足 cursor-position-neutral。
        let expected_restore = format!(
            "\x1b[{};{}H",
            cursor_pos.y.saturating_add(1),
            cursor_pos.x.saturating_add(1)
        );
        assert!(
            written.ends_with(&expected_restore),
            "改道路径同样必须恢复光标: {written:?}"
        );
    }

    #[test]
    fn standard_mode_leaves_viewport_bytes_untouched() {
        let width = 24u16;
        let height = 12u16;
        let backend = VT100Backend::new(width, height);
        // 同样钉在触底位置，area 在注入前后保持一致，比较才有意义。
        let viewport = Rect::new(0, height - 2, width, 2);
        let cursor_pos = Position::new(0, height - 1);
        let mut terminal = Terminal::with_cursor_position(backend, cursor_pos).expect("terminal");
        terminal.set_viewport_area(viewport);

        // 预先把 composer 内容写进 viewport 行，模拟真实场景下 viewport 已经画过一帧。
        {
            let writer = terminal.backend_mut();
            queue!(
                writer,
                MoveTo(0, viewport.top()),
                Print("composer-marker-one")
            )
            .expect("seed row 0");
            queue!(
                writer,
                MoveTo(0, viewport.top().saturating_add(1)),
                Print("composer-marker-two")
            )
            .expect("seed row 1");
        }

        let all_rows_before: Vec<String> = terminal.backend().screen().rows(0, width).collect();
        let viewport_before: Vec<String> = all_rows_before
            .iter()
            .skip(usize::from(viewport.top()))
            .take(usize::from(viewport.height))
            .cloned()
            .collect();

        let lines = vec![
            Line::from("history line one"),
            Line::from("history line two"),
        ];
        insert_history_lines(
            &mut terminal,
            &lines,
            InsertHistoryMode::Standard,
            HistoryLineWrapPolicy::PreWrap,
        )
        .expect("insert history");

        // Standard 理论上可能把 viewport 往下推；重新取更新后的 area 保证比较同一批内容。
        let after_area = terminal.viewport_area();
        let all_rows_after: Vec<String> = terminal.backend().screen().rows(0, width).collect();
        let viewport_after: Vec<String> = all_rows_after
            .iter()
            .skip(usize::from(after_area.top()))
            .take(usize::from(after_area.height))
            .cloned()
            .collect();

        assert_eq!(
            viewport_before, viewport_after,
            "viewport 内容在 Standard 注入前后必须逐字节一致"
        );
    }

    #[test]
    fn zellij_raw_mode_skips_scroll_region_and_leading_newline() {
        let width = 24u16;
        let height = 8u16;
        let backend = VT100Backend::new(width, height);
        let viewport = Rect::new(0, height - 2, width, 2);
        let cursor_pos = Position::new(0, height - 1);
        let mut terminal = Terminal::with_cursor_position(backend, cursor_pos).expect("terminal");
        terminal.set_viewport_area(viewport);

        let lines = vec![Line::from("first line"), Line::from("second line")];
        insert_history_lines(
            &mut terminal,
            &lines,
            InsertHistoryMode::ZellijRaw,
            HistoryLineWrapPolicy::Terminal,
        )
        .expect("insert zellij raw history");

        let written = terminal.backend().written_str();
        assert_eq!(
            count_set_scroll_region(&written),
            0,
            "ZellijRaw 不应该发 SetScrollRegion: {written:?}"
        );
        assert_eq!(
            written.matches("\x1b[r").count(),
            0,
            "ZellijRaw 不应该发 ResetScrollRegion: {written:?}"
        );

        // 首行绝对定位到 viewport.top() 之后应该紧跟着行内容（可能先有 SGR 序列），
        // 不能先来一个 "\r\n"——那是 codex 用来区分"首行"和"续行"的标记。
        let move_to_top = format!("\x1b[{};1H", viewport.top().saturating_add(1));
        let after_move = written.split_once(move_to_top.as_str()).map_or_else(
            || panic!("expected initial MoveTo to viewport top: {written:?}"),
            |(_, rest)| rest,
        );
        assert!(
            !after_move.starts_with("\r\n"),
            "ZellijRaw 首行前不应该有前导 \\r\\n: {written:?}"
        );
    }

    #[test]
    fn returns_actual_wrapped_row_count_for_a_wide_line() {
        let width = 10u16;
        let height = 6u16;
        let backend = VT100Backend::new(width, height);
        let viewport = Rect::new(0, height - 1, width, 1);
        let cursor_pos = Position::new(0, height - 1);
        let mut terminal = Terminal::with_cursor_position(backend, cursor_pos).expect("terminal");
        terminal.set_viewport_area(viewport);

        // 35 个等宽字符，在宽度 10 下应占 ceil(35 / 10) = 4 个物理行。
        let wide = "a".repeat(35);
        let lines = vec![Line::from(wide)];
        let rows = insert_history_lines(
            &mut terminal,
            &lines,
            InsertHistoryMode::Standard,
            HistoryLineWrapPolicy::PreWrap,
        )
        .expect("insert wide history line");

        assert_eq!(rows, 4, "35 个等宽字符在宽度 10 下应占 4 个物理行");
        assert_eq!(terminal.visible_history_rows(), 4);
    }

    #[test]
    fn empty_batch_emits_nothing_and_skips_row_bookkeeping() {
        let width = 20u16;
        let height = 8u16;
        let backend = VT100Backend::new(width, height);
        let viewport = Rect::new(0, height - 1, width, 1);
        let cursor_pos = Position::new(0, height - 1);
        let mut terminal = Terminal::with_cursor_position(backend, cursor_pos).expect("terminal");
        terminal.set_viewport_area(viewport);

        let rows = insert_history_lines(
            &mut terminal,
            &[],
            InsertHistoryMode::Standard,
            HistoryLineWrapPolicy::PreWrap,
        )
        .expect("insert empty history");

        assert_eq!(rows, 0);
        assert_eq!(terminal.visible_history_rows(), 0);
        assert!(
            terminal.backend().written_str().is_empty(),
            "空批次不应该发出任何字节"
        );
    }
}
