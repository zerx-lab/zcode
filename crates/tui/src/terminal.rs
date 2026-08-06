// 本文件 fork 自 codex-rs 的 `custom_terminal.rs`
// （C:/Users/zero/Desktop/code/github/codex/codex-rs/tui/src/custom_terminal.rs），
// 而 codex 的该文件又派生自 `ratatui::Terminal`，遵循以下 ratatui 的 MIT 许可声明：
//
// The MIT License (MIT)
// Copyright (c) 2016-2022 Florian Dehau
// Copyright (c) 2023-2025 The Ratatui Developers
//
// Permission is hereby granted, free of charge, to any person obtaining a copy
// of this software and associated documentation files (the "Software"), to deal
// in the Software without restriction, including without limitation the rights
// to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
// copies of the Software, and to permit persons to whom the Software is
// furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in all
// copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
// OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
// SOFTWARE.

//! fork 自 `ratatui::Terminal` 的双缓冲差分渲染器。
//!
//! 与上游 `ratatui::Terminal` 的关键差异：viewport 是运行时可变的 [`Rect`]（inline 模式下
//! 随内容增长/收缩），且 [`Terminal::backend_mut`] 把裸 writer 暴露给调用方，供
//! [`crate::insert_history`] 之类的模块绕开双缓冲直接发相对定位的历史注入字节。
//!
//! # 不变量 2：普通增量帧零绝对定位
//!
//! `draw` 与 [`Terminal::move_cursor_relative`] 只发 `MoveUp`/`MoveDown`/`MoveLeft`/`MoveRight`
//! 四种相对光标移动（`ESC[{n}A/B/C/D`），绝不发 CUP（`ESC[{row};{col}H`）或 CHA
//! （`ESC[{n}G`）。这是本 crate 五条不变量的第二条，证据链：
//!
//! - `plans/tui/README.md:88` ——“普通增量帧零绝对定位。in-window diff 与 scroll-append
//!   的窗口重绘只用相对光标移动；`MoveTo(0,0)` / `Clear` 会把已向上滚动的读者猛拽回底部”；
//! - `plans/tui/architecture.md:59,64` —— in-window diff 一栏标注“**相对**光标移动”，
//!   且“普通 update 路径永不发 ED2/ED3 或绝对光标 home”；
//! - `oh-my-pi/docs/tui-core-renderer.md:119-120`、
//!   `oh-my-pi/packages/tui/src/tui.ts:4007-4010`（“the live window repaints in place with
//!   relative moves. This path never emits ED2/ED3 or an absolute cursor home”）、
//!   `:4130-4133`（用 `\x1b[{n}A` 而非绝对 home）、`:4135-4137`（行方向用
//!   `\x1b[{n}B`/`\x1b[{n}A`）。
//!
//! codex 原文在这一点上用的是绝对 `MoveTo(x, y)`（`custom_terminal.rs:676-678`），与本仓
//! 事实源冲突，此处**不**照抄，改为下方的相对定位实现。oh-my-pi 的整行重写只需要 `\r`
//! 回到列 0（`tui.ts:4139`），但本仓 `diff_buffers` 是逐 cell 粒度的差分，行内起点可以落在
//! 任意列，因此列方向同样必须相对移动（`MoveRight`/`MoveLeft`），不能退化成 CHA 或 `\r`。

use std::io;
use std::io::Write;

use crossterm::cursor::{MoveDown, MoveLeft, MoveRight, MoveUp, SetCursorStyle};
use crossterm::queue;
use crossterm::style::{
    Colors, Print, SetAttribute, SetBackgroundColor, SetColors, SetForegroundColor,
};
use crossterm::terminal::Clear;
use ratatui::backend::{Backend, ClearType, IntoCrossterm};
use ratatui::buffer::{Buffer, Cell, CellDiffOption};
use ratatui::layout::{Position, Rect, Size};
use ratatui::style::{Color, Modifier};
use ratatui::widgets::{Widget, WidgetRef};

/// 单元格符号的可见显示宽度。
///
/// codex 原文（`custom_terminal.rs:57-80`）手写剥离 OSC 序列再调用
/// `UnicodeWidthStr::width`；`zcode_text::width::visible_width` 本身就是 ANSI/OSC 感知的
/// 扫描器（含 OSC8 超链接，见 `crates/text/src/width.rs:127-129` 的模块文档），直接调用
/// 即可，不需要在这里重复一遍剥离逻辑（渲染侧另写一份宽度计算是
/// `rule://zcode-architecture`「TUI 输出清理」明确禁止的）。
fn display_width(symbol: &str) -> usize {
    zcode_text::width::visible_width(symbol)
}

/// 从一个单元格符号里拆出 OSC8 超链接的 `(目标 URI, 可见文本)`。
///
/// 取的是超链接的 payload，不是宽度，因此不经 `zcode_text`。原文
/// `custom_terminal.rs:82-91`，这里只是把 `&str` 直接切片换成 `.get()`
/// （本仓 `clippy::indexing_slicing` 是 deny 级，禁止裸 `&s[a..b]`）。
fn osc8_hyperlink_parts(symbol: &str) -> Option<(&str, &str)> {
    let content = symbol.strip_prefix("\x1b]8;;")?;
    let destination_end = content.find('\x07')?;
    let destination = content.get(..destination_end)?;
    if destination.is_empty() {
        return None;
    }
    let rest = content.get(destination_end + 1..)?;
    let visible = rest.strip_suffix("\x1b]8;;\x07")?;
    Some((destination, visible))
}

