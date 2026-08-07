//! 多行输入框：文本缓冲、光标移动、按显示宽度换行、bracketed paste 插入。
//!
//! 宽度计算一律经 `zcode_text::width`，绝不用 `str::len()`——`rule://zcode-architecture`
//! 「TUI 输出清理」与本任务的硬约束都点名了这一条。光标在字符（Unicode 标量值）
//! 边界上移动，不做完整 grapheme 簇聚类：本 crate 未引入 `unicode-segmentation`，
//! 对 ZWJ 家族 emoji 之类的多码点簇，光标会在簇内部停留，这是已知的精度取舍，
//! 不是遗漏（多数终端编辑器对纯 ASCII/CJK 输入已经够用）。

use std::ops::Range;

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use zcode_text::width::{display_col_to_byte, visible_width};

use crate::app::ids::INPUT_COMPONENT;
use zcode_tui::card::{Card, Section, card_content_width, render_card};
use zcode_tui::theme::Theme;
use zcode_tui::{Component, ComponentId};

/// 按字节区间取子串，越界或落在非法边界时退回空串而不是 panic
/// （`clippy::indexing_slicing` 是 deny 级，禁止裸 `&s[a..b]`）。
fn slice(s: &str, range: Range<usize>) -> &str {
    s.get(range).unwrap_or("")
}

/// 一条视觉行覆盖的字节区间 `[start, end)`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct VisualRow {
    start: usize,
    end: usize,
}

/// 把整段输入文本按 `width` 拆成若干视觉行的字节区间。
///
/// 先按源文本里的 `\n` 拆成逻辑行，每条逻辑行再交给 [`zcode_tui::wrap`] 按显示
/// 宽度硬换行；空文本与空逻辑行都产生恰好一条视觉行（否则光标在空行上无处安放）。
fn visual_rows(text: &str, width: usize) -> Vec<VisualRow> {
    let mut rows = Vec::new();
    let mut offset = 0usize;
    loop {
        let logical_end = slice(text, offset..text.len())
            .find('\n')
            .map_or(text.len(), |rel| offset + rel);
        let logical = slice(text, offset..logical_end);
        let wrapped = zcode_tui::wrap::wrap_line(&Line::raw(logical.to_owned()), width);
        let wrapped_count = wrapped.len();
        let mut cursor = offset;
        for (i, line) in wrapped.iter().enumerate() {
            let rendered: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            let row_len = rendered.len();
            let is_last_wrapped = i + 1 == wrapped_count;
            let end = if is_last_wrapped {
                logical_end
            } else {
                cursor.saturating_add(row_len)
            };
            rows.push(VisualRow { start: cursor, end });
            cursor = end;
        }
        if logical_end >= text.len() {
            break;
        }
        offset = logical_end.saturating_add(1); // 跳过 '\n'
    }
    rows
}

/// 多行输入框的状态：原始 UTF-8 文本 + 字节偏移光标。
#[derive(Debug, Clone, Default)]
pub(crate) struct InputState {
    text: String,
    cursor: usize,
    /// 单调递增版本号，供 [`InputComponent`] 判定是否需要重渲染。
    revision: u64,
}

impl InputState {
    /// 是否为空（提交前的判断依据）。
    #[must_use]
    pub(crate) fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// 取出全部文本并清空，供提交时使用。
    pub(crate) fn take(&mut self) -> String {
        self.cursor = 0;
        self.revision = self.revision.saturating_add(1);
        std::mem::take(&mut self.text)
    }

    /// 清空但不返回内容（Esc 清输入）。
    pub(crate) fn clear(&mut self) {
        if !self.text.is_empty() {
            self.text.clear();
            self.cursor = 0;
            self.revision = self.revision.saturating_add(1);
        }
    }

    /// 在光标处插入一段文本（含多行，用于粘贴）。
    pub(crate) fn insert(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        self.text.insert_str(self.cursor, s);
        self.cursor = self.cursor.saturating_add(s.len());
        self.revision = self.revision.saturating_add(1);
    }

    /// 在光标处插入单个字符。
    pub(crate) fn insert_char(&mut self, c: char) {
        let mut buf = [0u8; 4];
        self.insert(c.encode_utf8(&mut buf));
    }

    /// 退格：删除光标前一个字符。
    pub(crate) fn backspace(&mut self) {
        let Some(prev) = prev_char_boundary(&self.text, self.cursor) else {
            return;
        };
        self.text.drain(prev..self.cursor);
        self.cursor = prev;
        self.revision = self.revision.saturating_add(1);
    }

