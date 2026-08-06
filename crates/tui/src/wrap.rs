//! span 感知的按显示宽度硬换行——`textwrap` 的替代实现
//! （约 150 行，见 `plans/tui/dependencies.md:41,94` 对自写换行的容许）。
//!
//! 本模块只负责在 `Line`/`Span` 结构上做切分与样式保留；宽度求值、按列截断、
//! grapheme 边界判定一律转调 [`zcode_text::width`]，绝不在这里另写一份
//! （见 `rule://zcode-architecture`「TUI 输出清理」）。

use ratatui::text::{Line, Span};
use zcode_text::width::{display_col_to_byte, expand_tabs, visible_width};

/// 一条 `Line` 的可见显示宽度（列）：逐 span 求 [`visible_width`] 之和。
#[must_use]
pub fn line_width(line: &Line<'_>) -> usize {
    line.spans
        .iter()
        .map(|span| visible_width(&span.content))
        .sum()
}

/// 该行在宽度 `width` 下占用的物理行数，至少 1。`width == 0` 时返回 1（没有可用列，
/// 但仍然要占一行——账本按"已写出的物理行数"推进，不能因为宽度退化成 0 就消失）。
///
/// **与 [`wrap_line`] 走同一个 `segment`，所以两者不可能算出不同的行数。**
/// 这是硬不变式：`insert_history` 用它推进 viewport 锚点、决定预留几行、以及上报注入了
/// 多少历史行；差一行就是光标错位加一条历史行被后续帧覆盖，而且**不会**在下一帧被
/// "自纠正"——账本记的是逻辑行，物理行数只在这里算一次。
///
/// 纯算术的 `ceil(总宽度 / width)` 是错的两种形状：
/// - 全 2 列宽字符配奇数 `width`：`"界界界"` @ 3 每行只放得下一个字、实际 3 行，公式给 2；
/// - 跨 span 的边界：`["ab", "界"]` @ 3 时 `界` 放不进剩下的 1 列，实际 2 行，公式给 1。
#[must_use]
pub fn line_rows(line: &Line<'_>, width: usize) -> usize {
    if width == 0 || line.spans.iter().all(|span| span.content.is_empty()) {
        return 1;
    }
    segment(line, width, |_| {})
}

/// 按显示宽度硬换行成若干条 `Line`。
///
/// - 切分点用 [`display_col_to_byte`] 定位，永远落在合法 UTF-8 与 grapheme 边界，
///   绝不把宽字符切半。
/// - 每个 span 的 `Style` 随切分保留；一个 span 被切开时两半各自带原 style。
/// - 行级 `style` / `alignment` 原样保留到每一条产出的 `Line`。
/// - `width == 0` 时返回单元素 `vec![line_to_static(line)]`（没有可用列，不切）。
/// - 空行（无 span，或全部 span 内容为空）返回**一条**空 `Line`，绝不返回空
///   `Vec`——空 `Vec` 会让上层的行数账本与实际写出的物理行数对不上。
/// - 制表符先经 [`expand_tabs`] 展开成对齐空格再切，避免等宽网格里出现视觉空洞。
///
/// 返回的行数恒等于 [`line_rows`]：两者共用 `segment`，且本函数每收到一次
/// `Piece::Break` 就落一行、结束时再落最后一行，与 `segment` 的计数逐一对应。
#[must_use]
pub fn wrap_line(line: &Line<'_>, width: usize) -> Vec<Line<'static>> {
    if width == 0 {
        return vec![line_to_static(line)];
    }
    if line.spans.iter().all(|span| span.content.is_empty()) {
        return vec![Line {
            style: line.style,
            alignment: line.alignment,
            spans: Vec::new(),
        }];
    }

    let mut rows: Vec<Line<'static>> = Vec::new();
    let mut current: Vec<Span<'static>> = Vec::new();
    segment(line, width, |piece| match piece {
        Piece::Text { style, text } => current.push(Span {
            style,
            content: text.to_owned().into(),
        }),
        Piece::Break => rows.push(finish_row(line, &mut current)),
    });
    rows.push(finish_row(line, &mut current));
    rows
}

/// [`segment`] 产出的事件。
enum Piece<'a> {
    /// 往当前行追加一段文本；保证非空，且整段来自同一个 span。
    Text {
        style: ratatui::style::Style,
        text: &'a str,
    },
    /// 当前行结束，后续内容进下一行。
    Break,
}