/// 单帧渲染句柄：持有本帧要写入的 back buffer 与视口尺寸。
///
/// 渲染闭包结束后，[`Terminal::try_draw`] 取出 `cursor_position`/`cursor_style` 决定下一步
/// 是隐藏光标还是（用相对移动）挪过去并设样式；`Frame` 本身随闭包结束析构，不跨帧存活。
#[derive(Debug)]
pub struct Frame<'a> {
    /// 本帧结束后光标应处的位置；`None` 表示隐藏光标。
    cursor_position: Option<Position>,
    /// 本帧结束后应用的光标形状。
    cursor_style: SetCursorStyle,
    /// 视口区域，渲染期间保持不变。
    viewport_area: Rect,
    /// 本帧写入的 buffer。
    buffer: &'a mut Buffer,
}

impl Frame<'_> {
    /// 当前帧的区域。渲染期间保证不变，可重复调用。
    #[must_use]
    pub const fn area(&self) -> Rect {
        self.viewport_area
    }

    /// 渲染一个取得所有权的 [`Widget`]。
    pub fn render_widget<W: Widget>(&mut self, widget: W, area: Rect) {
        widget.render(area, self.buffer);
    }

    /// 渲染一个 [`WidgetRef`]。
    // `WidgetRef::render_ref` 只需要 `&widget`，但按值接收让调用点不必操心借用，
    // 与上面 `render_widget` 的签名保持对称；ratatui 自己的 `render_widget_ref` 也是
    // 同样取舍（见 codex 原文 `custom_terminal.rs:122-129`）。
    #[allow(clippy::needless_pass_by_value)]
    pub fn render_widget_ref<W: WidgetRef>(&mut self, widget: W, area: Rect) {
        widget.render_ref(area, self.buffer);
    }

    /// 本帧结束后把光标移到 `position` 并置为可见；不调用则光标隐藏。
    pub fn set_cursor_position<P: Into<Position>>(&mut self, position: P) {
        self.cursor_position = Some(position.into());
    }

    /// 本帧结束后应用的光标形状。
    pub fn set_cursor_style(&mut self, style: SetCursorStyle) {
        self.cursor_style = style;
    }

    /// 可变借用本帧的 buffer，供自定义渲染逻辑直接写单元格。
    pub fn buffer_mut(&mut self) -> &mut Buffer {
        self.buffer
    }
}

/// fork 自 `ratatui::Terminal` 的双缓冲差分渲染器，见模块文档。
///
/// # 双缓冲布局
///
/// 用两个具名字段 `front`/`back` 代替 codex 原文的 `buffers: [Buffer; 2]` + `current: usize`
/// （`custom_terminal.rs:163-167`）：后者每次访问都要下标 `buffers[self.current]`，被本仓
/// `clippy::indexing_slicing`（deny）拒绝。具名字段零开销地表达同一套 ping-pong 语义，见
/// `Terminal::swap_buffers`。
#[derive(Debug)]
pub struct Terminal<B>
where
    B: Backend<Error = io::Error> + Write,
{
    backend: B,
    /// 本帧渲染目标 buffer（对应 codex 的 `buffers[current]`）。
    front: Buffer,
    /// 上一帧内容，是 `diff_buffers` 的比较基线（对应 codex 的 `buffers[1 - current]`）。
    back: Buffer,
    /// 光标当前是否处于隐藏状态；`Drop` 靠它决定退出前要不要恢复可见。
    hidden_cursor: bool,
    viewport_area: Rect,
    last_known_screen_size: Size,
    last_known_cursor_pos: Position,
    /// inline 模式下、viewport 上方已经写入的历史行数；被 clamp 在 `viewport_area.top()` 之内。
    visible_history_rows: u16,
}

impl<B> Drop for Terminal<B>
where
    B: Backend<Error = io::Error> + Write,
{
    fn drop(&mut self) {
        // codex 原文用 `eprintln!`（`custom_terminal.rs:192,198`）；本仓渲染路径禁止直写
        // stderr（会和 TUI 输出打架），一律走 `tracing`。
        if let Err(err) = self.reset_cursor_style() {
            tracing::warn!(error = %err, "重置光标样式失败");
        }

        if self.hidden_cursor
            && let Err(err) = self.show_cursor()
        {
            tracing::warn!(error = %err, "恢复光标可见性失败");
        }
    }
}

/// [`Terminal::new`] 的光标探测：Unix 走有界的 `terminal_probe`，Windows 走后端。
///
/// 拿不到位置一律返回 `None` 交给调用方兜底；探测失败不是错误
/// （不支持 CPR 的终端很常见，`plans/tui/platform.md:203`）。
#[cfg(unix)]
fn probe_initial_cursor<B>(_backend: &mut B) -> Option<Position>
where
    B: Backend<Error = io::Error> + Write,
{
    match crate::terminal_probe::cursor_position(crate::terminal_probe::DEFAULT_TIMEOUT) {
        Ok(position) => position,
        Err(err) => {
            tracing::warn!(error = %err, "CSI 6n 光标探测失败");
            None
        }
    }
}

/// 见 `#[cfg(unix)]` 版本的文档。
#[cfg(not(unix))]
fn probe_initial_cursor<B>(backend: &mut B) -> Option<Position>
where
    B: Backend<Error = io::Error> + Write,
{
    match backend.get_cursor_position() {
        Ok(position) => Some(position),
        Err(err) => {
            tracing::warn!(error = %err, "后端光标位置查询失败");
            None
        }
    }
}

