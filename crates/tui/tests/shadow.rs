//! 影子提交账本：独立重算 C/W，并把屏幕上能观察到的整条 tape 与
//! `shadow tape + window slice` 逐行比对。
//!
//! 做法来自 `oh-my-pi/docs/tui-core-renderer.md:243-251`：影子账本只吃**观测输入**
//! （渲染了什么、终端收到了什么字节），绝不复用被测代码的中间量，否则同一个 bug
//! 会在两边同时出现而测试全绿。
//!
//! # 能断言什么、不能断言什么
//!
//! `vt100` 的滚动区实现只在滚动区覆盖整屏时才把滚出的行推进 scrollback
//! （`vt100-0.16.2/src/grid.rs:566` 的 `!self.scroll_region_active()`），而 DECSTBM
//! 注入用的正是"只到 viewport 顶"的受限区间；它也不实现 ED3（`screen.rs:1061-1066`
//! 的 `mode 3` 落进 `unhandled`）。所以这里断言的是**可见屏幕**与**发出的字节**，
//! 不是 scrollback 里的内容。
//!
//! 这与 `plans/tui/modules.md:115` 的结论一致：`vt100` 单测只能保证发出的字节是对的，
//! 保证不了终端照做——macOS / Linux 各跑一次真机烟测不可省。

use std::io::{self, Write};
use std::ops::Range;

use ratatui::backend::{Backend, ClearType, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Size};
use ratatui::text::Line;
use zcode_tui::caps::OutputCaps;
use zcode_tui::compose::{Component, ComponentId};
use zcode_tui::emit::{EmitPath, Emitter};
use zcode_tui::terminal::Terminal;

// ── 测试后端 ───────────────────────────────────────────────────────────

/// 真解析的测试后端：写入的字节同时喂给 `vt100::Parser`（还原屏幕）和 `written`
/// （让测试能直接数 ED3 之类的转义序列）。
struct Screen {
    parser: vt100::Parser,
    size: Size,
    written: Vec<u8>,
}

impl Screen {
    fn new(width: u16, height: u16) -> Self {
        Self {
            parser: vt100::Parser::new(height, width, 1024),
            size: Size::new(width, height),
            written: Vec::new(),
        }
    }

    /// 改变屏幕尺寸，模拟终端 resize。
    fn resize(&mut self, width: u16, height: u16) {
        self.parser.screen_mut().set_size(height, width);
        self.size = Size::new(width, height);
    }

    /// 可见屏幕的每一行（去掉行尾空白，便于与源文本比较）。
    fn rows(&self) -> Vec<String> {
        self.parser
            .screen()
            .rows(0, self.size.width)
            .map(|row| row.trim_end().to_owned())
            .collect()
    }

    fn bytes(&self) -> String {
        String::from_utf8_lossy(&self.written).into_owned()
    }

    fn take_bytes(&mut self) -> String {
        let out = self.bytes();
        self.written.clear();
        out
    }
}

impl Write for Screen {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.written.extend_from_slice(buf);
        self.parser.process(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Backend for Screen {
    type Error = io::Error;

    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        for (x, y, cell) in content {
            write!(self, "\x1b[{};{}H{}", y + 1, x + 1, cell.symbol())?;
        }
        Ok(())
    }

    fn hide_cursor(&mut self) -> io::Result<()> {
        write!(self, "\x1b[?25l")
    }

    fn show_cursor(&mut self) -> io::Result<()> {
        write!(self, "\x1b[?25h")
    }

    fn get_cursor_position(&mut self) -> io::Result<Position> {
        let (row, col) = self.parser.screen().cursor_position();
        Ok(Position::new(col, row))
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        let position = position.into();
        write!(self, "\x1b[{};{}H", position.y + 1, position.x + 1)
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
        write!(
            self,
            "\x1b[{};{}r\x1b[{line_count}S\x1b[r",
            region.start + 1,
            region.end
        )
    }

    fn scroll_region_down(&mut self, region: Range<u16>, line_count: u16) -> io::Result<()> {
        write!(
            self,
            "\x1b[{};{}r\x1b[{line_count}T\x1b[r",
            region.start + 1,
            region.end
        )
    }
}

// ── 组件 ───────────────────────────────────────────────────────────────

/// 一段固定文本。`live` 上报组件内第一条仍可能变化的行。
struct Block {
    id: u64,
    revision: u64,
    lines: Vec<String>,
    live: Option<usize>,
}

impl Block {
    fn new(id: u64, lines: &[&str]) -> Self {
        Self {
            id,
            revision: 0,
            lines: lines.iter().map(|s| (*s).to_owned()).collect(),
            live: None,
        }
    }