    /// 前向删除：删除光标后一个字符。
    pub(crate) fn delete_forward(&mut self) {
        let Some(next) = next_char_boundary(&self.text, self.cursor) else {
            return;
        };
        self.text.drain(self.cursor..next);
        self.revision = self.revision.saturating_add(1);
    }

    /// 光标左移一个字符。
    pub(crate) fn move_left(&mut self) {
        if let Some(prev) = prev_char_boundary(&self.text, self.cursor) {
            self.cursor = prev;
        }
    }

    /// 光标右移一个字符。
    pub(crate) fn move_right(&mut self) {
        if let Some(next) = next_char_boundary(&self.text, self.cursor) {
            self.cursor = next;
        }
    }

    /// 光标移到行首（当前视觉行，按 `width` 计算）。
    pub(crate) fn move_line_start(&mut self, width: usize) {
        let rows = visual_rows(&self.text, width.max(1));
        if let Some(row) = current_row(&rows, self.cursor) {
            self.cursor = row.start;
        }
    }

    /// 光标移到行尾（当前视觉行，按 `width` 计算）。
    pub(crate) fn move_line_end(&mut self, width: usize) {
        let rows = visual_rows(&self.text, width.max(1));
        if let Some(row) = current_row(&rows, self.cursor) {
            self.cursor = row.end;
        }
    }

    /// 光标上移一个视觉行，尽量保持显示列不变。
    pub(crate) fn move_up(&mut self, width: usize) {
        self.move_vertical(width, -1);
    }

    /// 光标下移一个视觉行，尽量保持显示列不变。
    pub(crate) fn move_down(&mut self, width: usize) {
        self.move_vertical(width, 1);
    }

    fn move_vertical(&mut self, width: usize, delta: i32) {
        let width = width.max(1);
        let rows = visual_rows(&self.text, width);
        let Some(idx) = row_index_at(&rows, self.cursor) else {
            return;
        };
        let target = if delta < 0 {
            idx.checked_sub(1)
        } else {
            (idx.saturating_add(1) < rows.len()).then_some(idx.saturating_add(1))
        };
        let Some(target) = target else {
            return;
        };
        let Some(current) = rows.get(idx) else {
            return;
        };
        let col = visible_width(slice(&self.text, current.start..self.cursor));
        let Some(row) = rows.get(target) else {
            return;
        };
        let row_text = slice(&self.text, row.start..row.end);
        self.cursor = row.start.saturating_add(display_col_to_byte(row_text, col));
    }

    /// 渲染成若干条 `Line`，并给出光标在该渲染结果里的 `(视觉行号, 显示列)`。
    ///
    /// 供 [`InputComponent::render`] 与终端光标定位共用同一份换行计算，
    /// 保证"看到的换行"与"光标落点"永远一致。
    #[must_use]
    pub(crate) fn render(&self, width: u16) -> (Vec<Line<'static>>, (usize, usize)) {
        let width_usize = usize::from(width.max(1));
        let rows = visual_rows(&self.text, width_usize);
        let lines = rows
            .iter()
            .map(|row| Line::raw(slice(&self.text, row.start..row.end).to_owned()))
            .collect();
        let cursor_row = row_index_at(&rows, self.cursor).unwrap_or(0);
        let col = rows.get(cursor_row).map_or(0, |row| {
            visible_width(slice(&self.text, row.start..self.cursor))
        });
        (lines, (cursor_row, col))
    }
}

fn current_row(rows: &[VisualRow], cursor: usize) -> Option<VisualRow> {
    row_index_at(rows, cursor).and_then(|i| rows.get(i).copied())
}

fn row_index_at(rows: &[VisualRow], cursor: usize) -> Option<usize> {
    if rows.is_empty() {
        return None;
    }
    for (i, row) in rows.iter().enumerate() {
        let is_last = i.saturating_add(1) == rows.len();
        if cursor < row.end || (is_last && cursor <= row.end) {
            return Some(i);
        }
    }
    Some(rows.len().saturating_sub(1))
}

fn prev_char_boundary(s: &str, from: usize) -> Option<usize> {
    if from == 0 {
        return None;
    }
    let mut i = from.saturating_sub(1);
    while i > 0 && !s.is_char_boundary(i) {
        i = i.saturating_sub(1);
    }
    Some(i)
}

fn next_char_boundary(s: &str, from: usize) -> Option<usize> {
    if from >= s.len() {
        return None;
    }
    let mut i = from.saturating_add(1);
    while i < s.len() && !s.is_char_boundary(i) {
        i = i.saturating_add(1);
    }
    Some(i)
}