impl<B> Terminal<B>
where
    B: Backend<Error = io::Error> + Write,
{
    /// 用后端当前状态创建终端：查询屏幕尺寸，并探测光标位置以确定 inline viewport 的锚点。
    ///
    /// 光标探测按平台走**不同**的路径，这不是可选优化：
    ///
    /// - Unix：走 `terminal_probe::cursor_position`（`cfg(unix)`）。**绝不**用
    ///   `Backend::get_cursor_position()`——`CrosstermBackend` 的实现会自己读 stdin 等 CPR
    ///   回复，没有 100ms 预算、不过滤交错的 focus report、也不与事件流互斥；
    ///   `plans/tui/platform.md:193-207` 逐条列出了这些约束各自对应的故障，
    ///   所以 codex 才另写了一个 probe 模块。
    /// - Windows：用 `Backend::get_cursor_position()` 即可。没有 Ctrl-Z，这一次探测发生在
    ///   事件流启动之前，不存在争抢（`plans/tui/platform.md:211`）。
    ///
    /// **前提**：调用方此刻不能有正在读 stdin 的事件流。启动阶段天然满足；
    /// 之后要重新锚定请走 `job_control::suspend`（`cfg(unix)`），它会先暂停事件流。
    ///
    /// 部分 PTY 根本不响应 CPR：探测拿不到位置时退回原点并记一条警告，不让 TUI 启动失败
    /// （对应 codex `custom_terminal.rs:209-222`）。
    pub fn new(mut backend: B) -> io::Result<Self> {
        let screen_size = backend.size()?;
        let cursor_pos = probe_initial_cursor(&mut backend).unwrap_or_else(|| {
            tracing::warn!("初始光标位置探测无响应，退回原点");
            Position { x: 0, y: 0 }
        });
        Ok(Self::from_parts(backend, screen_size, cursor_pos))
    }

    /// 用调用方已知的光标位置创建终端，跳过后端探测。
    ///
    /// 供启动阶段已经用有界的 `terminal_probe::cursor_position` 探测过一次的调用方使用：
    /// 传入陈旧或合成的位置会改变 inline viewport 的锚点，调用方必须确保这里传入的兜底值
    /// 与自己选择的探测失败兜底一致（对应 codex `custom_terminal.rs:224-237`）。
    pub fn with_cursor_position(backend: B, cursor_pos: Position) -> io::Result<Self> {
        let screen_size = backend.size()?;
        Ok(Self::from_parts(backend, screen_size, cursor_pos))
    }

    /// 绕过 `backend.size()` 查询直接构造。
    ///
    /// 供测试（尺寸固定的 fake backend）与已经在别处确定了终端尺寸的宿主使用。codex 原文
    /// 用 `#[cfg(test)]` 私有构造器 + 影子字段（`custom_terminal.rs:263-273`）实现同样的
    /// 效果，但那只在 crate 内部的 `#[cfg(test)] mod tests` 可见；本仓的集成测试跑在
    /// crate 外的 `tests/` 目录，`#[cfg(test)]` 够不着，因此改成无条件公开的构造函数。
    pub fn with_screen_size(backend: B, screen_size: Size, cursor_pos: Position) -> Self {
        Self::from_parts(backend, screen_size, cursor_pos)
    }

    fn from_parts(backend: B, screen_size: Size, cursor_pos: Position) -> Self {
        Self {
            backend,
            front: Buffer::empty(Rect::ZERO),
            back: Buffer::empty(Rect::ZERO),
            hidden_cursor: false,
            viewport_area: Rect::new(
                /* x */ 0,
                cursor_pos.y,
                /* width */ 0,
                /* height */ 0,
            ),
            last_known_screen_size: screen_size,
            last_known_cursor_pos: cursor_pos,
            visible_history_rows: 0,
        }
    }

    /// 构造供渲染的 [`Frame`]，绑定到 `front`（本帧渲染目标）。
    fn frame(&mut self) -> Frame<'_> {
        Frame {
            cursor_position: None,
            cursor_style: SetCursorStyle::DefaultUserShape,
            viewport_area: self.viewport_area,
            buffer: &mut self.front,
        }
    }

    fn current_buffer(&self) -> &Buffer {
        &self.front
    }

    fn current_buffer_mut(&mut self) -> &mut Buffer {
        &mut self.front
    }

    fn previous_buffer(&self) -> &Buffer {
        &self.back
    }

    fn previous_buffer_mut(&mut self) -> &mut Buffer {
        &mut self.back
    }

    /// 只读借用后端。
    pub const fn backend(&self) -> &B {
        &self.backend
    }

    /// 可变借用后端，供 [`crate::insert_history`] 之类需要绕过双缓冲直接发字节的调用方使用。
    pub fn backend_mut(&mut self) -> &mut B {
        &mut self.backend
    }

    /// 当前视口区域。
    pub const fn viewport_area(&self) -> Rect {
        self.viewport_area
    }

    /// 设置视口区域：同步 resize 双缓冲，并把 `visible_history_rows` clamp 到新的
    /// `area.top()`——viewport 上移后，"已经写入的历史行数" 不能超过新的可用空间。
    pub fn set_viewport_area(&mut self, area: Rect) {
        self.current_buffer_mut().resize(area);
        self.previous_buffer_mut().resize(area);
        self.viewport_area = area;
        self.visible_history_rows = self.visible_history_rows.min(area.top());
    }

    /// 上一次落定的光标位置，由内部差分绘制、[`Terminal::set_cursor_position`]、
    /// [`Terminal::move_cursor_relative`] 与 [`Terminal::clear_visible_screen`] 维护。
    pub const fn last_known_cursor_pos(&self) -> Position {
        self.last_known_cursor_pos
    }

    /// 直接改写缓存的光标位置，不发任何字节。
    ///
    /// 供 `job_control::suspend` 之类在 `SIGCONT` 后重新探测到真实光标位置时同步缓存使用。
    pub fn set_last_known_cursor_pos(&mut self, pos: Position) {
        self.last_known_cursor_pos = pos;
    }

    /// 缓存的最后一次已知屏幕尺寸。
    pub const fn last_known_screen_size(&self) -> Size {
        self.last_known_screen_size
    }

    /// 查询后端的真实屏幕尺寸。
    pub fn screen_size(&self) -> io::Result<Size> {
        self.backend.size()
    }

    fn resize(&mut self, screen_size: Size) {
        self.last_known_screen_size = screen_size;
    }

    /// 查询后端尺寸；变了就更新缓存并返回 `true`。
    pub fn autoresize(&mut self) -> io::Result<bool> {
        let screen_size = self.screen_size()?;
        if screen_size == self.last_known_screen_size {
            return Ok(false);
        }
        self.resize(screen_size);
        Ok(true)
    }

    /// 绘制一帧。`render` 必须完整重绘整个视口（包括与上一帧相比未变的区域），因为
    /// `flush` 靠逐 cell 比较两帧来决定发哪些字节；`render` 若只画了变化的部分，
    /// 下一帧的 diff 基线就是错的。
    pub fn draw<F>(&mut self, render: F) -> io::Result<()>
    where
        F: FnOnce(&mut Frame<'_>),
    {
        self.try_draw(|frame| {
            render(frame);
            io::Result::Ok(())
        })
    }

    /// [`Terminal::draw`] 的可失败版本：`render` 返回 `Result`，出错时通过 `?` 传播，
    /// 且不更新 back buffer（下一次 `draw`/`try_draw` 仍以旧帧为基线重试）。
    pub fn try_draw<F, E>(&mut self, render: F) -> io::Result<()>
    where
        F: FnOnce(&mut Frame<'_>) -> Result<(), E>,
        E: Into<io::Error>,
    {
        // 先 autoresize，否则收缩时会越界、增长时 widget 与终端实际尺寸失步。
        self.autoresize()?;

        let mut frame = self.frame();
        render(&mut frame).map_err(Into::into)?;

        let cursor_position = frame.cursor_position;
        let cursor_style = frame.cursor_style;

        self.flush()?;

        match cursor_position {
            None => self.hide_cursor()?,
            Some(position) => {
                self.set_cursor_style(cursor_style)?;
                self.show_cursor()?;
                // 普通增量帧的硬件光标定位同样要遵守不变量 2：只用相对移动，
                // 不调用下面的绝对 `set_cursor_position`（模块文档有完整证据链）。
                self.move_cursor_relative(position)?;
            }
        }

        self.swap_buffers();

        Backend::flush(&mut self.backend)?;

        Ok(())
    }

    /// 计算并写出 `back`（上一帧）到 `front`（本帧）的差分字节。
    fn flush(&mut self) -> io::Result<()> {
        let updates = diff_buffers(self.previous_buffer(), self.current_buffer());
        let mut cursor = self.last_known_cursor_pos;
        draw(&mut self.backend, updates.into_iter(), &mut cursor)?;
        self.last_known_cursor_pos = cursor;
        Ok(())
    }

    /// 隐藏光标。
    pub fn hide_cursor(&mut self) -> io::Result<()> {
        self.backend.hide_cursor()?;
        self.hidden_cursor = true;
        Ok(())
    }

    /// 显示光标。
    pub fn show_cursor(&mut self) -> io::Result<()> {
        self.backend.show_cursor()?;
        self.hidden_cursor = false;
        Ok(())
    }

    /// 设置可见的终端光标形状。
    pub fn set_cursor_style(&mut self, style: SetCursorStyle) -> io::Result<()> {
        queue!(self.backend, style)
    }

    /// 恢复用户配置的默认终端光标形状。
    pub fn reset_cursor_style(&mut self) -> io::Result<()> {
        self.set_cursor_style(SetCursorStyle::DefaultUserShape)
    }

    /// 用**绝对** `MoveTo` 把光标钉到 `position`。
    ///
    /// 只许 `full_paint`（全量重绘手势）与历史注入路径（[`crate::insert_history`]，
    /// cursor-position-neutral：进函数存光标、出函数绝对恢复）调用；普通增量帧调用它
    /// 就违反不变量 2，见模块文档的证据链。普通增量帧的光标定位一律走
    /// [`Terminal::move_cursor_relative`]。
    pub fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        let position = position.into();
        self.backend.set_cursor_position(position)?;
        self.last_known_cursor_pos = position;
        Ok(())
    }

    /// 用**相对**光标移动把光标从 [`Terminal::last_known_cursor_pos`] 挪到 `position`，
    /// 并更新缓存。普通增量帧（in-window diff、scroll-append 收尾时定位硬件光标）唯一允许
    /// 的定位方式；实现见自由函数 `move_relative`。
    pub fn move_cursor_relative<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        let target = position.into();
        let mut cursor = self.last_known_cursor_pos;
        move_relative(&mut self.backend, &mut cursor, target)?;
        self.last_known_cursor_pos = cursor;
        Ok(())
    }

    /// 清视口并强制下一帧全量重绘（对应 codex 的 `clear`，`custom_terminal.rs:499-504`）。
    pub fn clear_viewport(&mut self) -> io::Result<()> {
        if self.viewport_area.is_empty() {
            return Ok(());
        }
        self.clear_after_position(self.viewport_area.as_position())
    }

    /// 从 `position` 清到可见屏幕末尾（`ClearType::AfterCursor`），并重置 back buffer
    /// 强制下一帧全量重绘。
    ///
    /// 这是一次**绝对**定位，只许 full paint 与历史注入路径调用（不变量 2/3）。
    /// 必须同步 `last_known_cursor_pos`：codex 原文没这一步也能活，是因为它的 `draw`
    /// 每个命令都发绝对 `MoveTo`；本 fork 的 `flush` / `move_cursor_relative` 完全靠
    /// 这份账本算相对位移，漏更新就会让下一帧从陈旧坐标起算，整屏打歪。
    pub fn clear_after_position(&mut self, position: Position) -> io::Result<()> {
        self.backend.set_cursor_position(position)?;
        self.backend.clear_region(ClearType::AfterCursor)?;
        self.last_known_cursor_pos = position;
        self.previous_buffer_mut().reset();
        Ok(())
    }

    /// 只重置 back buffer，让下一帧完整重画视口；不发任何字节。
    ///
    /// 供调用方在用裸 writer（[`Terminal::backend_mut`]）做了 ratatui 不知情的原始终端
    /// 操作（例如历史注入的滚动区字节）之后，让下一帧的 diff 不因为 stale 的 back buffer
    /// 而漏发本该重画的单元格。
    pub fn invalidate_viewport(&mut self) {
        self.previous_buffer_mut().reset();
    }

    /// 全量重绘用的清屏：复位滚动区 + SGR、归位光标、ED2、再归位光标，一次性写出后
    /// `Write::flush`，并同步内部状态（历史行数清零、back buffer 强制下一帧全量重绘、
    /// 光标缓存归零）。
    ///
    /// 只许 full paint 路径（[`crate::emit::Emitter`] 的 full paint 分支）调用：清可见屏幕和归位
    /// 都是不变量 2 明确禁止普通增量帧使用的绝对操作。**不碰 scrollback**——ED3
    /// （`CSI 3J`）不在这里发；全 crate 唯一的 ED3 callsite 在 `emit.rs` 的 full-paint
    /// 分支里，对应不变量 1，本方法不做任何越权的事。
    pub fn clear_visible_screen(&mut self) -> io::Result<()> {
        // `\x1b[r` 复位滚动区：上一次 `insert_history` 的 DECSTBM 若被信号/panic 打断，
        // 滚动区可能仍然生效，后续所有相对光标移动都会被限制在残留区间里。
        // `\x1b[0m` 复位 SGR。两次 home 夹 ED2 抄的是 codex `custom_terminal.rs:525-527`
        // 的理由：部分终端（尤其 Terminal.app）在 ED2 前后各配一次显式 cursor-home 才稳定，
        // 与常见 shell `clear` 序列（`CSI 2J` + `CSI H`）一致。
        write!(self.backend, "\x1b[r\x1b[0m\x1b[H\x1b[2J\x1b[H")?;
        Write::flush(&mut self.backend)?;
        // `draw`/`move_relative` 全部是相对定位；不重置这里的话，清屏后第一帧的相对移动
        // 会从清屏前的旧光标位置算起，落点全错——codex 原文的 `clear_visible_screen`
        // 没设它（只在 `clear_scrollback_and_visible_screen_ansi` 里设），这是本仓相对
        // 定位模型下必须补上的一处。
        self.last_known_cursor_pos = Position { x: 0, y: 0 };
        self.visible_history_rows = 0;
        self.previous_buffer_mut().reset();
        Ok(())
    }

    /// 记录 inline viewport 上方新写入的历史行数，clamp 在当前 `viewport_area.top()` 之内。
    pub fn note_history_rows_inserted(&mut self, rows: u16) {
        self.visible_history_rows = self
            .visible_history_rows
            .saturating_add(rows)
            .min(self.viewport_area.top());
    }

    /// 已记录的、viewport 上方可见的历史行数。
    pub const fn visible_history_rows(&self) -> u16 {
        self.visible_history_rows
    }

    /// ping-pong 交换两块 buffer：先重置 back（它是本帧要覆写的旧帧），再与 front 互换。
    /// 交换后新 front（刚被重置过）是下一帧的渲染目标，新 back（本帧刚画好的内容）是下一次
    /// `diff_buffers` 的比较基线——语义与 codex 用下标翻转 `current = 1 - current` 完全一致
    /// （`custom_terminal.rs:564-567`），只是用 `mem::swap` 表达，不触碰 `clippy::indexing_slicing`。
    fn swap_buffers(&mut self) {
        self.back.reset();
        std::mem::swap(&mut self.front, &mut self.back);
    }
}