    fn live_from(mut self, row: usize) -> Self {
        self.live = Some(row);
        self
    }

    /// 内容变了就必须让 revision 跟着变——[`Component::revision`] 是 composer
    /// 判定"能否复用上一帧的行"的唯一依据，忘了改就会看到上一帧的内容。
    fn at_revision(mut self, revision: u64) -> Self {
        self.revision = revision;
        self
    }
}

impl Component for Block {
    fn id(&self) -> ComponentId {
        ComponentId(self.id)
    }

    fn revision(&self) -> u64 {
        self.revision
    }

    fn render(&self, _width: u16) -> Vec<Line<'static>> {
        self.lines.iter().map(|s| Line::from(s.clone())).collect()
    }

    fn live_boundary(&self) -> Option<usize> {
        self.live
    }
}

/// 把组件列表摊平成"如果排版正确，帧应该长什么样"的行文本。
///
/// 刻意不调用 `Composer`：影子侧必须独立算出期望值。
fn expected_frame(blocks: &[Block]) -> Vec<String> {
    blocks
        .iter()
        .flat_map(|b| b.lines.iter().cloned())
        .collect()
}

// ── 影子账本 ───────────────────────────────────────────────────────────

/// 独立重算 C/W。数学抄自 `plans/tui/architecture.md:25-35`，不看 `Ledger` 的实现。
#[derive(Debug, Default, Clone, Copy)]
struct Shadow {
    committed: usize,
    window_top: usize,
}

impl Shadow {
    /// full paint 会把 committed prefix 整段重放，账本先退回 0。
    fn full_paint(&mut self) {
        self.committed = 0;
    }

    fn step(&mut self, frame_rows: usize, height: usize, boundary: usize, pinned: bool) {
        let window_top = self.committed.max(frame_rows.saturating_sub(height));
        let chunk_to = if pinned {
            window_top.min(boundary)
        } else {
            window_top
        };
        self.committed = self.committed.max(chunk_to);
        self.window_top = window_top;
    }
}

/// 一帧渲染之后的全部断言。
///
/// 1. 引擎的 C/W 与影子账本一致；
/// 2. 可见窗口逐行等于 `frame[W .. W+h]`；
/// 3. viewport **上方**还留在屏幕上的行，逐行等于 `frame[0..C]` 的对应尾段——
///    这一条才是"tape == shadow tape + window slice"的可观测形式，它同时抓住
///    "某段被提交了两次"（重复段会把尾段整体错位）。
#[track_caller]
fn assert_tape(emitter: &Emitter<Screen>, shadow: Shadow, frame: &[String], height: usize) {
    let ledger = emitter.ledger();
    assert_eq!(
        ledger.committed_rows(),
        shadow.committed,
        "C 与影子账本不一致"
    );
    assert_eq!(ledger.window_top(), shadow.window_top, "W 与影子账本不一致");

    let area = emitter.terminal().viewport_area();
    let rows = emitter.terminal().backend().rows();
    let top = usize::from(area.y);

    // 两侧都去掉行尾空白：`Screen::rows` 读的是等宽网格，行尾必然被空格填满，
    // 而源文本可能自带尾随空格（比如宽度为 0 的进度条）。
    for offset in 0..height {
        let on_screen = rows.get(top + offset).map_or("", String::as_str);
        let expected = frame
            .get(shadow.window_top + offset)
            .map_or("", |line| line.trim_end());
        assert_eq!(on_screen, expected, "窗口第 {offset} 行不符");
    }

    // viewport 上方还能看见的已提交行：屏幕上有 `top` 行，账本说提交了 `C` 行，
    // 能对上的是二者的较小值。
    let visible_history = top.min(shadow.committed);
    for back in 1..=visible_history {
        let on_screen = rows.get(top - back).map_or("", String::as_str);
        let expected = frame
            .get(shadow.committed - back)
            .map_or("", |line| line.trim_end());
        assert_eq!(on_screen, expected, "viewport 上方倒数第 {back} 行不符");
    }
}

