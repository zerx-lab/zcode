//! 卡片（圆角边框容器）与工具输出块的单行标题头。
//!
//! 对标 oh-my-pi `packages/coding-agent/src/tui/output-block.ts`（卡片本体）与
//! `packages/coding-agent/src/tui/status-line.ts`（标题头）。二者在上游都是
//! 拼接烘焙好的 ANSI 字符串；本模块吐 [`Line`]/[`Span`]，颜色由调用点组装的
//! [`Style`] 承载，不做字符串拼接式的样式补丁——这类补丁在 ratatui 下不需要，
//! 理由见 `crate::theme` 模块文档「与上游最重要的三处偏离」。
//!
//! 渲染函数全同步、无副作用：只吃参数吐 `Vec<Line<'static>>`，宽度全部经
//! `zcode_text::width`，换行复用 [`crate::wrap`]，不另起一套。

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use zcode_text::width::{display_col_to_byte, visible_width};

use crate::theme::Theme;
use crate::wrap::{line_to_static, line_width};

/// [`render_status_line`] 的输入：工具输出块顶部的单行标题头。
///
/// 对应 oh-my-pi `packages/coding-agent/src/tui/status-line.ts:8-19` 的
/// `StatusLineOptions`。**不是**底部状态栏（那部分是本批次的非目标）。
#[derive(Debug)]
pub struct StatusLine<'a> {
    /// 头部图标（已带好样式，例如 `theme.symbols.status.success` 配成功色）。
    /// `None` 时**不加前导空格**——`status-line.ts:37` 用三元表达式在图标缺失时
    /// 直接返回裸标题，本仓同理，否则无图标的头行会比有图标的头行整体右移一列。
    pub icon: Option<Span<'a>>,
    /// 主标题文本，渲染前会先经换行压平（见模块内 [`flatten_header_text`]）。
    pub title: &'a str,
    /// 主标题颜色。调用点按状态自行选取，惯例是 `theme.accent()`
    /// （`status-line.ts:35` 的默认值 `"accent"`）。
    pub title_style: Style,
    /// 描述文本，渲染时自动加前缀 `": "`（不着色，与 `status-line.ts:40` 一致：
    /// 冒号本身是裸字符串拼接，只有描述内容被 `theme.fg("muted", …)` 包住）。
    pub description: Option<&'a str>,
    /// 徽章：文本 + 颜色，渲染成 `⟦text⟧`（括号取 `theme.symbols.format.bracket_left`/
    /// `bracket_right`），对应 `status-line.ts:43-46`。
    pub badge: Option<(&'a str, Style)>,
    /// 尾部元信息条目，渲染前逐项换行压平并**过滤空白项**
    /// （`status-line.ts:48` 的 `.filter(value => value.trim().length > 0)`），
    /// 非空时用 `theme.dim()` 整体着色、`theme.symbols.sep.dot` 连接
    /// （该分隔符自带两侧空格，见 `crate::theme::symbols::SepSymbols::dot`）。
    pub meta: &'a [&'a str],
}

/// 把 `\r\n` / `\r` / `\n` 全部替换成单个空格。
///
/// 对应 oh-my-pi `packages/coding-agent/src/tui/status-line.ts:28-30` 的
/// `flattenForHeader`：内嵌换行会把单行标题头撑成多行，直接破坏它所在的
/// 带边框输出块（多出来的物理行没有边框，视觉上就是断裂的卡片）。
fn flatten_header_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push(' ');
            }
            '\n' => out.push(' '),
            other => out.push(other),
        }
    }
    out
}