/// 一条差分绘制指令：要么原样打印一个单元格，要么用 EL 清到行尾。
#[derive(Debug)]
enum DrawCommand {
    Put { x: u16, y: u16, cell: Cell },
    ClearToEnd { x: u16, y: u16, bg: Color },
}

#[cfg(test)]
impl DrawCommand {
    /// 手写替代 `derive_more::IsVariant`（本仓未引入该依赖）。只有测试需要区分变体：
    /// 产品代码里 `draw` 对两种变体的处理是同一个 `match`。
    const fn is_put(&self) -> bool {
        matches!(self, Self::Put { .. })
    }
}

/// 比较 `a`（上一帧）与 `b`（本帧），生成把终端从 `a` 更新到 `b` 所需的最小指令序列。
///
/// 对应 codex `custom_terminal.rs:587-650`；行为不变，只是把裸切片/裸下标换成 `.get()`，
/// 把 `usize`/`u16` 互转换成 `usize::from`/`u16::try_from`（本仓 `as_conversions`、
/// `indexing_slicing` 均为 deny）。
fn diff_buffers(a: &Buffer, b: &Buffer) -> Vec<DrawCommand> {
    let previous_buffer = &a.content;
    let next_buffer = &b.content;

    let height = usize::from(a.area.height);
    let width = usize::from(a.area.width);

    let mut updates = vec![];
    let mut last_nonblank_columns = vec![0u16; height];
    for y in 0..a.area.height {
        let row_start = usize::from(y) * width;
        let row_end = row_start + width;
        let Some(row) = next_buffer.get(row_start..row_end) else {
            // area.height/width 与 content.len() 由 `Buffer::resize` 恒同步维护；拿不到
            // 说明缓冲区被破坏，宁可跳过这一行也不索引越界。
            continue;
        };
        let bg = row.last().map_or(Color::Reset, |cell| cell.bg);

        // 扫描整行找出最右侧"仍然重要"的列：非空格字形、背景色与行尾背景不同、或带修饰符
        // 的单元格；宽字形把这个边界延伸到它完整显示宽度覆盖的最后一列。这个边界之后的部分
        // 用一条 `ClearToEnd` 清掉，比逐格发空格 `Put` 更省字节。
        let mut last_nonblank_column = 0usize;
        let mut column = 0usize;
        while column < row.len() {
            let Some(cell) = row.get(column) else {
                break;
            };
            let glyph_width = display_width(cell.symbol());
            if cell.symbol() != " " || cell.bg != bg || cell.modifier != Modifier::empty() {
                last_nonblank_column = column + glyph_width.saturating_sub(1);
            }
            column += glyph_width.max(1); // 零宽符号按 1 列算，保证扫描前进
        }

        if last_nonblank_column + 1 < row.len() {
            let (x, y) = a.pos_of(row_start + last_nonblank_column + 1);
            updates.push(DrawCommand::ClearToEnd { x, y, bg });
        }

        let last_nonblank_column_u16 = u16::try_from(last_nonblank_column).unwrap_or(u16::MAX);
        if let Some(slot) = last_nonblank_columns.get_mut(usize::from(y)) {
            *slot = last_nonblank_column_u16;
        }
    }

    // 因覆盖/替换前一个宽字符而失效的单元格数。
    let mut invalidated: usize = 0;
    // 因为前面的宽字符占位（这些格子本应是空白）或逐格跳过而要跳过的单元格数。
    let mut to_skip: usize = 0;
    for (i, (current, previous)) in next_buffer.iter().zip(previous_buffer.iter()).enumerate() {
        let is_skip = matches!(current.diff_option, CellDiffOption::Skip);
        if !is_skip && (current != previous || invalidated > 0) && to_skip == 0 {
            let (x, y) = a.pos_of(i);
            let row = i / width;
            let row_limit = last_nonblank_columns.get(row).copied().unwrap_or(0);
            if x <= row_limit {
                updates.push(DrawCommand::Put {
                    x,
                    y,
                    cell: current.clone(),
                });
            }
        }

        to_skip = display_width(current.symbol()).saturating_sub(1);

        let affected_width = display_width(current.symbol()).max(display_width(previous.symbol()));
        invalidated = affected_width.max(invalidated).saturating_sub(1);
    }
    updates
}

