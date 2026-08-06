//! 冒烟用例：在**真实终端**上跑一遍 transcript-first 渲染。
//!
//! 它做的事就是这个 crate 的目标形态：
//!
//! 1. 先像普通程序那样往终端打几行（模拟 shell 里已有的输出）；
//! 2. 进入 inline 渲染：底部一块固定高度的活跃区（sliding tail window），
//!    上方逐条追加已定稿的 transcript；
//! 3. 退出时活跃区收起，transcript 留在终端原生 scrollback 里。
//!
//! 跑法：
//!
//! ```console
//! cargo run -p zcode-tui --example inline_demo
//! ```
//!
//! 观察点（`plans/tui/modules.md:99-115` 的验证矩阵在真机上只能靠肉眼确认这几条）：
//!
//! - 退出后往上滚，`turn N` 那些行**还在**（进了 native scrollback）；
//! - 运行期间往上滚，视图**不会**被每帧渲染猛拽回底部（不变量 2）；
//! - 底部的框高度恒定、内容持续刷新，但不会在 scrollback 里堆重复副本
//!   （`L` 恒定 ⇒ 零字节进历史）。

use std::io::{self, Write, stdout};
use std::thread::sleep;
use std::time::Duration;

use ratatui::backend::CrosstermBackend;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use zcode_tui::caps::{self, OutputCaps};
use zcode_tui::compose::{Component, ComponentId};
use zcode_tui::emit::Emitter;
use zcode_tui::terminal::Terminal;

/// 活跃区高度：固定 8 行，其中 2 行是框线。
const VIEWPORT_ROWS: u16 = 8;
/// 框内可见的尾部行数。
const TAIL_ROWS: usize = 6;

/// 已定稿的一段 transcript。`revision` 不再变化，composer 会一直复用它的行。
struct Turn {
    index: u64,
    lines: Vec<String>,
}

impl Component for Turn {
    fn id(&self) -> ComponentId {
        ComponentId(self.index)
    }

    fn revision(&self) -> u64 {
        // 定稿后再不变化：revision 恒 0，composer 每帧直接复用缓存的行。
        0
    }

    fn render(&self, _width: u16) -> Vec<Line<'static>> {
        self.lines
            .iter()
            .map(|text| {
                Line::from(vec![
                    Span::styled("│ ", Style::default().fg(Color::DarkGray)),
                    Span::raw(text.clone()),
                ])
            })
            .collect()
    }
}

/// 底部的流式框：高度恒定，只展示最后 [`TAIL_ROWS`] 行，更早的行折成一条计数。
///
/// 高度恒定是刻意设计而非巧合：`L` 恒定 ⇒ `W` 恒定 ⇒ 零字节进历史，
/// 所以它每帧只重画 viewport。让它长过 viewport 会把可变尾部推到 commit window
/// 之上，每帧往 scrollback 提交一份新快照（`plans/tui/architecture.md:74-89`）。
struct TailBox {
    revision: u64,
    produced: usize,
    tail: Vec<String>,
}

impl Component for TailBox {
    fn id(&self) -> ComponentId {
        ComponentId(u64::MAX)
    }

    fn revision(&self) -> u64 {
        self.revision
    }

    fn render(&self, width: u16) -> Vec<Line<'static>> {
        let inner = usize::from(width.saturating_sub(4)).max(8);
        let hidden = self.produced.saturating_sub(self.tail.len());
        let border = Style::default().fg(Color::Cyan);

        let mut rows = Vec::with_capacity(VIEWPORT_ROWS.into());
        rows.push(Line::styled(format!("╭{}╮", "─".repeat(inner + 2)), border));
        let head = if hidden > 0 {
            format!("… ({hidden} earlier lines)")
        } else {
            "streaming…".to_owned()
        };
        rows.push(boxed(
            &head,
            inner,
            border,
            Style::default().fg(Color::DarkGray),
        ));
        for slot in 0..TAIL_ROWS.saturating_sub(1) {
            let text = self.tail.get(slot).map_or("", String::as_str);
            rows.push(boxed(text, inner, border, Style::default()));
        }
        rows.push(Line::styled(format!("╰{}╯", "─".repeat(inner + 2)), border));
        rows
    }

    fn live_boundary(&self) -> Option<usize> {
        // 整个框每帧都可能变。上报 0 让它落进 frozen 区，滚出窗口时提交的是快照，
        // 不会每帧触发 committed-prefix 重锚定。
        Some(0)
    }
}

/// 把一行文本包进框线，按显示宽度补齐（宽度求值走 `zcode-text`，不用 `len()`）。
fn boxed(text: &str, inner: usize, border: Style, body: Style) -> Line<'static> {
    let clipped = zcode_text::width::truncate_to_width(text, inner, "…").into_owned();
    let pad = inner.saturating_sub(zcode_text::width::visible_width(&clipped));
    Line::from(vec![
        Span::styled("│ ", border),
        Span::styled(clipped, body),
        Span::raw(" ".repeat(pad)),
        Span::styled(" │", border),
    ])
}

fn main() -> io::Result<()> {
    // 不变量 4：VT 启用必须早于任何 escape，且独立于 crossterm。
    caps::apply_output_modes()?;
    let caps = OutputCaps::probe();

    println!("$ zcode --example inline_demo");
    println!("（下面这块会在退出后留在终端 scrollback 里）");
    stdout().flush()?;

    let terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    let mut emitter = Emitter::new(terminal, caps);

    let mut turns: Vec<Turn> = Vec::new();
    let mut tail = TailBox {
        revision: 0,
        produced: 0,
        tail: Vec::new(),
    };

    for step in 0..48u64 {
        tail.revision = step;
        tail.produced += 1;
        tail.tail.push(format!(
            "chunk {step:02}: {}",
            "▁▂▃▄▅▆▇█"
                .chars()
                .cycle()
                .skip(usize::try_from(step).unwrap_or(0) % 8)
                .take(24)
                .collect::<String>()
        ));
        if tail.tail.len() > TAIL_ROWS - 1 {
            tail.tail.remove(0);
        }

        // 每 8 步定稿一轮，把它挪进 transcript。
        if step % 8 == 7 {
            turns.push(Turn {
                index: step / 8,
                lines: vec![
                    format!("turn {} 完成", step / 8),
                    format!("  产出 {} 个 chunk", tail.produced),
                ],
            });
            tail.produced = 0;
            tail.tail.clear();
        }

        let mut components: Vec<&dyn Component> =
            turns.iter().map(|t| -> &dyn Component { t }).collect();
        components.push(&tail);
        emitter.render(&components, VIEWPORT_ROWS)?;
        sleep(Duration::from_millis(60));
    }

    // 收起活跃区：把最后一个组件撤掉，只留 transcript。
    let components: Vec<&dyn Component> = turns.iter().map(|t| -> &dyn Component { t }).collect();
    emitter.render(&components, 1)?;
    emitter.terminal_mut().show_cursor()?;
    println!();
    println!(
        "最后一帧走的路径：{:?}；已提交 {} 行",
        emitter.last_path(),
        emitter.ledger().committed_rows()
    );
    Ok(())
}