fn interactive_caps(scrollback_purge: bool) -> OutputCaps {
    OutputCaps {
        interactive_output: true,
        scrollback_purge,
    }
}

fn new_emitter(width: u16, height: u16, caps: OutputCaps) -> Emitter<Screen> {
    let backend = Screen::new(width, height);
    let terminal =
        Terminal::with_screen_size(backend, Size::new(width, height), Position { x: 0, y: 0 });
    Emitter::new(terminal, caps)
}

// ── 测试 ───────────────────────────────────────────────────────────────

/// 逐帧追加内容，每帧都对账。这是主契约测试。
#[test]
fn tape_equals_shadow_tape_plus_window_slice() {
    const HEIGHT: u16 = 6;
    let mut emitter = new_emitter(40, 20, interactive_caps(true));
    let mut shadow = Shadow::default();

    let mut blocks: Vec<Block> = Vec::new();
    for turn in 0..8u64 {
        blocks.push(Block::new(
            turn,
            &[
                &format!("turn {turn} line a"),
                &format!("turn {turn} line b"),
                &format!("turn {turn} line c"),
            ],
        ));
        let frame = expected_frame(&blocks);
        let refs: Vec<&dyn Component> = blocks.iter().map(|b| -> &dyn Component { b }).collect();

        let path = emitter.render(&refs, HEIGHT).expect("render 失败");
        if turn == 0 {
            shadow.full_paint();
            assert_eq!(path, EmitPath::FullPaint, "首帧必须是 full paint");
        }
        shadow.step(frame.len(), usize::from(HEIGHT), frame.len(), false);

        assert_tape(&emitter, shadow, &frame, usize::from(HEIGHT));
    }
}

/// pinned 活跃区：可变尾部留在 viewport 内，`C` 卡在 `B`。
#[test]
fn pinned_live_region_keeps_mutable_suffix_out_of_history() {
    const HEIGHT: u16 = 6;
    let mut emitter = new_emitter(40, 20, interactive_caps(true));
    emitter.set_pinned(true);
    let mut shadow = Shadow::default();

    let history: Vec<Block> = (0..5u64)
        .map(|i| Block::new(i, &[&format!("history {i}")]))
        .collect();

    for tick in 0..4u64 {
        let mut blocks: Vec<Block> = history
            .iter()
            .map(|b| {
                Block::new(
                    b.id,
                    &b.lines.iter().map(String::as_str).collect::<Vec<_>>(),
                )
            })
            .collect();
        // 尾部是一个整段都在变的仪表盘：`live_boundary(0)` 表示它的第 0 行起就可变。
        blocks.push(
            Block::new(
                99,
                &[
                    &format!("dash tick {tick}"),
                    &format!(
                        "dash bar {}",
                        "#".repeat(usize::try_from(tick).unwrap_or(0))
                    ),
                ],
            )
            .live_from(0)
            .at_revision(tick),
        );

        let frame = expected_frame(&blocks);
        let boundary = frame.len() - 2;
        let refs: Vec<&dyn Component> = blocks.iter().map(|b| -> &dyn Component { b }).collect();
        emitter.render(&refs, HEIGHT).expect("render 失败");
        if tick == 0 {
            shadow.full_paint();
        }
        shadow.step(frame.len(), usize::from(HEIGHT), boundary, true);

        assert_eq!(
            emitter.ledger().boundary(),
            boundary,
            "compose 上报的 B 与手算不一致"
        );
        assert_tape(&emitter, shadow, &frame, usize::from(HEIGHT));
    }
}