/// 贪心切分的**唯一**实现。返回产出的物理行数（至少 1，等于 `Break` 次数 + 1）。
///
/// 关键约束：**只有"空行上的单个 grapheme 自身宽于整个 `width`"才允许该行超宽。**
/// 当前行已有内容而剩余列放不下下一个视觉单元时必须先换行——强塞的话我们会报 1 行，
/// 而终端在打印那个宽字符之前自己 soft-wrap 成 2 行，账本与屏幕就此错开。
fn segment(line: &Line<'_>, width: usize, mut sink: impl FnMut(Piece<'_>)) -> usize {
    debug_assert!(width > 0, "segment 要求 width > 0，退化情形由调用方短路");
    let mut rows = 1usize;
    let mut current_width = 0usize;

    for span in &line.spans {
        let expanded = expand_tabs(&span.content);
        let mut rest: &str = expanded.as_ref();

        while !rest.is_empty() {
            let remaining = width.saturating_sub(current_width);
            // `display_col_to_byte` 落在宽字符/tab 展开空白中间时往前退一格，
            // 退到 0 就说明 `rest` 打头的视觉单元连 `remaining` 列都放不下；
            // 整段比 `remaining` 窄时它返回 `rest.len()`，于是自然结束本 span。
            let cut = if remaining == 0 {
                0
            } else {
                display_col_to_byte(rest, remaining)
            };

            if cut == 0 {
                if current_width > 0 {
                    sink(Piece::Break);
                    rows = rows.saturating_add(1);
                    current_width = 0;
                    continue;
                }
                // 空行上单个 grapheme 自身就宽于 `width`（极窄终端遇 CJK/emoji）：
                // 只能整簇塞进这一行（该行因此超宽），否则永远推进不了。
                let forced = force_cut(rest, width);
                let (head, tail) = rest.split_at(forced);
                sink(Piece::Text {
                    style: span.style,
                    text: head,
                });
                current_width = visible_width(head);
                rest = tail;
                continue;
            }

            let (head, tail) = rest.split_at(cut);
            sink(Piece::Text {
                style: span.style,
                text: head,
            });
            current_width += visible_width(head);
            rest = tail;
            if !rest.is_empty() {
                sink(Piece::Break);
                rows = rows.saturating_add(1);
                current_width = 0;
            }
        }
    }

    rows
}

/// 找到能整簇纳入 `rest` 打头那个超宽视觉单元的最小切点，恒 `> 0`。
///
/// 逐列上探而不是直接取整段：一条超长行里只有首个 grapheme 超宽时，
/// 取整段会把后面全部内容也挤进这一行。上探被 `rest` 的可见宽度封顶，
/// 触顶就退回整段——保证终止，且任何情况下都不丢内容。
fn force_cut(rest: &str, width: usize) -> usize {
    let limit = visible_width(rest).max(width).saturating_add(1);
    let mut probe = width.saturating_add(1);
    while probe <= limit {
        let cut = display_col_to_byte(rest, probe);
        if cut > 0 {
            return cut;
        }
        probe = probe.saturating_add(1);
    }
    rest.len()
}

/// 把 `current` 中攒的 span 收成一条 `Line`，携带原行的 `style`/`alignment`，
/// 并清空 `current` 供下一行继续攒。
fn finish_row(line: &Line<'_>, current: &mut Vec<Span<'static>>) -> Line<'static> {
    Line {
        style: line.style,
        alignment: line.alignment,
        spans: std::mem::take(current),
    }
}