/// 渲染工具输出块的单行标题头。
///
/// 对应 oh-my-pi `packages/coding-agent/src/tui/status-line.ts:32-54` 的
/// `renderStatusLine`；返回值不带尾部换行，调用方通常把它作为
/// [`Card`] 的 `header` 或 `sections[0].label`。
#[must_use]
pub fn render_status_line(s: &StatusLine<'_>, theme: &Theme) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();

    if let Some(icon) = &s.icon {
        spans.push(Span::styled(icon.content.to_string(), icon.style));
        spans.push(Span::raw(" "));
    }
    spans.push(Span::styled(flatten_header_text(s.title), s.title_style));

    if let Some(description) = s.description {
        // 冒号裸拼接，只有描述文本本身着色，对应 status-line.ts:40。
        spans.push(Span::raw(": "));
        spans.push(Span::styled(
            flatten_header_text(description),
            theme.muted(),
        ));
    }

    if let Some((label, style)) = s.badge {
        spans.push(Span::raw(" "));
        let bracketed = format!(
            "{}{}{}",
            theme.symbols.format.bracket_left,
            flatten_header_text(label),
            theme.symbols.format.bracket_right
        );
        spans.push(Span::styled(bracketed, style));
    }

    let meta: Vec<String> = s
        .meta
        .iter()
        .map(|m| flatten_header_text(m))
        .filter(|m| !m.trim().is_empty())
        .collect();
    if !meta.is_empty() {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(meta.join(theme.symbols.sep.dot), theme.dim()));
    }

    Line::from(spans)
}

/// [`Card`] 的一个内容分节。
///
/// 对应 oh-my-pi `packages/coding-agent/src/tui/output-block.ts:16` 的
/// `sections` 数组元素（`{ label?, lines, separator? }`）；本仓省去 `separator`
/// 字段——一个分节要不要画分隔行完全由 `label` 是否存在决定，`separator: true`
/// 但 `label` 为空的「纯分隔线」用法在本批次范围内没有调用点，不预先造接口。
#[derive(Debug)]
pub struct Section<'a> {
    /// 分节标签，嵌在分隔行上（`├─ label ─…─┤`）。`None` 时不画分隔行——
    /// 典型用法是首节紧跟在顶边框之后，再画一条分隔线只会跟顶边框重复。
    pub label: Option<Line<'a>>,
    /// 该节的内容行。调用方已自行用 [`crate::wrap::wrap_line`] 按
    /// [`card_content_width`] 换好行；本模块只做防御性硬截断，不在这里重新 wrap。
    pub lines: &'a [Line<'static>],
}

/// 圆角边框卡片。
///
/// 对应 oh-my-pi `packages/coding-agent/src/tui/output-block.ts:12-24` 的
/// `OutputBlockOptions`。省去的字段：`applyBg`（本仓用 `bg: Option<Color>`
/// 表达同一个开关）、`borderColor`（`border: Style` 已经是调用点算好的最终颜色，
/// 状态到颜色的映射是调用方的职责，不是绘制原语的职责）。
#[derive(Debug)]
pub struct Card<'a> {
    /// 嵌在顶边框上的标题行，保留调用方给的逐 span 样式（不会被 `border` 覆盖）。
    pub header: Option<Line<'a>>,
    /// 内容分节，至少要有一个（`sections` 为空时卡片只有上下边框、没有内容行）。
    pub sections: &'a [Section<'a>],
    /// 边框颜色：四角、横竖线、分隔行的 T 型交叉都用它。
    pub border: Style,
    /// 整张卡片铺的背景色，包括边框、padding 补白与内容行右侧补白。
    /// `None` 时不铺背景，各行只在有内容的列上带样式。
    pub bg: Option<Color>,
    /// 内容区左缩进列数。调用点约定默认 1（对应 `output-block.ts:45-48` 的
    /// `normalizeContentPaddingLeft` 缺省值），本结构体字段不隐式给默认值。
    pub padding_left: u16,
    /// 内容区右缩进列数。调用点约定默认与 `padding_left` 相同
    /// （`output-block.ts:62` 的 `contentPaddingRight ?? contentPaddingLeft`）。
    pub padding_right: u16,
}

/// 卡片在给定外宽 `width` 下，是否还画得下边框。
///
/// 两侧竖线各占 1 列，加上左右 padding，至少还要再留 1 列给内容——否则整条边框
/// 会吞掉全部可用宽度。对应 oh-my-pi `packages/tui/src/components/box.ts:124`
/// 的 `width - 2 >= paddingX * 2 + 1`（这里把对称的 `paddingX * 2` 换成非对称的
/// `padding_left + padding_right`，其余等价）。
fn card_has_border(width: u16, padding_left: u16, padding_right: u16) -> bool {
    let width = u32::from(width);
    let padding = u32::from(padding_left) + u32::from(padding_right);
    width >= padding.saturating_add(3)
}