/// **不丢行的硬不变量**：活跃区比整块屏幕还高时，`[B, W)` 不许消失。
///
/// pinned 语义把 `commit_end` 卡在 `B`（`ledger.rs:157-158`）。只要 `W > B`——活跃区
/// 装不进屏幕时必然如此——这段行就既不会被提交进 scrollback，也不落在窗口切片里。
/// 真机现象是消息凭空少几行，且不会自愈。
///
/// 引擎的取舍是 duplication, never loss：本帧降级 unpinned，把滚出去的可变行按
/// 冻结快照提交。这个测试盯的是**每一行都还在**，不是「pinned 有没有被关掉」，
/// 所以换别的补救手段它照样有效。
#[test]
fn oversized_live_region_never_drops_rows() {
    const SCREEN_H: u16 = 10;
    let mut emitter = new_emitter(40, SCREEN_H, interactive_caps(true));
    emitter.set_pinned(true);

    // 一个整段可变的块，行数是屏幕高度的两倍——真机上对应「工具输出的尾窗 + 状态行 +
    // 带边框的输入框」在 24 行终端里的处境。
    let tail: Vec<String> = (0..SCREEN_H * 2).map(|i| format!("live {i}")).collect();
    let history = Block::new(0, &["committed history"]);
    let live = Block::new(99, &tail.iter().map(String::as_str).collect::<Vec<_>>()).live_from(0);

    let blocks = [history, live];
    let refs: Vec<&dyn Component> = blocks.iter().map(|b| -> &dyn Component { b }).collect();
    emitter.render(&refs, 1).expect("render 失败");

    let frame = expected_frame(&blocks);
    let committed = emitter.ledger().committed_rows();
    let window_top = emitter.ledger().window_top();
    assert_eq!(
        committed, window_top,
        "已提交行数必须与窗口顶端接上；差出来的那一段就是丢掉的行"
    );
    // 屏幕上能看到的 + 已经交给历史的，合起来必须覆盖整帧，一行不少。
    assert!(
        committed + usize::from(SCREEN_H) >= frame.len(),
        "历史 {committed} 行 + 屏幕 {SCREEN_H} 行 < 整帧 {} 行，中间那段丢了",
        frame.len()
    );
}

/// 退出时活跃区必须被收起，否则它原样烙在终端上。
///
/// 真机现象：退出 zcode 后 shell 提示符下方挂着半个圆角输入框，而且边框的 SGR
/// 还开着，后面每一行都染上边框色。原因是活跃区里的东西**从没提交进 scrollback**
/// ——它是「还会变」的内容，进程一走就没人再管它了。
///
/// 已经提交的 transcript 不受影响：`shutdown` 只清 viewport 及其下方。
#[test]
fn shutdown_clears_the_live_region_but_keeps_history() {
    const SCREEN_H: u16 = 12;
    let mut emitter = new_emitter(20, SCREEN_H, interactive_caps(true));
    emitter.set_pinned(true);

    let history = Block::new(0, &["committed one", "committed two"]);
    let live = Block::new(99, &["LIVEBOX", "LIVEBOX"]).live_from(0);
    let blocks = [history, live];
    let refs: Vec<&dyn Component> = blocks.iter().map(|b| -> &dyn Component { b }).collect();
    emitter.render(&refs, 1).expect("render 失败");

    let before = emitter.terminal().backend().rows().join("\n");
    assert!(before.contains("LIVEBOX"), "活跃区应该先画出来：{before}");

    emitter.shutdown().expect("收起活跃区失败");

    let after = emitter.terminal().backend().rows().join("\n");
    assert!(
        !after.contains("LIVEBOX"),
        "活跃区没被清掉，它会留在 shell 提示符旁边：{after}"
    );
    assert!(
        after.contains("committed one"),
        "已提交的 transcript 不该被一起清掉：{after}"
    );
}