/// 把 `cursor` 移到 `target`：行方向 `MoveUp`/`MoveDown`（`ESC[{n}A`/`ESC[{n}B`），
/// 列方向 `MoveRight`/`MoveLeft`（`ESC[{n}C`/`ESC[{n}D`），**不发** CUP、也不发 CHA。
///
/// 顺序先行后列。这是不变量 2 的核心实现，详细证据链见模块文档；本仓的差分粒度是
/// 逐 cell 而非整行重写，所以列方向也必须相对移动，不能像 oh-my-pi 整行重写那样单纯靠
/// `\r` 回到列 0。
fn move_relative(
    writer: &mut impl Write,
    cursor: &mut Position,
    target: Position,
) -> io::Result<()> {
    match target.y.cmp(&cursor.y) {
        std::cmp::Ordering::Greater => queue!(writer, MoveDown(target.y - cursor.y))?,
        std::cmp::Ordering::Less => queue!(writer, MoveUp(cursor.y - target.y))?,
        std::cmp::Ordering::Equal => {}
    }
    match target.x.cmp(&cursor.x) {
        std::cmp::Ordering::Greater => queue!(writer, MoveRight(target.x - cursor.x))?,
        std::cmp::Ordering::Less => queue!(writer, MoveLeft(cursor.x - target.x))?,
        std::cmp::Ordering::Equal => {}
    }
    *cursor = target;
    Ok(())
}