/// 输入框在 transcript 里的组件外观：圆角边框 + 多行文本 + 反显光标。
///
/// 边框形态与 oh-my-pi 的主编辑器一致：UI 外框一律用 `boxRound`
/// （`packages/tui/src/components/editor.ts:866`）。边框色**是模式指示器**——上游
/// 按 bash / python / thinking 档位换色（`modes/interactive-mode.ts:1723-1747`）；
/// 本仓目前只有「有没有焦点」这一个维度，因此聚焦用 `borderAccent`、失焦用
/// `borderMuted`，等这些模式落地了再按同一条优先级链扩展。
///
/// **不画 prompt gutter。** 上游的 `setPromptGutter("> ")` 两处入口都以
/// `if (borderVisible) return` 开头（`editor.ts:710,720`）：有边框时提示符是多余的
/// 第二重「这里可以输入」信号，只会吃掉一列内容宽。
#[derive(Debug)]
pub(crate) struct InputComponent<'a> {
    state: &'a InputState,
    focused: bool,
    theme: &'a Theme,
}

impl<'a> InputComponent<'a> {
    /// 包一层 `&InputState`，供 [`zcode_tui::Composer::compose`] 调用。`focused`
    /// 决定是否在光标处画反显高亮——[`zcode_tui`] 的 `Emitter` 不驱动真实终端
    /// 光标定位（`draw_window` 从不调用 `Frame::set_cursor_position`），所以
    /// "光标"在这里是渲染出来的一个反色字符，不是终端原生光标。
    pub(crate) fn new(state: &'a InputState, focused: bool, theme: &'a Theme) -> Self {
        Self {
            state,
            focused,
            theme,
        }
    }
}

impl Component for InputComponent<'_> {
    fn id(&self) -> ComponentId {
        INPUT_COMPONENT
    }

    fn revision(&self) -> u64 {
        // 把 focused 折进 revision：焦点切换但文本未变时也要强制重渲染，
        // 否则 Composer 会复用上一帧的缓存行，光标高亮与边框色不会跟着变。
        self.state
            .revision
            .saturating_mul(2)
            .saturating_add(u64::from(self.focused))
    }

    fn render(&self, width: u16) -> Vec<Line<'static>> {
        let inner = card_content_width(width, INPUT_PADDING, INPUT_PADDING);
        let (mut lines, (cursor_row, cursor_col)) = self.state.render(inner);
        if self.focused
            && let Some(line) = lines.get_mut(cursor_row)
        {
            *line = highlight_cursor(line, cursor_col);
        }
        let border = if self.focused {
            self.theme.colors.border_accent
        } else {
            self.theme.colors.border_muted
        };
        let sections = [Section {
            label: None,
            lines: &lines,
        }];
        render_card(
            &Card {
                header: None,
                sections: &sections,
                border: Style::default().fg(border),
                bg: None,
                padding_left: INPUT_PADDING,
                padding_right: INPUT_PADDING,
            },
            width,
            self.theme,
        )
    }
}

/// 输入框内容区的左右内边距（列）。与 transcript 侧的 `CONTENT_PADDING` 同为 1，
/// 让输入文字与上方消息正文的左边缘对齐（边框各占 1 列，正好抵掉气泡的背景边）。
pub(crate) const INPUT_PADDING: u16 = 1;