/// full paint 的三种几何：空 transcript / 未溢出屏高 / 溢出屏高。
///
/// 关键断言是 viewport 顶端落点：预留空行的循环与 prefix 共用一个行计数器，
/// prefix 为空时不能先发一个 `\r\n` 把 viewport 顶推到 row 1。
#[test]
fn full_paint_anchors_viewport_for_every_geometry() {
    const HEIGHT: u16 = 4;
    const SCREEN_H: u16 = 12;

    // a. 空 transcript：viewport 必须落在 row 0。
    {
        let mut emitter = new_emitter(24, SCREEN_H, interactive_caps(false));
        let empty: Vec<&dyn Component> = Vec::new();
        emitter.render(&empty, HEIGHT).expect("render 失败");
        assert_eq!(
            emitter.terminal().viewport_area().y,
            0,
            "空帧 viewport 应在 row 0"
        );
        assert_eq!(emitter.ledger().committed_rows(), 0);
    }

    // b. 未溢出：3 行历史 + 4 行 viewport 共 7 行，装得下 12 行的屏幕。
    {
        let mut emitter = new_emitter(24, SCREEN_H, interactive_caps(false));
        let block = Block::new(0, &["h0", "h1", "h2", "w0", "w1", "w2", "w3"]);
        let refs: Vec<&dyn Component> = vec![&block];
        emitter.render(&refs, HEIGHT).expect("render 失败");

        assert_eq!(
            emitter.ledger().committed_rows(),
            3,
            "7 行帧、4 行窗口 → 提交 3 行"
        );
        assert_eq!(
            emitter.terminal().viewport_area().y,
            3,
            "viewport 紧接历史下方"
        );
        let rows = emitter.terminal().backend().rows();
        assert_eq!(rows.first().map(String::as_str), Some("h0"));
        assert_eq!(rows.get(2).map(String::as_str), Some("h2"));
        assert_eq!(rows.get(3).map(String::as_str), Some("w0"));
        assert_eq!(rows.get(6).map(String::as_str), Some("w3"));
    }

    // c. 溢出屏高：40 行帧塞进 12 行屏幕，viewport 必须钉在最底部。
    {
        let mut emitter = new_emitter(24, SCREEN_H, interactive_caps(false));
        let lines: Vec<String> = (0..40).map(|i| format!("row{i}")).collect();
        let block = Block::new(0, &lines.iter().map(String::as_str).collect::<Vec<_>>());
        let refs: Vec<&dyn Component> = vec![&block];
        emitter.render(&refs, HEIGHT).expect("render 失败");

        assert_eq!(emitter.ledger().committed_rows(), 36);
        assert_eq!(
            emitter.terminal().viewport_area().y,
            SCREEN_H - HEIGHT,
            "溢出时 viewport 钉在屏幕底部"
        );
        let rows = emitter.terminal().backend().rows();
        for (offset, expected) in ["row36", "row37", "row38", "row39"].iter().enumerate() {
            let y = usize::from(SCREEN_H - HEIGHT) + offset;
            assert_eq!(rows.get(y).map(String::as_str), Some(*expected));
        }
    }
}

/// resize 是三类默认发 ED3 的显式手势之一。非 mux 下必须擦 scrollback 后重放，
/// 否则每次 resize 都把 `[0, C)` 再往历史追加一份。
#[test]
fn resize_purges_scrollback_once_outside_multiplexer() {
    const HEIGHT: u16 = 5;
    let lines: Vec<String> = (0..30).map(|i| format!("line{i}")).collect();
    let block = Block::new(0, &lines.iter().map(String::as_str).collect::<Vec<_>>());
    let frame: Vec<String> = lines.clone();

    for purge in [true, false] {
        let mut emitter = new_emitter(40, 20, interactive_caps(purge));
        let refs: Vec<&dyn Component> = vec![&block];
        emitter.render(&refs, HEIGHT).expect("首帧失败");

        // 首帧不算手势，绝不发 ED3。
        let first = emitter.terminal_mut().backend_mut().take_bytes();
        assert!(
            !first.contains("\x1b[3J"),
            "首帧不是用户手势，不该擦 scrollback（purge={purge}）"
        );

        emitter.terminal_mut().backend_mut().resize(40, 16);
        let path = emitter.render(&refs, HEIGHT).expect("resize 帧失败");
        assert_eq!(path, EmitPath::FullPaint, "resize 必须走 full paint");

        let after = emitter.terminal_mut().backend_mut().take_bytes();
        let ed3 = after.matches("\x1b[3J").count();
        if purge {
            assert_eq!(ed3, 1, "非 mux 的 resize 必须恰好发一次 ED3");
        } else {
            assert_eq!(ed3, 0, "mux 下绝不发 ED3");
        }

        // 无论有没有 ED3，屏幕上的内容都必须是重放后的正确结果。
        let mut shadow = Shadow::default();
        shadow.full_paint();
        shadow.step(frame.len(), usize::from(HEIGHT), frame.len(), false);
        assert_tape(&emitter, shadow, &frame, usize::from(HEIGHT));
    }
}

/// `ctrl+o`：`reset_display` 是手势，能擦就擦；擦不了就重放出重复段，
/// 但**绝不**在 mux 下发 ED3。
#[test]
fn reset_display_purges_when_allowed_and_degrades_otherwise() {
    const HEIGHT: u16 = 4;
    let block = Block::new(0, &["a", "b", "c", "d", "e", "f", "g", "h"]);

    for purge in [true, false] {
        let mut emitter = new_emitter(20, 10, interactive_caps(purge));
        let refs: Vec<&dyn Component> = vec![&block];
        emitter.render(&refs, HEIGHT).expect("首帧失败");
        let _ = emitter.terminal_mut().backend_mut().take_bytes();

        emitter.reset_display();
        let path = emitter.render(&refs, HEIGHT).expect("reset 帧失败");
        assert_eq!(path, EmitPath::FullPaint);

        let bytes = emitter.terminal_mut().backend_mut().take_bytes();
        assert_eq!(
            bytes.matches("\x1b[3J").count(),
            usize::from(purge),
            "reset_display 的 ED3 次数只由 scrollback_purge 决定（purge={purge}）"
        );
    }
}