/// 把 `commands` 写成字节序列，`cursor` 既是相对移动的起点也在过程中被持续更新，
/// 函数返回时其值必须等于终端上真实的光标位置。
///
/// 对应 codex `custom_terminal.rs:652-729`；光标定位部分被替换成 [`move_relative`]
/// （见模块文档的不变量 2 证据链），其余的样式/超链接批处理逻辑照抄。
fn draw<I>(writer: &mut impl Write, commands: I, cursor: &mut Position) -> io::Result<()>
where
    I: Iterator<Item = DrawCommand>,
{
    let mut fg = Color::Reset;
    let mut bg = Color::Reset;
    let mut modifier = Modifier::empty();
    let mut active_hyperlink: Option<String> = None;
    for command in commands {
        let (x, y) = match &command {
            DrawCommand::Put { x, y, .. } | DrawCommand::ClearToEnd { x, y, .. } => (*x, *y),
        };
        let hyperlink = match &command {
            DrawCommand::Put { cell, .. } => osc8_hyperlink_parts(cell.symbol()),
            DrawCommand::ClearToEnd { .. } => None,
        };
        let destination = hyperlink.map(|(destination, _)| destination);
        let hyperlink_changed = active_hyperlink.as_deref() != destination;
        if hyperlink_changed && active_hyperlink.is_some() {
            queue!(writer, Print("\x1b]8;;\x07"))?;
        }

        // 不发这次移动 <=> 目标恰好紧接在上一次打印之后（cursor 已经在那）——这条优化是
        // 不变量 2 真正省字节的地方；`cursor` 由本函数按实际打印宽度精确推进，见下方
        // `Put` 分支，因此这个比较对宽字符同样准确（不再依赖 codex 原文假设宽度为 1 的
        // `x == p.x + 1` 判断，`custom_terminal.rs:676`）。
        if x != cursor.x || y != cursor.y {
            move_relative(writer, cursor, Position { x, y })?;
        }

        match &command {
            DrawCommand::Put { cell, .. } => {
                if cell.modifier != modifier {
                    let diff = ModifierDiff {
                        from: modifier,
                        to: cell.modifier,
                    };
                    diff.queue(writer)?;
                    modifier = cell.modifier;
                }
                if cell.fg != fg || cell.bg != bg {
                    queue!(
                        writer,
                        SetColors(Colors::new(
                            cell.fg.into_crossterm(),
                            cell.bg.into_crossterm(),
                        ))
                    )?;
                    fg = cell.fg;
                    bg = cell.bg;
                }

                if hyperlink_changed && let Some(destination) = destination {
                    queue!(writer, Print(format!("\x1b]8;;{destination}\x07")))?;
                }
                let symbol = hyperlink.map_or_else(|| cell.symbol(), |(_, visible)| visible);
                queue!(writer, Print(symbol))?;

                let glyph_width = display_width(cell.symbol()).max(1);
                let advance = u16::try_from(glyph_width).unwrap_or(u16::MAX);
                cursor.x = x.saturating_add(advance);
                cursor.y = y;
            }
            DrawCommand::ClearToEnd { bg: clear_bg, .. } => {
                queue!(writer, SetAttribute(crossterm::style::Attribute::Reset))?;
                modifier = Modifier::empty();
                queue!(writer, SetBackgroundColor((*clear_bg).into_crossterm()))?;
                bg = *clear_bg;
                queue!(writer, Clear(crossterm::terminal::ClearType::UntilNewLine))?;
                // EL 不移动光标，落点仍是这条指令的起点。
                cursor.x = x;
                cursor.y = y;
            }
        }
        if hyperlink_changed {
            active_hyperlink = destination.map(str::to_owned);
        }
    }
    if active_hyperlink.is_some() {
        queue!(writer, Print("\x1b]8;;\x07"))?;
    }

    queue!(
        writer,
        SetForegroundColor(crossterm::style::Color::Reset),
        SetBackgroundColor(crossterm::style::Color::Reset),
        SetAttribute(crossterm::style::Attribute::Reset),
    )?;

    Ok(())
}