/// 深拷贝成 `'static`，用于把借用行存进账本。
#[must_use]
pub fn line_to_static(line: &Line<'_>) -> Line<'static> {
    Line {
        style: line.style,
        alignment: line.alignment,
        spans: line
            .spans
            .iter()
            .map(|span| Span {
                style: span.style,
                content: span.content.to_string().into(),
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use ratatui::style::{Color, Style};
    use ratatui::text::{Line, Span};

    use super::{line_rows, line_to_static, line_width, wrap_line};

    #[test]
    fn ascii_single_span_splits_into_expected_segments() {
        let line = Line::from(Span::raw("abcdefghij"));
        let wrapped = wrap_line(&line, 4);
        let contents: Vec<String> = wrapped
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        assert_eq!(contents, vec!["abcd", "efgh", "ij"]);
    }

    #[test]
    fn cjk_never_splits_a_wide_char_and_stays_within_width() {
        // "中文" 每字宽 2 列，"abc" 每字宽 1 列，总宽 4 + 3 = 7。
        let line = Line::from(Span::raw("中文abc"));
        let wrapped = wrap_line(&line, 5);
        // 精确锁定切分结果：第一行贪心吞到刚好 5 列（中 2 + 文 2 + a 1），
        // 'b' 会把宽度推到 6 才触发换行，第二行剩 "bc"。
        let contents: Vec<String> = wrapped
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        assert_eq!(contents, vec!["中文a", "bc"]);
        for row in &wrapped {
            assert!(line_width(row) <= 5, "row {row:?} exceeds width 5");
        }
        // 拼回原文：证明没有字符被丢弃或重复，也就没有宽字符被腰斩成半个再拼错。
        let joined: String = wrapped
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(joined, "中文abc");
    }

    #[test]
    fn split_inside_span_preserves_style_on_both_halves() {
        let style = Style::default().fg(Color::Red);
        let line = Line::from(Span::styled("abcdef", style));
        let wrapped = wrap_line(&line, 4);
        assert_eq!(wrapped.len(), 2);
        for row in &wrapped {
            for span in &row.spans {
                assert_eq!(span.style, style);
            }
        }
        assert_eq!(wrapped[0].spans[0].content.as_ref(), "abcd");
        assert_eq!(wrapped[1].spans[0].content.as_ref(), "ef");
    }

    #[test]
    fn empty_line_wraps_to_single_empty_line() {
        let line = Line::default();
        let wrapped = wrap_line(&line, 10);
        assert_eq!(wrapped.len(), 1);
        assert!(wrapped[0].spans.is_empty());

        let line_with_empty_span = Line::from(Span::raw(""));
        let wrapped = wrap_line(&line_with_empty_span, 10);
        assert_eq!(wrapped.len(), 1);
    }

    /// `line_rows` 与 `wrap_line().len()` 必须逐例相等——`insert_history` 靠它推进
    /// viewport 锚点与上报注入行数，差一行就是光标错位加历史行被覆盖。
    ///
    /// 用例刻意包含旧的纯算术公式会算错的形状：全 2 列宽字符配奇数 `width`
    /// （`"界界界"` @ 3 实际 3 行、算术公式 2 行）、宽窄混排、切分点落在 span 内部、
    /// 以及 tab 展开后才知道真实宽度的行。
    #[test]
    fn line_rows_agrees_with_wrap_line_on_every_shape() {
        let cases: Vec<Line<'static>> = vec![
            Line::from(Span::raw("界界界")),
            Line::from(Span::raw("中文abc")),
            Line::from(Span::raw("abcdefghij")),
            Line::from(vec![
                Span::raw("hello "),
                Span::raw("world, this is zcode-tui"),
            ]),
            Line::from(vec![Span::raw("界"), Span::raw("a"), Span::raw("文字")]),
            Line::from(vec![Span::raw("ab"), Span::raw("界")]),
            Line::from(vec![Span::raw("abc"), Span::raw("界界"), Span::raw("d")]),
            Line::from(Span::raw("\tindented\ttext")),
            Line::from(Span::raw("👨‍👩‍👧‍👦 family")),
            Line::default(),
        ];
        for line in &cases {
            for width in [0usize, 1, 2, 3, 4, 5, 7, 8, 16, 64] {
                assert_eq!(
                    line_rows(line, width),
                    wrap_line(line, width).len(),
                    "line={line:?} width={width}"
                );
            }
        }
    }

    /// 锁死两个具体反例，防止有人把切分退回"算术公式"或"剩余列放不下就强塞"。
    #[test]
    fn wide_chars_never_overflow_a_row_that_already_has_content() {
        // 全宽字符配奇数宽度：每行只放得下一个字。算术公式会给 2。
        let repeated = Line::from(Span::raw("界界界"));
        assert_eq!(line_width(&repeated), 6);
        assert_eq!(line_rows(&repeated, 3), 3);
        assert_eq!(wrap_line(&repeated, 3).len(), 3);

        // 跨 span：`界` 放不进 "ab" 之后剩下的 1 列，必须先换行。
        // 强塞的话我们报 1 行，而终端会在打印 `界` 前自己 soft-wrap 成 2 行。
        let across = Line::from(vec![Span::raw("ab"), Span::raw("界")]);
        let wrapped = wrap_line(&across, 3);
        assert_eq!(line_rows(&across, 3), 2);
        assert_eq!(wrapped.len(), 2);
        for row in &wrapped {
            assert!(line_width(row) <= 3, "row {row:?} 超出宽度 3");
        }

        // 唯一允许超宽的形状：空行上单个 grapheme 自身就宽于整个 width。
        let too_wide = Line::from(Span::raw("界a"));
        let wrapped = wrap_line(&too_wide, 1);
        assert_eq!(line_rows(&too_wide, 1), wrapped.len());
        assert_eq!(line_width(&wrapped[0]), 2, "超宽 grapheme 必须整簇留在首行");
    }

    #[test]
    fn line_to_static_deep_copies_and_keeps_style_and_alignment() {
        let style = Style::default().fg(Color::Blue);
        let line = Line::from(Span::styled("borrowed", style))
            .alignment(ratatui::layout::Alignment::Right);
        let owned = line_to_static(&line);
        assert_eq!(owned.alignment, line.alignment);
        assert_eq!(owned.spans[0].style, style);
        assert_eq!(owned.spans[0].content.as_ref(), "borrowed");
    }
}