/// `interactive_output == false` 时只有纯文本，一个 escape 都不发。
#[test]
fn non_interactive_output_writes_plain_text_only() {
    let caps = OutputCaps {
        interactive_output: false,
        scrollback_purge: false,
    };
    let mut emitter = new_emitter(40, 20, caps);

    // 尾部组件从第 1 行起仍可变：只有 B 之前的行能写出去。
    let block = Block::new(0, &["done 0", "done 1", "streaming…"]).live_from(2);
    let refs: Vec<&dyn Component> = vec![&block];
    let path = emitter.render(&refs, 5).expect("render 失败");

    assert_eq!(path, EmitPath::PlainStdout);
    let bytes = emitter.terminal_mut().backend_mut().take_bytes();
    assert!(
        !bytes.contains('\x1b'),
        "纯文本路径不得发任何 escape: {bytes:?}"
    );
    assert_eq!(bytes, "done 0\ndone 1\n", "只写到 live boundary 为止");

    // 再渲染一次同样的内容：已写出的行不能重复写。
    let path = emitter.render(&refs, 5).expect("render 失败");
    assert_eq!(path, EmitPath::PlainStdout);
    assert_eq!(emitter.terminal_mut().backend_mut().take_bytes(), "");
}

/// 帧高度恒定的 sliding tail window：第一帧之后不应再有任何行进入历史，
/// 也不该因为内容变化触发重锚定。这是"固定高度活跃区"必须走 in-window diff
/// 的不变式（`plans/tui/architecture.md:66-73`）。
///
/// 关键前提是组件**上报了 live boundary**：整段都可变（`live_from(0)`）意味着
/// 滚出窗口的行是冻结快照，落在 audit-exempt 的 frozen 区。少了这个上报，
/// 这些行会被声明为 FINAL，内容一变就是 committed prefix 漂移
/// （下一个测试锁死那条路径）。
#[test]
fn stable_frame_height_stops_committing_and_stays_in_window() {
    const HEIGHT: u16 = 6;
    let mut emitter = new_emitter(40, 20, interactive_caps(true));

    let mut committed_after_first = None;
    for tick in 0..5u64 {
        // 行数恒定、内容每帧都变：典型的 sliding tail window。
        let lines: Vec<String> = (0..10).map(|i| format!("tick {tick} slot {i}")).collect();
        let block = Block::new(0, &lines.iter().map(String::as_str).collect::<Vec<_>>())
            .at_revision(tick)
            .live_from(0);
        let refs: Vec<&dyn Component> = vec![&block];
        let path = emitter.render(&refs, HEIGHT).expect("render 失败");

        match committed_after_first {
            None => committed_after_first = Some(emitter.ledger().committed_rows()),
            Some(expected) => {
                assert_eq!(
                    emitter.ledger().committed_rows(),
                    expected,
                    "L 恒定时不该有新行进入历史"
                );
                assert_eq!(path, EmitPath::InWindowDiff, "应当停在 in-window diff");
            }
        }
    }
}