/// 把 `line` 在显示列 `col` 处的字符反色，模拟一个终端光标。`col` 落在行尾
/// （行内没有对应字符）时插入一个反色空格，保证行尾光标也能看见。
fn highlight_cursor(line: &Line<'static>, col: usize) -> Line<'static> {
    let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    let start = display_col_to_byte(&text, col);
    let before = slice(&text, 0..start);
    let rest = slice(&text, start..text.len());
    let mut chars = rest.chars();
    let (cursor_char, after) = match chars.next() {
        Some(c) => {
            let consumed = c.len_utf8();
            (c.to_string(), slice(rest, consumed..rest.len()).to_owned())
        }
        None => (" ".to_owned(), String::new()),
    };
    let reversed = Style::default().add_modifier(Modifier::REVERSED);
    Line::from(vec![
        Span::raw(before.to_owned()),
        Span::styled(cursor_char, reversed),
        Span::raw(after),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_and_backspace_roundtrip() {
        let mut input = InputState::default();
        input.insert("hello");
        assert_eq!(input.text, "hello");
        input.backspace();
        assert_eq!(input.text, "hell");
    }

    #[test]
    fn backspace_on_empty_is_noop() {
        let mut input = InputState::default();
        input.backspace();
        assert_eq!(input.text, "");
    }

    #[test]
    fn multiline_paste_wraps_and_cursor_lands_at_end() {
        let mut input = InputState::default();
        input.insert("first line\nsecond line");
        let (lines, (row, col)) = input.render(80);
        assert_eq!(lines.len(), 2);
        assert_eq!(row, 1);
        assert_eq!(col, "second line".len());
    }

    #[test]
    fn narrow_width_hard_wraps_a_single_logical_line() {
        let mut input = InputState::default();
        input.insert("abcdefghij");
        let (lines, _) = input.render(4);
        assert_eq!(lines.len(), 3); // 4+4+2
    }

    #[test]
    fn move_left_right_stays_on_char_boundaries_with_wide_chars() {
        let mut input = InputState::default();
        input.insert("a界b");
        input.move_left();
        input.move_left();
        input.move_left();
        // 已经在开头，再移不应 panic 或越界。
        input.move_left();
        assert_eq!(input.cursor, 0);
        input.move_right();
        input.move_right();
        input.move_right();
        assert_eq!(input.cursor, "a界b".len());
    }

    #[test]
    fn vertical_movement_preserves_display_column() {
        let mut input = InputState::default();
        input.insert("abcdef\nxy");
        input.move_up(80); // 从第二行末尾（列 2）上移到第一行
        let (_, (row, col)) = input.render(80);
        assert_eq!(row, 0);
        assert_eq!(col, 2);
    }

    #[test]
    fn clear_resets_cursor() {
        let mut input = InputState::default();
        input.insert("hi");
        input.move_left();
        input.clear();
        assert!(input.is_empty());
        assert_eq!(input.cursor, 0);
    }

    /// 光标处的字符被反显。输入框现在有边框，所以内容在第 2 行（第 1 行是顶边）。
    #[test]
    fn focused_input_highlights_cursor_char() {
        let theme = crate::app::test_theme();
        let mut input = InputState::default();
        input.insert("abc");
        input.move_left(); // 光标落在 'c' 前
        let lines = InputComponent::new(&input, true, &theme).render(80);
        let reversed: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .filter(|s| s.style.add_modifier.contains(Modifier::REVERSED))
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(reversed, "c", "只有光标那一个字符被反显：{lines:?}");
    }

    #[test]
    fn unfocused_input_has_no_highlight() {
        let theme = crate::app::test_theme();
        let mut input = InputState::default();
        input.insert("abc");
        let lines = InputComponent::new(&input, false, &theme).render(80);
        assert!(
            !lines
                .iter()
                .flat_map(|l| l.spans.iter())
                .any(|s| s.style.add_modifier.contains(Modifier::REVERSED)),
            "失焦时不该画光标：{lines:?}"
        );
    }

    /// 输入框是圆角卡片：三行（顶边 + 内容 + 底边），每行恰好占满宽度，
    /// 焦点用边框色区分。行宽不齐会让右边框参差，是最显眼的视觉缺陷。
    #[test]
    fn input_is_a_rounded_card_with_focus_colored_border() {
        let theme = crate::app::test_theme();
        let input = InputState::default();
        for (focused, expected) in [
            (true, theme.colors.border_accent),
            (false, theme.colors.border_muted),
        ] {
            let lines = InputComponent::new(&input, focused, &theme).render(40);
            assert_eq!(lines.len(), 3, "空输入框应是顶边 + 一行内容 + 底边");
            for line in &lines {
                assert_eq!(
                    zcode_tui::wrap::line_width(line),
                    40,
                    "输入框每行必须铺满整宽：{line:?}"
                );
            }
            let top = lines.first().map(|l| l.spans.clone()).unwrap_or_default();
            let corner = top
                .iter()
                .find(|s| s.content.contains(theme.symbols.box_round.top_left));
            assert_eq!(
                corner.map(|s| s.style.fg),
                Some(Some(expected)),
                "focused={focused} 时边框色不对：{top:?}"
            );
        }
    }

    #[test]
    fn focus_change_alone_bumps_component_revision() {
        let theme = crate::app::test_theme();
        let input = InputState::default();
        let unfocused = InputComponent::new(&input, false, &theme);
        let focused = InputComponent::new(&input, true, &theme);
        assert_ne!(unfocused.revision(), focused.revision());
    }
}