/// 给调用方预先算内容区宽度，让它能先把内容 wrap 好再交进来。
///
/// 对应 oh-my-pi `packages/coding-agent/src/tui/output-block.ts:56-64` 的
/// `outputBlockContentWidth`，额外叠加了 [`card_has_border`] 的宽度守卫：
/// 边框被丢弃时不再扣两侧竖线的 2 列（`box.ts:125` 的
/// `innerWidth = border ? width - 2 : width` 同一处理），否则调用方按这个宽度
/// wrap 出来的内容会比 `render_card` 实际渲染的行窄，两侧留白对不齐。
#[must_use]
pub fn card_content_width(width: u16, padding_left: u16, padding_right: u16) -> u16 {
    let inner: u32 = if card_has_border(width, padding_left, padding_right) {
        u32::from(width).saturating_sub(2)
    } else {
        u32::from(width)
    };
    let content = inner
        .saturating_sub(u32::from(padding_left))
        .saturating_sub(u32::from(padding_right))
        .max(1);
    u16::try_from(content).unwrap_or(u16::MAX)
}

/// 按显示宽度截断一条 `Line`，超出时尾部换成 `ellipsis`。保留每个 span 的
/// `Style`；未超宽时零拷贝语义等价（仍会深拷贝成 `'static`，但内容不变）。
///
/// `ellipsis` 本身宽于 `width` 时退化为截断 `ellipsis` 自身（不保留任何原内容）；
/// `ellipsis` 为空串时等价于纯硬截断，`render_card` 的防御性截断路径用的就是这个。
#[must_use]
pub fn truncate_line(line: &Line<'_>, width: usize, ellipsis: &str) -> Line<'static> {
    let total = line_width(line);
    if total <= width {
        return line_to_static(line);
    }

    let ellipsis_width = visible_width(ellipsis);
    if ellipsis_width >= width {
        let style = line
            .spans
            .first()
            .map(|span| span.style)
            .unwrap_or_default();
        let byte_end = display_col_to_byte(ellipsis, width);
        let text = ellipsis.get(..byte_end).unwrap_or("").to_string();
        return Line {
            style: line.style,
            alignment: line.alignment,
            spans: vec![Span::styled(text, style)],
        };
    }

    let available = width - ellipsis_width;
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut used = 0usize;
    let mut last_style = line
        .spans
        .first()
        .map(|span| span.style)
        .unwrap_or_default();
    for span in &line.spans {
        if used >= available {
            break;
        }
        let content = span.content.as_ref();
        let span_width = visible_width(content);
        last_style = span.style;
        if used.saturating_add(span_width) <= available {
            spans.push(Span::styled(content.to_string(), span.style));
            used += span_width;
        } else {
            let remaining = available - used;
            let byte_end = display_col_to_byte(content, remaining);
            let text = content.get(..byte_end).unwrap_or("").to_string();
            spans.push(Span::styled(text, span.style));
            break;
        }
    }
    spans.push(Span::styled(ellipsis.to_string(), last_style));
    Line {
        style: line.style,
        alignment: line.alignment,
        spans,
    }
}

/// 把一条 `Line` 右侧补空格到 `width` 列，补白带 `style`。
/// 已经 `>= width` 时原样返回（不截断——截断是 [`truncate_line`] 的职责）。
#[must_use]
pub fn pad_line(mut line: Line<'static>, width: usize, style: Style) -> Line<'static> {
    let current = line_width(&line);
    let pad = width.saturating_sub(current);
    if pad > 0 {
        line.spans.push(Span::styled(" ".repeat(pad), style));
    }
    line
}

/// 给整条 `Line` 的所有 span 叠加一个样式，用于铺背景。
///
/// 用 [`Style`] 的 patch 语义（`other` 里 `Some` 的字段覆盖，`None` 的字段保留
/// `self` 原值）：调用点传入只设置了 `.bg(color)` 的 `style` 时，每个 span 原有的
/// 前景色与修饰符原样保留，只叠加背景色。
#[must_use]
pub fn patch_line(mut line: Line<'static>, style: Style) -> Line<'static> {
    line.spans = line
        .spans
        .into_iter()
        .map(|span| span.patch_style(style))
        .collect();
    line
}

/// 把一个已经拼好的边框/内容行钳制到恰好 `width` 列：超出则硬截断（不加省略号，
/// 因为这里只是宽度守卫的最后防线，视觉上的省略号截断已经在
/// [`render_bar_row`] 的 label 预算里做过一次），不足则用 `style` 补白。是
/// 「每一行显示宽度恰好等于 `width`」这个不变式的唯一兜底点：边框行的横线填充
/// 预算、内容行的 padding 都用 `saturating_sub` 算，极窄宽度（尤其是
/// padding 本身就已经吃满甚至超过 `width` 时）会算出不精确的中间结果，
/// 这里统一收口，而不是在每个中间步骤单独防御。
fn fit_to_width(line: Line<'static>, width: usize, style: Style) -> Line<'static> {
    let current = line_width(&line);
    if current > width {
        truncate_line(&line, width, "")
    } else {
        pad_line(line, width, style)
    }
}

/// 渲染一条边框横条：顶边框、分节分隔行、底边框共用同一套布局。
///
/// 对应 oh-my-pi `packages/coding-agent/src/tui/output-block.ts:155-174` 的
/// `renderBar`（`renderBottom` 复用同一形状，`:176-182`）：
/// `左角 + "───"(cap, :70) + [" " + label + " "] + 横线填充 + 右角`。
/// cap 无论有没有 `label` 都会拼上——`label` 为 `None` 时它只是填充横线的一部分，
/// 视觉上与后续横线无差异。
///
/// 两端的角**永远不许被牺牲**：宽度不够时先砍标题，再砍横线填充，角最后走。
/// 角一旦缺失，卡片看起来就像渲染崩了，而只断言行宽的测试完全抓不到这件事。
fn render_bar_row(
    left_corner: &str,
    right_corner: &str,
    label: Option<&Line<'_>>,
    width: usize,
    border: Style,
    theme: &Theme,
) -> Line<'static> {
    let h = theme.symbols.box_round.horizontal;
    let cap = h.repeat(3);
    let left_glyphs = format!("{left_corner}{cap}");
    let left_w = visible_width(&left_glyphs);
    let right_w = visible_width(right_corner);

    let mut spans = vec![Span::styled(left_glyphs, border)];
    match label {
        None => {
            let fill = width.saturating_sub(left_w).saturating_sub(right_w);
            spans.push(Span::styled(h.repeat(fill), border));
        }
        Some(label_line) => {
            // 预算里**必须含 label 两侧那对空格**——这正是 omp 的算法：它把
            // `rawLabel = " " + labelText + " "` 整体交给
            // `truncateToWidth(rawLabel, lineWidth - leftWidth - rightWidth)`
            // （`output-block.ts:165-171`）。
            //
            // 早先的写法是先从预算里扣掉 2、截断 label、再无条件补两个空格：
            // `width` 只有 5-6 列时（`card_has_border` 的门槛就是 5）预算被 clamp 到 0，
            // 补上的两个空格仍然会把整行顶到 `width + 1`，收尾的 `fit_to_width` 于是
            // 砍掉最后一个字符——**右上角没了**，顶边变成 `╭───  `。
            // 断言行宽的测试抓不到它：行宽确实是对的，丢的是那个角。
            let mut framed = Vec::with_capacity(label_line.spans.len().saturating_add(2));
            framed.push(Span::raw(" "));
            framed.extend(label_line.spans.iter().cloned());
            framed.push(Span::raw(" "));
            let budget = width.saturating_sub(left_w).saturating_sub(right_w);
            let truncated =
                truncate_line(&Line::from(framed), budget, theme.symbols.format.ellipsis);
            let label_w = line_width(&truncated);
            spans.extend(truncated.spans);
            let fill = width
                .saturating_sub(left_w)
                .saturating_sub(label_w)
                .saturating_sub(right_w);
            spans.push(Span::styled(h.repeat(fill), border));
        }
    }
    spans.push(Span::styled(right_corner.to_string(), border));
    fit_to_width(Line::from(spans), width, border)
}

/// 一行内容在卡片里的几何：一次算好、逐行复用，省得把五个 `usize` 沿调用链传。
#[derive(Debug, Clone, Copy)]
struct RowGeometry {
    /// 内容区可用列数。
    content_width: usize,
    /// 左内边距列数。
    padding_left: usize,
    /// 右内边距列数。
    padding_right: usize,
    /// 是否画两侧竖线（宽度守卫可能整体丢弃边框）。
    has_border: bool,
    /// 整行必须占满的列数。
    width: usize,
}

/// 渲染一条内容行：`[竖线] [左 padding] 内容(补齐到 content_width) [右 padding] [竖线]`。
/// `has_border` 为假时省去两侧竖线（宽度守卫丢弃边框时仍保留 padding，
/// 对应 `box.ts:125-126` 的 `innerWidth = border ? width - 2 : width` 之后
/// `contentWidth = innerWidth - paddingX * 2` 仍然生效）。
fn render_content_row(
    content_line: &Line<'_>,
    geometry: &RowGeometry,
    border: Style,
    vertical: &str,
) -> Line<'static> {
    let RowGeometry {
        content_width,
        padding_left,
        padding_right,
        has_border,
        width,
    } = *geometry;
    let truncated = truncate_line(content_line, content_width, "");
    let padded = pad_line(truncated, content_width, Style::default());

    let mut spans: Vec<Span<'static>> = Vec::new();
    if has_border {
        spans.push(Span::styled(vertical.to_string(), border));
    }
    if padding_left > 0 {
        spans.push(Span::raw(" ".repeat(padding_left)));
    }
    spans.extend(padded.spans);
    if padding_right > 0 {
        spans.push(Span::raw(" ".repeat(padding_right)));
    }
    if has_border {
        spans.push(Span::styled(vertical.to_string(), border));
    }
    // 与 render_bar_row 同一道兜底：padding_left + content_width + padding_right
    // （加两侧竖线）理论上恒等于 width，但 padding 本身可能已经超过极窄宽度下
    // 的可用列数（card_content_width 对 content 恒留至少 1 列），这里统一钳制，
    // 保证「每一行显示宽度恰好等于 width」不因 padding 溢出而破例。
    fit_to_width(Line::from(spans), width, Style::default())
}

/// 渲染整张卡片：顶边框（可选嵌标题）、若干内容分节（分节前可选嵌标签的分隔行）、
/// 底边框。宽度守卫命中时（[`card_has_border`] 为假）整条边框连同 `header` 一起
/// 丢弃，只输出内容行——`header` 天然要嵌在顶边框里，没有边框就没有地方放它，
/// 对应 oh-my-pi `packages/tui/src/components/box.ts:117-126` 的宽度守卫思路，
/// 移植到「顶边框自带标题」这个 output-block.ts 独有的场景。
///
/// 每一条返回的 `Line` 显示宽度恒等于 `width`（[`fit_to_width`] / [`pad_line`]
/// 兜底），这是卡片不会撑破外层布局的根本不变式。
#[must_use]
pub fn render_card(card: &Card<'_>, width: u16, theme: &Theme) -> Vec<Line<'static>> {
    let has_border = card_has_border(width, card.padding_left, card.padding_right);
    let geometry = RowGeometry {
        content_width: usize::from(card_content_width(
            width,
            card.padding_left,
            card.padding_right,
        )),
        padding_left: usize::from(card.padding_left),
        padding_right: usize::from(card.padding_right),
        has_border,
        width: usize::from(width),
    };
    let width = geometry.width;
    let vertical = theme.symbols.box_round.vertical;

    let mut lines: Vec<Line<'static>> = Vec::new();

    if has_border {
        lines.push(render_bar_row(
            theme.symbols.box_round.top_left,
            theme.symbols.box_round.top_right,
            card.header.as_ref(),
            width,
            card.border,
            theme,
        ));
    }

    for section in card.sections {
        if has_border && let Some(label) = &section.label {
            lines.push(render_bar_row(
                theme.symbols.box_sharp.tee_right,
                theme.symbols.box_sharp.tee_left,
                Some(label),
                width,
                card.border,
                theme,
            ));
        }
        for content_line in section.lines {
            lines.push(render_content_row(
                content_line,
                &geometry,
                card.border,
                vertical,
            ));
        }
    }

    if has_border {
        lines.push(render_bar_row(
            theme.symbols.box_round.bottom_left,
            theme.symbols.box_round.bottom_right,
            None,
            width,
            card.border,
            theme,
        ));
    }

    if let Some(bg) = card.bg {
        let bg_style = Style::new().bg(bg);
        lines = lines
            .into_iter()
            .map(|line| patch_line(line, bg_style))
            .collect();
    }

    lines
}

#[cfg(test)]
mod tests {
    use ratatui::style::{Color, Modifier, Style};
    use ratatui::text::{Line, Span};

    use super::{
        Card, Section, StatusLine, card_content_width, card_has_border, render_card,
        render_status_line,
    };
    use crate::theme::{BuiltinTheme, ColorMode, SymbolPreset, Theme};
    use crate::wrap::line_width;

    fn theme() -> Theme {
        BuiltinTheme::Dark
            .load(ColorMode::TrueColor, SymbolPreset::Unicode)
            .expect("内置暗色主题必须能加载")
    }

    fn plain_line(text: &str) -> Line<'static> {
        Line::from(Span::raw(text.to_string()))
    }

    // ── render_card：宽度不变式 ──────────────────────────────────────────

    #[test]
    fn every_row_matches_exact_width_ascii() {
        let t = theme();
        let section = Section {
            label: None,
            lines: &[plain_line("hello")],
        };
        let card = Card {
            header: Some(plain_line("Title")),
            sections: &[section],
            border: t.accent(),
            bg: None,
            padding_left: 1,
            padding_right: 1,
        };
        for width in [1u16, 2, 3, 4, 5, 10, 20, 40] {
            let lines = render_card(&card, width, &t);
            for line in &lines {
                assert_eq!(
                    line_width(line),
                    usize::from(width),
                    "width={width} line={line:?}"
                );
            }
        }
    }

    #[test]
    fn every_row_matches_exact_width_with_cjk_content_and_wide_header() {
        let t = theme();
        // 内容行本身超宽（调用方没有先 wrap，模拟防御性硬截断路径）。
        let section = Section {
            label: None,
            lines: &[plain_line("中文内容超过宽度需要硬截断")],
        };
        let card = Card {
            // header 本身比整条卡片还宽，必须触发 output-block.ts:169 的
            // truncateToWidth 路径。
            header: Some(plain_line("一个非常非常长的标题用来测试截断行为超宽超宽")),
            sections: &[section],
            border: t.accent(),
            bg: None,
            padding_left: 1,
            padding_right: 1,
        };
        for width in [3u16, 8, 12, 16, 24, 30] {
            let lines = render_card(&card, width, &t);
            assert!(!lines.is_empty());
            for line in &lines {
                assert_eq!(
                    line_width(line),
                    usize::from(width),
                    "width={width} line={line:?}"
                );
            }
        }
    }

    #[test]
    fn narrow_width_drops_border_without_panicking() {
        let t = theme();
        let section = Section {
            label: Some(plain_line("sec")),
            lines: &[plain_line("x")],
        };
        let card = Card {
            header: Some(plain_line("H")),
            sections: &[section],
            border: t.accent(),
            bg: None,
            padding_left: 1,
            padding_right: 1,
        };
        // padding_left + padding_right + 3 = 5：宽度 < 5 时边框必须被丢弃。
        for width in [0u16, 1, 2, 3, 4] {
            let lines = render_card(&card, width, &t);
            for line in &lines {
                assert_eq!(line_width(line), usize::from(width));
                let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
                assert!(
                    !text.contains(t.symbols.box_round.top_left),
                    "width={width} 不应再画边框：{text:?}"
                );
            }
        }
    }

    /// 两端的角在**任何**宽度下都必须在。
    ///
    /// 回归：早先算 header 预算时先扣 2 再无条件补回两个空格，`width` 为 5-6
    /// （`card_has_border` 的门槛正是 5）时整行被顶到 `width + 1`，收尾的硬截断砍掉
    /// 右上角，顶边变成 `╭───  `。只断言行宽的测试抓不到——行宽当时也是对的。
    #[test]
    fn corners_survive_every_width_that_still_draws_a_border() {
        let t = theme();
        let lines_holder = [plain_line("x")];
        let section = Section {
            label: Some(plain_line("很长的分节标题")),
            lines: &lines_holder,
        };
        let sections = [section];
        for width in 5_u16..=40 {
            let card = Card {
                header: Some(plain_line("很长的标题文本 with ascii too")),
                sections: &sections,
                border: Style::default(),
                bg: None,
                padding_left: 1,
                padding_right: 1,
            };
            let rows = render_card(&card, width, &t);
            if !card_has_border(width, 1, 1) {
                continue;
            }
            let text: Vec<String> = rows
                .iter()
                .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
                .collect();
            let top = text.first().map(String::as_str).unwrap_or_default();
            let bottom = text.last().map(String::as_str).unwrap_or_default();
            assert!(
                top.starts_with(t.symbols.box_round.top_left)
                    && top.ends_with(t.symbols.box_round.top_right),
                "width={width} 顶边缺角：{top:?}"
            );
            assert!(
                bottom.starts_with(t.symbols.box_round.bottom_left)
                    && bottom.ends_with(t.symbols.box_round.bottom_right),
                "width={width} 底边缺角：{bottom:?}"
            );
            for (row, line) in rows.iter().enumerate() {
                assert_eq!(
                    line_width(line),
                    usize::from(width),
                    "width={width} 第 {row} 行宽度不对"
                );
            }
        }
    }

    #[test]
    fn zero_width_never_panics_and_yields_empty_rows() {
        let t = theme();
        let section = Section {
            label: None,
            lines: &[plain_line("content")],
        };
        let card = Card {
            header: Some(plain_line("H")),
            sections: &[section],
            border: t.accent(),
            bg: None,
            padding_left: 0,
            padding_right: 0,
        };
        let lines = render_card(&card, 0, &t);
        for line in &lines {
            assert_eq!(line_width(line), 0);
        }
    }

    // ── 分节分隔行形态 ────────────────────────────────────────────────

    #[test]
    fn section_separator_uses_tee_glyphs_and_embeds_label() {
        let t = theme();
        let first = Section {
            label: None,
            lines: &[plain_line("a")],
        };
        let second = Section {
            label: Some(plain_line("more")),
            lines: &[plain_line("b")],
        };
        let card = Card {
            header: Some(plain_line("H")),
            sections: &[first, second],
            border: t.accent(),
            bg: None,
            padding_left: 1,
            padding_right: 1,
        };
        let lines = render_card(&card, 40, &t);
        // 行序：顶边框、"a" 内容行、分隔行(second.label)、"b" 内容行、底边框。
        assert_eq!(lines.len(), 5);
        let sep_text: String = lines[2].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(sep_text.starts_with(t.symbols.box_sharp.tee_right));
        assert!(sep_text.trim_end().ends_with(t.symbols.box_sharp.tee_left));
        assert!(sep_text.contains("more"));
        // 首节没有 label，不应该在顶边框之后额外插入一条分隔行。
        let top_text: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(top_text.starts_with(t.symbols.box_round.top_left));
    }

    // ── bg 铺满每一格 ────────────────────────────────────────────────

    #[test]
    fn bg_patches_every_span_including_padding() {
        let t = theme();
        let section = Section {
            label: None,
            lines: &[plain_line("hi")],
        };
        let bg = Color::Rgb(10, 20, 30);
        let card = Card {
            header: Some(plain_line("H")),
            sections: &[section],
            border: t.accent(),
            bg: Some(bg),
            padding_left: 1,
            padding_right: 1,
        };
        let lines = render_card(&card, 20, &t);
        for line in &lines {
            for span in &line.spans {
                assert_eq!(span.style.bg, Some(bg), "span {span:?} 缺少背景色");
            }
        }
    }

    // ── card_content_width ──────────────────────────────────────────

    #[test]
    fn content_width_matches_rendered_row_budget() {
        // 有边框：width - 2 - pl - pr。
        assert_eq!(card_content_width(20, 1, 1), 16);
        // 边框被丢弃：不再扣两侧竖线的 2 列。
        assert_eq!(card_content_width(3, 1, 1), 1);
        // 恒 >= 1，不会返回 0 导致内容区消失。
        assert_eq!(card_content_width(0, 0, 0), 1);
    }

    // ── render_status_line ──────────────────────────────────────────

    #[test]
    fn status_line_flattens_embedded_newlines() {
        let t = theme();
        let s = StatusLine {
            icon: None,
            title: "line1\r\nline2\rline3\nline4",
            title_style: t.accent(),
            description: None,
            badge: None,
            meta: &[],
        };
        let line = render_status_line(&s, &t);
        let text: String = line.spans.iter().map(|sp| sp.content.as_ref()).collect();
        assert_eq!(text, "line1 line2 line3 line4");
    }

    #[test]
    fn status_line_without_icon_has_no_leading_space() {
        let t = theme();
        let s = StatusLine {
            icon: None,
            title: "Title",
            title_style: t.accent(),
            description: None,
            badge: None,
            meta: &[],
        };
        let line = render_status_line(&s, &t);
        let text: String = line.spans.iter().map(|sp| sp.content.as_ref()).collect();
        assert_eq!(text, "Title");
    }

    #[test]
    fn status_line_with_icon_has_exactly_one_separating_space() {
        let t = theme();
        let icon = Span::styled("✔", Style::default().add_modifier(Modifier::BOLD));
        let s = StatusLine {
            icon: Some(icon),
            title: "Title",
            title_style: t.accent(),
            description: None,
            badge: None,
            meta: &[],
        };
        let line = render_status_line(&s, &t);
        let text: String = line.spans.iter().map(|sp| sp.content.as_ref()).collect();
        assert_eq!(text, "✔ Title");
    }

    #[test]
    fn status_line_filters_blank_meta_entries() {
        let t = theme();
        let s = StatusLine {
            icon: None,
            title: "Title",
            title_style: t.accent(),
            description: None,
            badge: None,
            meta: &["a", "  ", "", "b"],
        };
        let line = render_status_line(&s, &t);
        let text: String = line.spans.iter().map(|sp| sp.content.as_ref()).collect();
        // 空白项被过滤掉，不会出现 "· ·" 这样的双分隔符。
        assert_eq!(text, format!("Title a{}b", t.symbols.sep.dot));
    }

    #[test]
    fn status_line_all_blank_meta_omits_meta_section_entirely() {
        let t = theme();
        let s = StatusLine {
            icon: None,
            title: "Title",
            title_style: t.accent(),
            description: None,
            badge: None,
            meta: &["", "   "],
        };
        let line = render_status_line(&s, &t);
        let text: String = line.spans.iter().map(|sp| sp.content.as_ref()).collect();
        assert_eq!(text, "Title");
    }

    #[test]
    fn status_line_badge_uses_bracket_symbols() {
        let t = theme();
        let s = StatusLine {
            icon: None,
            title: "Title",
            title_style: t.accent(),
            description: Some("desc"),
            badge: Some(("NEW", t.colors.success.into())),
            meta: &["m1", "m2"],
        };
        let line = render_status_line(&s, &t);
        let text: String = line.spans.iter().map(|sp| sp.content.as_ref()).collect();
        assert_eq!(
            text,
            format!(
                "Title: desc {}NEW{} m1{}m2",
                t.symbols.format.bracket_left, t.symbols.format.bracket_right, t.symbols.sep.dot
            )
        );
    }
}