/// 计算两个 [`Modifier`] 之间的差异，只发出真正变化的 SGR 属性指令。
struct ModifierDiff {
    from: Modifier,
    to: Modifier,
}

impl ModifierDiff {
    fn queue<W: Write>(self, w: &mut W) -> io::Result<()> {
        use crossterm::style::Attribute as CAttribute;
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

#[cfg(test)]
mod tests {
    use ratatui::backend::WindowSize;
    use ratatui::layout::Rect;
    use ratatui::style::Style;

    use super::*;

    /// 记录写入字节、可配置固定尺寸的测试后端。`ratatui::backend::TestBackend` 不实现
    /// `Write`，`Terminal<B>` 要求 `B: Write`，所以这里自己写一个（元组字段：`.0` 是输出
    /// 字节，`.1` 是固定屏幕尺寸）。
    #[derive(Debug)]
    struct RecordingBackend(Vec<u8>, Size);

    impl RecordingBackend {
        fn new(width: u16, height: u16) -> Self {
            Self(Vec::new(), Size { width, height })
        }
    }

    impl Write for RecordingBackend {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Backend for RecordingBackend {
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
            Ok(Position { x: 0, y: 0 })
        }

        fn set_cursor_position<P: Into<Position>>(&mut self, _position: P) -> io::Result<()> {
            Ok(())
        }

        fn clear(&mut self) -> io::Result<()> {
            Ok(())
        }

        fn clear_region(&mut self, _clear_type: ClearType) -> io::Result<()> {
            Ok(())
        }

        fn size(&self) -> io::Result<Size> {
            Ok(self.1)
        }

        fn window_size(&mut self) -> io::Result<WindowSize> {
            Ok(WindowSize {
                columns_rows: self.1,
                pixels: self.1,
            })
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }

        fn scroll_region_up(
            &mut self,
            _region: std::ops::Range<u16>,
            _line_count: u16,
        ) -> io::Result<()> {
            Ok(())
        }

        fn scroll_region_down(
            &mut self,
            _region: std::ops::Range<u16>,
            _line_count: u16,
        ) -> io::Result<()> {
            Ok(())
        }
    }

    /// 统计 `text` 里 CUP（`ESC[…H`，绝对定位）与 CHA（`ESC[…G`，绝对列）序列各出现几次。
    /// 用于断言不变量 2：普通增量帧路径两者都必须是 0。
    fn count_absolute_position_sequences(text: &str) -> (usize, usize) {
        let mut cup = 0usize;
        let mut cha = 0usize;
        for segment in text.split('\u{1b}') {
            let Some(rest) = segment.strip_prefix('[') else {
                continue;
            };
            let Some(params_end) = rest.find(|c: char| !c.is_ascii_digit() && c != ';') else {
                continue;
            };
            match rest.as_bytes().get(params_end) {
                Some(b'H') => cup += 1,
                Some(b'G') => cha += 1,
                _ => {}
            }
        }
        (cup, cha)
    }

    /// 统计 `text` 里相对定位序列（`ESC[{n}A/B/C/D`）各出现几次，按 上/下/右/左 顺序返回。
    fn count_relative_position_sequences(text: &str) -> (usize, usize, usize, usize) {
        let mut up = 0usize;
        let mut down = 0usize;
        let mut right = 0usize;
        let mut left = 0usize;
        for segment in text.split('\u{1b}') {
            let Some(rest) = segment.strip_prefix('[') else {
                continue;
            };
            let Some(params_end) = rest.find(|c: char| !c.is_ascii_digit() && c != ';') else {
                continue;
            };
            match rest.as_bytes().get(params_end) {
                Some(b'A') => up += 1,
                Some(b'B') => down += 1,
                Some(b'C') => right += 1,
                Some(b'D') => left += 1,
                _ => {}
            }
        }
        (up, down, right, left)
    }

    #[test]
    fn diff_buffers_handles_wide_glyphs_without_split_or_duplicate_put() {
        let area = Rect::new(0, 0, 4, 1);

        // 场景一：内容未变（都是"中文"），不应重复发送 Put。
        let mut unchanged = Buffer::empty(area);
        unchanged.set_string(0, 0, "中文", Style::default());
        let same = unchanged.clone();
        let commands = diff_buffers(&unchanged, &same);
        assert!(
            commands.iter().all(|c| !c.is_put()),
            "内容未变时不应发送 Put: {commands:?}"
        );

        // 场景二：一个宽字符覆盖两个窄字符；只应发一次 Put（宽字符整簇），不能被拆成
        // 两条 Put（那样等于把 CJK 字符切成两半发送）。
        let mut previous = Buffer::empty(area);
        previous.set_string(0, 0, "AB", Style::default());
        let mut next = Buffer::empty(area);
        next.set_string(0, 0, "中", Style::default());

        let commands = diff_buffers(&previous, &next);
        let puts: Vec<&DrawCommand> = commands.iter().filter(|c| c.is_put()).collect();
        assert_eq!(puts.len(), 1, "宽字符覆盖应恰好一条 Put: {commands:?}");
        assert!(
            matches!(puts[0], DrawCommand::Put { x: 0, y: 0, cell } if cell.symbol() == "中"),
            "唯一的 Put 应携带完整的宽字符: {commands:?}"
        );
    }

    #[test]
    fn diff_buffers_emits_clear_to_end_only_for_trailing_blank() {
        let area = Rect::new(0, 0, 5, 1);
        let previous = Buffer::empty(area);

        let mut trailing_blank = Buffer::empty(area);
        trailing_blank.set_string(0, 0, "ab", Style::default());
        let commands = diff_buffers(&previous, &trailing_blank);
        assert!(
            commands
                .iter()
                .any(|c| matches!(c, DrawCommand::ClearToEnd { x: 2, y: 0, .. })),
            "行尾有空白时应发 ClearToEnd: {commands:?}"
        );

        let mut trailing_nonblank = Buffer::empty(area);
        trailing_nonblank.set_string(0, 0, "abcde", Style::default());
        let commands = diff_buffers(&previous, &trailing_nonblank);
        assert!(
            !commands
                .iter()
                .any(|c| matches!(c, DrawCommand::ClearToEnd { .. })),
            "整行写满时不应发 ClearToEnd: {commands:?}"
        );
    }

    #[test]
    fn draw_uses_only_relative_positioning_never_absolute() {
        // 连续两次 in-window draw：第一次画满一行，第二次只改其中一个单元格。
        let mut cursor = Position { x: 0, y: 0 };
        let first_pass = vec![
            DrawCommand::Put {
                x: 0,
                y: 0,
                cell: Cell::new("a"),
            },
            DrawCommand::Put {
                x: 1,
                y: 0,
                cell: Cell::new("b"),
            },
            DrawCommand::Put {
                x: 2,
                y: 0,
                cell: Cell::new("c"),
            },
        ];
        let mut output: Vec<u8> = Vec::new();
        draw(&mut output, first_pass.into_iter(), &mut cursor).expect("首次绘制不应失败");

        let second_pass = vec![DrawCommand::Put {
            x: 1,
            y: 0,
            cell: Cell::new("x"),
        }];
        draw(&mut output, second_pass.into_iter(), &mut cursor).expect("二次绘制不应失败");

        let text = String::from_utf8(output).expect("draw 输出应为合法 utf8");
        let (cup, cha) = count_absolute_position_sequences(&text);
        assert_eq!(cup, 0, "普通增量帧不应出现 CUP（绝对行列）: {text:?}");
        assert_eq!(cha, 0, "普通增量帧不应出现 CHA（绝对列）: {text:?}");

        // 第一遍 3 个连续同行单元格只应触发 1 次相对移动（第一格从 (0,0) 出发需要 0 次，
        // 后两格紧跟在上一格之后，命中"目标==当前光标"优化），第二遍单独改中间那格需要
        // 再退回一次列方向的相对移动。
        let (up, down, right, left) = count_relative_position_sequences(&text);
        assert_eq!((up, down), (0, 0), "同一行内不应有行方向移动: {text:?}");
        assert!(
            right + left > 0,
            "第二遍应发出至少一次列方向相对移动: {text:?}"
        );
    }

    #[test]
    fn move_relative_tracks_cross_row_cross_column_and_backward_moves() {
        let mut output: Vec<u8> = Vec::new();
        let mut cursor = Position { x: 5, y: 5 };

        // 跨行前进 + 跨列前进。
        move_relative(&mut output, &mut cursor, Position { x: 9, y: 8 })
            .expect("前进方向的相对移动不应失败");
        assert_eq!(cursor, Position { x: 9, y: 8 });

        // 回退行（dy < 0）+ 回退列。
        move_relative(&mut output, &mut cursor, Position { x: 2, y: 3 })
            .expect("回退方向的相对移动不应失败");
        assert_eq!(cursor, Position { x: 2, y: 3 });

        let text = String::from_utf8(output).expect("move_relative 输出应为合法 utf8");
        let (cup, cha) = count_absolute_position_sequences(&text);
        assert_eq!((cup, cha), (0, 0), "相对移动不应出现 CUP/CHA: {text:?}");

        let (up, down, right, left) = count_relative_position_sequences(&text);
        assert_eq!(down, 1, "第一次移动应发一次 MoveDown: {text:?}");
        assert_eq!(right, 1, "第一次移动应发一次 MoveRight: {text:?}");
        assert_eq!(up, 1, "第二次移动应发一次 MoveUp: {text:?}");
        assert_eq!(left, 1, "第二次移动应发一次 MoveLeft: {text:?}");
    }

    #[test]
    fn set_viewport_area_clamps_visible_history_rows() {
        let backend = RecordingBackend::new(20, 10);
        let mut terminal = Terminal::with_screen_size(
            backend,
            Size {
                width: 20,
                height: 10,
            },
            Position { x: 0, y: 0 },
        );

        terminal.set_viewport_area(Rect::new(0, 5, 20, 5));
        terminal.note_history_rows_inserted(10);
        assert_eq!(
            terminal.visible_history_rows(),
            5,
            "note_history_rows_inserted 应 clamp 到 viewport_area.top()"
        );

        terminal.set_viewport_area(Rect::new(0, 2, 20, 8));
        assert_eq!(
            terminal.visible_history_rows(),
            2,
            "set_viewport_area 应把既有 visible_history_rows 重新 clamp 到新的 top()"
        );
    }
}