/// 组件改写了自己声明为 FINAL 的行：审计必须发现并重锚定，让后续行重新提交。
///
/// 旧副本留在历史里——duplication, never loss。这里**不该**出现 ED3：
/// divergence rebuild 默认关闭，擦历史只由用户手势触发
/// （`plans/tui/architecture.md:197-216`）。
#[test]
fn mutating_a_finalized_row_reanchors_without_touching_scrollback() {
    const HEIGHT: u16 = 4;
    let mut emitter = new_emitter(20, 12, interactive_caps(true));

    let first = Block::new(0, &["a0", "a1", "a2", "a3", "a4", "a5"]);
    let refs: Vec<&dyn Component> = vec![&first];
    emitter.render(&refs, HEIGHT).expect("首帧失败");
    assert_eq!(
        emitter.ledger().committed_rows(),
        2,
        "6 行帧、4 行窗口 → 提交 2 行"
    );
    let _ = emitter.terminal_mut().backend_mut().take_bytes();

    // 第 1 行（已提交、且声明为 FINAL）被改写。
    let mutated = Block::new(0, &["a0", "CHANGED", "a2", "a3", "a4", "a5"]).at_revision(1);
    let refs: Vec<&dyn Component> = vec![&mutated];
    let path = emitter.render(&refs, HEIGHT).expect("第二帧失败");

    assert_eq!(
        path,
        EmitPath::SeamRewrite,
        "committed prefix 漂移必须走 seam rewrite"
    );
    assert_eq!(
        emitter.ledger().committed_rows(),
        2,
        "重锚定后 [1, 2) 被重新提交"
    );
    let bytes = emitter.terminal_mut().backend_mut().take_bytes();
    assert!(
        !bytes.contains("\x1b[3J"),
        "divergence 不是用户手势，绝不擦 scrollback: {bytes:?}"
    );
}

/// 与 `examples/inline_demo.rs` 同形状的固定高度流式框：断言**终端上真正看到的样子**。
///
/// 真机（ConPTY / PTY）只能确认"不崩不卡"，看不出对齐错位——原始字节日志里
/// in-window diff 只重写变化的单元格，肉眼看上去必然是碎的。所以视觉正确性
/// 由这里的 `vt100` 回放来锁：边框逐行等宽、内容落在框内、上方是已定稿的 transcript。
#[test]
fn fixed_height_box_renders_aligned_on_screen() {
    const WIDTH: u16 = 24;
    const SCREEN_H: u16 = 12;
    const BOX_ROWS: u16 = 5;

    struct BoxedTail {
        revision: u64,
        rows: Vec<String>,
    }

    impl Component for BoxedTail {
        fn id(&self) -> ComponentId {
            ComponentId(u64::MAX)
        }

        fn revision(&self) -> u64 {
            self.revision
        }

        fn render(&self, width: u16) -> Vec<Line<'static>> {
            let inner = usize::from(width.saturating_sub(4));
            let mut out = vec![Line::from(format!("╭{}╮", "─".repeat(inner + 2)))];
            for slot in 0..usize::from(BOX_ROWS) - 2 {
                let text = self.rows.get(slot).map_or("", String::as_str);
                let clipped = zcode_text::width::truncate_to_width(text, inner, "…").into_owned();
                let pad = inner.saturating_sub(zcode_text::width::visible_width(&clipped));
                out.push(Line::from(format!("│ {clipped}{} │", " ".repeat(pad))));
            }
            out.push(Line::from(format!("╰{}╯", "─".repeat(inner + 2))));
            out
        }

        fn live_boundary(&self) -> Option<usize> {
            Some(0)
        }
    }

    let mut emitter = new_emitter(WIDTH, SCREEN_H, interactive_caps(true));
    let history = Block::new(0, &["turn 0 done", "turn 1 done"]);

    for tick in 0..4u64 {
        let tail = BoxedTail {
            revision: tick,
            rows: (0..3).map(|slot| format!("t{tick} s{slot}")).collect(),
        };
        let components: Vec<&dyn Component> = vec![&history, &tail];
        emitter.render(&components, BOX_ROWS).expect("render 失败");
    }

    let rows = emitter.terminal().backend().rows();
    let area = emitter.terminal().viewport_area();
    let top = usize::from(area.y);
    assert!(
        top >= 2,
        "transcript 应该被推到 viewport 上方，实际 top={top}"
    );

    assert_eq!(rows.get(top - 2).map(String::as_str), Some("turn 0 done"));
    assert_eq!(rows.get(top - 1).map(String::as_str), Some("turn 1 done"));

    let expected = [
        "╭──────────────────────╮",
        "│ t3 s0                │",
        "│ t3 s1                │",
        "│ t3 s2                │",
        "╰──────────────────────╯",
    ];
    for (offset, want) in expected.iter().enumerate() {
        let got = rows.get(top + offset).map_or("", String::as_str);
        assert_eq!(got, *want, "框第 {offset} 行不符");
        assert_eq!(
            zcode_text::width::visible_width(got),
            usize::from(WIDTH),
            "框第 {offset} 行显示宽度应铺满终端"
        );
    }
}
