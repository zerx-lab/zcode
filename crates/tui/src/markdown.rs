//! `pulldown-cmark` 事件流 → `Line`：标题/强调/行内代码/代码块/列表/引用/水平线/链接/表格。
//!
//! 渲染形态逐条对标 oh-my-pi `packages/tui/src/components/markdown.ts`，具体行号
//! 标注在各渲染函数上。**非目标**（本次不做）：LaTeX、mermaid、图片协议、OSC 8 超链接、
//! OSC 66 双倍字号、流式增量缓存——ratatui 的 `Buffer` 没有对应属性，硬塞会破坏宽度计算。
//!
//! # 两遍架构：先解析成内部 AST，再渲染
//!
//! `pulldown-cmark` 产出的是一条扁平的 `Start(Tag)/End(TagEnd)` 事件流，紧凑列表（tight
//! list，条目间无空行）里的段落还会省略 `Start(Paragraph)`／`End(Paragraph)` 包裹，直接把
//! 内联事件摆在块级位置。渲染（换行、悬挂缩进、块间距）需要在真正落笔之前就知道「这是不是
//! 最后一个块」「这个列表项的 bullet 有多宽」，边解析边渲染做不到必要的前瞻。因此先把事件流
//! 收敛成一棵小 [`Block`]/[`Inline`] 树，再单独一遍把树排版成 `Vec<Line>`。
//!
//! # 宽度与换行
//!
//! 一律经 `zcode_text::width` 求宽度、经 [`crate::wrap::wrap_line`] 硬换行，绝不用
//! `str::len()` 或另写一份切分逻辑（见 `rule://zcode-architecture`「TUI 输出清理」）。
//! 每一层嵌套（引用竖条、列表悬挂缩进）都通过缩小往下传的可用列数体现，最终物理行数与
//! 视觉宽度天然一致，不需要事后校正。

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use std::iter::Peekable;
use zcode_text::width::visible_width;

use crate::highlight::Highlighter;
use crate::theme::Theme;
use crate::theme::symbols::BoxSymbols;
use crate::wrap::{line_rows, line_width, wrap_line};

/// markdown 渲染的可配置项。
#[derive(Debug, Clone, Copy)]
pub struct MarkdownOptions {
    /// 代码块正文的额外左缩进列数。omp 默认 2（`markdown.ts:2276-2311`），围栏本身
    /// 不缩进，只缩正文，让代码块在视觉上比围栏窄一格、更容易一眼分辨边界。
    /// 助手消息渲染时显式传 0：代码块与外层气泡对齐，不需要额外缩进。
    pub code_block_indent: u16,
    /// 正文默认样式，沿整棵渲染树向下传递并逐层 `patch`。用户气泡会传带背景色的
    /// `Style`，让代码块/列表/引用等全部嵌套内容延续同一块背景色；不需要背景时传
    /// `Style::default()`。
    pub base: Style,
    /// 是否对代码块正文做语法高亮；`false` 时代码块整体走 `md_code_block` 单色
    /// （即便有 `lang` 也不高亮，用于流式渲染中间态等不值得为语法高亮花时间的场景）。
    pub highlight: bool,
}

/// 把一段 markdown 源码渲染成一组 `Line`。
///
/// `width == 0`（或极窄终端）时所有内容宽度都退化到 `max(1, …)`，不产生 panic，但也不
/// 保证可读——调用方应避免真的把 0 传进来。
#[must_use]
pub fn render_markdown(
    src: &str,
    width: u16,
    theme: &Theme,
    opts: &MarkdownOptions,
) -> Vec<Line<'static>> {
    let width = usize::from(width).max(1);
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let mut iter: EventIter<'_> = Parser::new_ext(src, options).peekable();
    let blocks = parse_blocks(&mut iter, None);
    let ctx = RenderCtx {
        theme,
        opts: *opts,
        width,
        depth: 0,
    };
    render_blocks(&blocks, ctx)
}

type EventIter<'a> = Peekable<Parser<'a>>;

// ============================================================================
// 内部 AST
// ============================================================================

/// 内联元素：粗/斜/删除线可嵌套，代码不含子节点。`Image` 不建独立节点——按规范只取
/// alt 文本，解析时直接把它的内联子节点拼进外层内联流。
#[derive(Debug, Clone)]
enum Inline {
    /// 纯文本。
    Text(String),
    /// 行内代码。
    Code(String),
    /// 强调（`*text*`）。
    Emphasis(Vec<Inline>),
    /// 加粗（`**text**`）。
    Strong(Vec<Inline>),
    /// 删除线（`~~text~~`）。
    Strikethrough(Vec<Inline>),
    /// 链接：文字与目标 URL。
    Link { text: Vec<Inline>, url: String },
    /// 软换行（源码内的普通换行，渲染时按空格处理，交给后续 wrap 重新排版）。
    SoftBreak,
    /// 硬换行（两个空格或反斜杠结尾），渲染时强制断行，不并入 wrap。
    HardBreak,
}

/// 块级元素。
#[derive(Debug, Clone)]
enum Block {
    /// 段落。
    Paragraph(Vec<Inline>),
    /// 标题。
    Heading {
        level: HeadingLevel,
        inlines: Vec<Inline>,
    },
    /// 代码块。`lang` 为围栏语言标记原文（可能为空串或 `None`，`None` 对应缩进代码块）。
    Code { lang: Option<String>, code: String },
    /// 引用块，可嵌套。
    Quote(Vec<Block>),
    /// 列表。`start` 为空表示无序，否则为有序列表起始序号。
    List {
        start: Option<u64>,
        items: Vec<ListItem>,
    },
    /// 表格：表头一行、正文若干行，每行若干单元格。忽略源码里的列对齐标记
    /// （与 omp 一致，`markdown.ts:2820-3020` 附近的实现同样不读 alignment）。
    Table {
        head: Vec<Vec<Inline>>,
        rows: Vec<Vec<Vec<Inline>>>,
    },
    /// 水平线。
    Rule,
}

/// 列表项：`checked` 为 `Some` 表示任务列表项（`Some(true)` 已勾选）。
#[derive(Debug, Clone)]
struct ListItem {
    checked: Option<bool>,
    blocks: Vec<Block>,
}

// ============================================================================
// 解析：事件流 → AST
// ============================================================================

/// `tag` 是否是「真正的块级」标签——出现在块位置时应当结束当前隐式段落、另起一个
/// [`Block`]。紧凑列表项里裸露的内联事件（Emphasis/Strong/Link/…/Text）不在此列，
/// 它们由 [`push_inline_event`] 并入隐式段落。
fn is_block_tag(tag: &Tag<'_>) -> bool {
    matches!(
        tag,
        Tag::Paragraph
            | Tag::Heading { .. }
            | Tag::CodeBlock(_)
            | Tag::BlockQuote(_)
            | Tag::List(_)
            | Tag::Table(_)
            | Tag::HtmlBlock
            | Tag::FootnoteDefinition(_)
            | Tag::DefinitionList
            | Tag::DefinitionListTitle
            | Tag::DefinitionListDefinition
            | Tag::MetadataBlock(_)
    )
}

/// 解析一段块级内容，直到遇到 `stop`（`None` 表示解析到事件流耗尽为止，用于顶层文档）。
/// 紧凑列表项里没有 `Start(Paragraph)` 包裹的裸内联事件会被收进 `pending`，遇到下一个
/// 真正的块级标签、`Rule`，或 `stop` 时作为一个隐式 [`Block::Paragraph`] 落盘。
fn parse_blocks(iter: &mut EventIter<'_>, stop: Option<TagEnd>) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut pending: Vec<Inline> = Vec::new();
    while let Some(event) = iter.next() {
        match event {
            Event::End(end) if Some(&end) == stop.as_ref() => {
                flush_paragraph(&mut pending, &mut blocks);
                return blocks;
            }
            Event::Start(tag) if is_block_tag(&tag) => {
                flush_paragraph(&mut pending, &mut blocks);
                let end = tag.to_end();
                match tag {
                    Tag::Paragraph => {
                        blocks.push(Block::Paragraph(parse_inline_children(iter, end)));
                    }
                    Tag::Heading { level, .. } => blocks.push(Block::Heading {
                        level,
                        inlines: parse_inline_children(iter, end),
                    }),
                    Tag::CodeBlock(kind) => blocks.push(parse_code_block(iter, &kind, end)),
                    Tag::BlockQuote(_) => {
                        blocks.push(Block::Quote(parse_blocks(iter, Some(end))));
                    }
                    Tag::List(start) => blocks.push(Block::List {
                        start,
                        items: parse_list_items(iter, end),
                    }),
                    Tag::Table(_) => blocks.push(parse_table(iter, end)),
                    _ => skip_until(iter, end),
                }
            }
            Event::Rule => {
                flush_paragraph(&mut pending, &mut blocks);
                blocks.push(Block::Rule);
            }
            Event::TaskListMarker(_) => {
                // 只应出现在 `parse_list_items` 消费的位置；这里出现是防御性兜底，忽略。
            }
            other => push_inline_event(other, iter, &mut pending),
        }
    }
    flush_paragraph(&mut pending, &mut blocks);
    blocks
}

fn flush_paragraph(pending: &mut Vec<Inline>, blocks: &mut Vec<Block>) {
    if !pending.is_empty() {
        blocks.push(Block::Paragraph(std::mem::take(pending)));
    }
}

/// 消费纯内联内容直到 `stop`，用于段落/标题正文，以及链接文字等内联子树。
fn parse_inline_children(iter: &mut EventIter<'_>, stop: TagEnd) -> Vec<Inline> {
    let mut out = Vec::new();
    // 循环体内 `push_inline_event` 还要继续从 `iter` 取事件（内联标签是递归下降解析的），
    // 所以不能改成 `for … in iter.by_ref()`——那会把 `iter` 整个借走。
    while let Some(event) = iter.next() {
        if let Event::End(end) = &event
            && *end == stop
        {
            break;
        }
        push_inline_event(event, iter, &mut out);
    }
    out
}

fn push_inline_event(event: Event<'_>, iter: &mut EventIter<'_>, out: &mut Vec<Inline>) {
    match event {
        Event::Text(s) => out.push(Inline::Text(s.into_string())),
        Event::Code(s) => out.push(Inline::Code(s.into_string())),
        Event::SoftBreak => out.push(Inline::SoftBreak),
        Event::HardBreak => out.push(Inline::HardBreak),
        Event::Start(tag) => {
            let end = tag.to_end();
            match tag {
                Tag::Emphasis => out.push(Inline::Emphasis(parse_inline_children(iter, end))),
                Tag::Strong => out.push(Inline::Strong(parse_inline_children(iter, end))),
                Tag::Strikethrough => {
                    out.push(Inline::Strikethrough(parse_inline_children(iter, end)));
                }
                Tag::Link { dest_url, .. } => out.push(Inline::Link {
                    text: parse_inline_children(iter, end),
                    url: dest_url.into_string(),
                }),
                // 图片：只要 alt 文本，不建立 Image 包装节点，直接拼进当前内联流。
                Tag::Image { .. } | Tag::Superscript | Tag::Subscript => {
                    out.extend(parse_inline_children(iter, end));
                }
                // 出现在内联位置的块级标签（不应发生，防御性兜底）：整体跳过。
                _ => skip_until(iter, end),
            }
        }
        // 未匹配到自己 stop 的杂散 End（不应发生，pulldown-cmark 的事件流总是良好嵌套的）、
        // 非目标事件（脚注引用、原始 HTML、LaTeX 数学），以及只在块级有意义的 Rule /
        // TaskListMarker：都不产出任何可见字符，静默忽略。
        Event::End(_)
        | Event::FootnoteReference(_)
        | Event::Html(_)
        | Event::InlineHtml(_)
        | Event::InlineMath(_)
        | Event::DisplayMath(_)
        | Event::Rule
        | Event::TaskListMarker(_) => {}
    }
}

/// 跳过一个块级/内联标签的整个子树，容忍其中出现同类型嵌套（靠 `to_end()` 相等做深度计数，
/// 而不是只认最近一个 `End`）。用于本次不支持的标签（HTML 块、脚注定义、定义列表等）。
fn skip_until(iter: &mut EventIter<'_>, end: TagEnd) {
    let mut depth = 0usize;
    for event in iter.by_ref() {
        match event {
            Event::Start(tag) if tag.to_end() == end => depth = depth.saturating_add(1),
            Event::End(e) if e == end => {
                if depth == 0 {
                    return;
                }
                depth = depth.saturating_sub(1);
            }
            _ => {}
        }
    }
}

fn parse_code_block(iter: &mut EventIter<'_>, kind: &CodeBlockKind<'_>, end: TagEnd) -> Block {
    let lang = match kind {
        CodeBlockKind::Fenced(info) if !info.is_empty() => Some(info.to_string()),
        _ => None,
    };
    let mut code = String::new();
    for event in iter.by_ref() {
        match event {
            Event::End(e) if e == end => break,
            Event::Text(s) => code.push_str(&s),
            _ => {}
        }
    }
    Block::Code { lang, code }
}

fn parse_list_items(iter: &mut EventIter<'_>, list_end: TagEnd) -> Vec<ListItem> {
    let mut items = Vec::new();
    while let Some(event) = iter.next() {
        match event {
            Event::End(e) if e == list_end => break,
            Event::Start(Tag::Item) => {
                let checked = match iter.peek() {
                    Some(Event::TaskListMarker(_)) => match iter.next() {
                        Some(Event::TaskListMarker(v)) => Some(v),
                        _ => None,
                    },
                    _ => None,
                };
                let blocks = parse_blocks(iter, Some(TagEnd::Item));
                items.push(ListItem { checked, blocks });
            }
            _ => {}
        }
    }
    items
}

fn parse_table(iter: &mut EventIter<'_>, table_end: TagEnd) -> Block {
    let mut head = Vec::new();
    let mut rows = Vec::new();
    while let Some(event) = iter.next() {
        match event {
            Event::End(e) if e == table_end => break,
            Event::Start(Tag::TableHead) => head = parse_table_cells(iter, TagEnd::TableHead),
            Event::Start(Tag::TableRow) => rows.push(parse_table_cells(iter, TagEnd::TableRow)),
            _ => {}
        }
    }
    Block::Table { head, rows }
}

fn parse_table_cells(iter: &mut EventIter<'_>, row_end: TagEnd) -> Vec<Vec<Inline>> {
    let mut cells = Vec::new();
    while let Some(event) = iter.next() {
        match event {
            Event::End(e) if e == row_end => break,
            Event::Start(Tag::TableCell) => {
                cells.push(parse_inline_children(iter, TagEnd::TableCell));
            }
            _ => {}
        }
    }
    cells
}

// ============================================================================
// 渲染上下文
// ============================================================================

/// 渲染上下文：贯穿整棵块/内联树的只读参数，`Copy` 以便按值传递、不必操心生命周期。
#[derive(Clone, Copy)]
struct RenderCtx<'a> {
    theme: &'a Theme,
    opts: MarkdownOptions,
    /// 当前嵌套层级可用的显示列数，已经扣掉全部祖先前缀（引用竖条、列表悬挂缩进）。
    width: usize,
    /// 列表嵌套深度，只被 [`render_list`] 读取，用来决定「嵌套每级缩 2 空格」的本级
    /// 贡献：每层固定 +2，靠递归物理前缀自然叠加成 depth 层总量 `depth*2`，不在这里
    /// 直接乘 depth（那样会在物理嵌套之外重复计入祖先层级已经贡献过的缩进）。
    depth: u16,
}

// ============================================================================
// 渲染：块级
// ============================================================================

/// 渲染一组同级块，并按 omp 的块间距规则（`markdown.ts:2251,2270,2307,2313-2317`）在
/// 相邻块之间插入空行：存在下一个块就插，除了「段落后紧跟列表」这个例外不插。
/// 列表自身从不在这里被特殊对待——它「从不加尾随空行」是 [`render_list`] 的职责
/// （从不在条目之间插空行），跟本函数处理的「块与块之间」是两层不同的事，不会重复插入。
fn render_blocks(blocks: &[Block], ctx: RenderCtx<'_>) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for (i, block) in blocks.iter().enumerate() {
        lines.extend(render_block(block, ctx));
        if let Some(next) = blocks.get(i.saturating_add(1))
            && !matches!((block, next), (Block::Paragraph(_), Block::List { .. }))
        {
            lines.push(Line::default());
        }
    }
    lines
}

fn render_block(block: &Block, ctx: RenderCtx<'_>) -> Vec<Line<'static>> {
    match block {
        Block::Paragraph(inlines) => {
            render_inline_wrapped(inlines, ctx.width, ctx.opts.base, ctx.theme)
        }
        Block::Heading { level, inlines } => render_heading(*level, inlines, ctx),
        Block::Code { lang, code } => render_code_block(lang.as_deref(), code, ctx),
        Block::Quote(children) => render_blockquote(children, ctx),
        Block::List { start, items } => render_list(*start, items, ctx),
        Block::Table { head, rows } => render_table(head, rows, ctx),
        Block::Rule => vec![render_rule(ctx)],
    }
}

/// 标题（`markdown.ts:2225-2255`）：H1 = `md_heading` + BOLD + UNDERLINED，无 `#` 前缀；
/// H2 = `md_heading` + BOLD，无前缀；H3 及以下保留字面 `###` 前缀 + 一个空格，整行
/// `md_heading` + BOLD（内联的粗/斜/链接/代码等语义色仍会在这个基色之上叠加/覆盖，
/// 因为渲染走的是同一套 `base.patch(...)` 级联，不是整行强制单色）。
fn render_heading(
    level: HeadingLevel,
    inlines: &[Inline],
    ctx: RenderCtx<'_>,
) -> Vec<Line<'static>> {
    let modifier = match level {
        HeadingLevel::H1 => Modifier::BOLD | Modifier::UNDERLINED,
        _ => Modifier::BOLD,
    };
    let heading_style = ctx.opts.base.patch(
        Style::new()
            .fg(ctx.theme.colors.md_heading)
            .add_modifier(modifier),
    );
    let prefix = match level {
        HeadingLevel::H1 | HeadingLevel::H2 => String::new(),
        HeadingLevel::H3 => "### ".to_owned(),
        HeadingLevel::H4 => "#### ".to_owned(),
        HeadingLevel::H5 => "##### ".to_owned(),
        HeadingLevel::H6 => "###### ".to_owned(),
    };
    let mut lines = Vec::new();
    for (i, run) in split_hard_breaks(inlines).into_iter().enumerate() {
        let mut spans = Vec::new();
        if i == 0 && !prefix.is_empty() {
            spans.push(Span::styled(prefix.clone(), heading_style));
        }
        spans.extend(render_inline(run, heading_style, ctx.theme));
        lines.extend(wrap_line(&Line::from(spans), ctx.width.max(1)));
    }
    if lines.is_empty() {
        lines.push(Line::default());
    }
    lines
}

/// 代码块（`markdown.ts:2276-2311`）：首行 \`\`\`lang（语言紧贴三反引号）、尾行 \`\`\`，
/// 两行都用 `md_code_block_border` 色，围栏本身不缩进。正文左缩
/// `opts.code_block_indent` 个空格；有语言且 `opts.highlight` 时走 [`Highlighter`]
/// （fallback 样式 = `md_code_block` 色），否则逐行单色 `md_code_block`。无行号。
fn render_code_block(lang: Option<&str>, code: &str, ctx: RenderCtx<'_>) -> Vec<Line<'static>> {
    let border_style = ctx
        .opts
        .base
        .patch(Style::new().fg(ctx.theme.colors.md_code_block_border));
    let mut lines = vec![Line::from(Span::styled(
        format!("```{}", lang.unwrap_or("")),
        border_style,
    ))];

    let indent = usize::from(ctx.opts.code_block_indent);
    let content_width = ctx.width.saturating_sub(indent).max(1);
    let indent_prefix = [Span::styled(" ".repeat(indent), ctx.opts.base)];
    let code_trimmed = code.strip_suffix('\n').unwrap_or(code);

    let body_lines: Vec<Line<'static>> = if ctx.opts.highlight && lang.is_some() {
        let fallback = ctx
            .opts
            .base
            .patch(Style::new().fg(ctx.theme.colors.md_code_block));
        Highlighter::shared()
            .highlight(code_trimmed, lang, ctx.theme, fallback)
            .into_iter()
            .map(|line| patch_line_base(line, ctx.opts.base))
            .collect()
    } else {
        let style = ctx
            .opts
            .base
            .patch(Style::new().fg(ctx.theme.colors.md_code_block));
        if code_trimmed.is_empty() {
            vec![Line::default()]
        } else {
            code_trimmed
                .split('\n')
                .map(|line| Line::from(Span::styled(line.to_owned(), style)))
                .collect()
        }
    };

    for line in &body_lines {
        for wrapped in wrap_line(line, content_width) {
            lines.push(prefix_line(wrapped, &indent_prefix));
        }
    }

    lines.push(Line::from(Span::styled("```".to_owned(), border_style)));
    lines
}

/// 用 `base` 补上高亮结果里缺失的背景色：[`Highlighter::highlight`] 只设置前景（语法色），
/// 背景（用户气泡色）要在这里用 `base.patch(span.style)` 补回——`patch` 保留 `self`（`base`）
/// 的 `bg`，因为高亮出的 `span.style` 从不设置 `bg`，`other.bg` 恒为 `None`。
fn patch_line_base(line: Line<'static>, base: Style) -> Line<'static> {
    Line {
        spans: line
            .spans
            .into_iter()
            .map(|s| Span {
                content: s.content,
                style: base.patch(s.style),
            })
            .collect(),
        style: line.style,
        alignment: line.alignment,
    }
}

/// 引用块（`markdown.ts:2327-2392`）：前缀 = `md.quote_border` + 一个空格，`md_quote_border`
/// 色；内容宽 = `max(1, width - 2)`，内容样式 = `md_quote` 色 + `Modifier::ITALIC`，
/// 作为新的 `base` 向下级联（因此引用里的代码块/标题等仍按各自语义色叠加，不会被
/// 斜体/引用色整体吞掉）。嵌套引用天然支持：内层再套一层 `render_blockquote` 时，
/// 外层已经把 `width` 缩到位，内层的前缀继续往里叠一层缩进。
fn render_blockquote(children: &[Block], ctx: RenderCtx<'_>) -> Vec<Line<'static>> {
    let border = ctx.theme.symbols.md.quote_border;
    let prefix_style = Style::new().fg(ctx.theme.colors.md_quote_border);
    let prefix = [Span::styled(format!("{border} "), prefix_style)];
    let content_width = ctx.width.saturating_sub(2).max(1);
    let content_base = ctx.opts.base.patch(
        Style::new()
            .fg(ctx.theme.colors.md_quote)
            .add_modifier(Modifier::ITALIC),
    );
    let inner_ctx = RenderCtx {
        opts: MarkdownOptions {
            base: content_base,
            ..ctx.opts
        },
        width: content_width,
        ..ctx
    };
    let mut inner_lines = render_blocks(children, inner_ctx);
    if inner_lines.is_empty() {
        inner_lines.push(Line::default());
    }
    indent_lines(inner_lines, &prefix, &prefix)
}

/// 列表（`markdown.ts:2641-2716`）：无序 bullet 从主题读 `theme.symbols.md.bullet`
/// 后跟一空格——注意 omp 自己在这里硬编码了字面 `"- "`，与它自己主题里的 `md.bullet`
/// 不一致，是上游的缺陷；本仓统一走主题取值，不复现这个不一致。有序 = `{start + i}. `；
/// bullet/序号整体用 `md_list_bullet` 色；嵌套每级缩 2 空格（见 [`RenderCtx::depth`]
/// 文档）；续行悬挂缩进 = bullet 的实际显示宽度（`"10. "` 是 4 列，不是固定 2）。
/// 任务列表项的 bullet 换成 `theme.symbols.choice.{unchecked,checked}`。
/// 列表从不自己在条目之间插入空行——这是与 [`render_blocks`] 块间距规则的分工。
fn render_list(start: Option<u64>, items: &[ListItem], ctx: RenderCtx<'_>) -> Vec<Line<'static>> {
    // 嵌套缩进**不在这里再加**：子列表的行已经被父项的 `rest_prefix`（宽度 =
    // 父 bullet 的显示宽）推过一次，正好把子项对齐到父项正文的起始列。
    //
    // 这一处刻意不照抄 omp。它嵌套按固定 `"  ".repeat(depth)`（`markdown.ts:2643`）
    // 缩，而续行悬挂按 bullet 实际宽度（`markdown.ts:2685`）——两者在 `"10. "`
    // 这种 4 列 bullet 下对不齐：子项落在父项正文左边两列。对齐到父正文起点既
    // 消掉这个不一致，在 bullet 宽为 2 的常见情形下与 omp 的结果又完全相同。
    let local_indent: usize = 0;
    let bullet_style = Style::new().fg(ctx.theme.colors.md_list_bullet);
    let mut lines = Vec::new();
    for (i, item) in items.iter().enumerate() {
        let bullet = list_bullet(start, i, item.checked, ctx.theme);
        let bullet_width = visible_width(&bullet);
        let content_width = ctx
            .width
            .saturating_sub(local_indent)
            .saturating_sub(bullet_width)
            .max(1);
        let item_ctx = RenderCtx {
            width: content_width,
            depth: ctx.depth.saturating_add(1),
            ..ctx
        };
        let mut item_lines = render_blocks(&item.blocks, item_ctx);
        if item_lines.is_empty() {
            item_lines.push(Line::default());
        }
        let indent_span = Span::styled(" ".repeat(local_indent), ctx.opts.base);
        let first_prefix = [
            indent_span,
            Span::styled(bullet, ctx.opts.base.patch(bullet_style)),
        ];
        let rest_prefix = [Span::styled(
            " ".repeat(local_indent.saturating_add(bullet_width)),
            ctx.opts.base,
        )];
        lines.extend(indent_lines(item_lines, &first_prefix, &rest_prefix));
    }
    lines
}

fn list_bullet(start: Option<u64>, index: usize, checked: Option<bool>, theme: &Theme) -> String {
    if let Some(is_checked) = checked {
        let glyph = if is_checked {
            theme.symbols.choice.checked
        } else {
            theme.symbols.choice.unchecked
        };
        return format!("{glyph} ");
    }
    match start {
        Some(first) => {
            let n = first.saturating_add(u64::try_from(index).unwrap_or(u64::MAX));
            format!("{n}. ")
        }
        None => format!("{} ", theme.symbols.md.bullet),
    }
}

/// 水平线（`markdown.ts:2394-2427`）：`theme.symbols.md.hr_char` 重复铺满整个 `width`，
/// 色 `md_hr`。**与 omp 不同：这里用完整 `width`，不截到 `min(width, 80)`**——那个 80
/// 在 omp 仓里无注释无依据，宽终端下看起来更像遗留 bug 而非有意设计，故这里刻意不移植。
fn render_rule(ctx: RenderCtx<'_>) -> Line<'static> {
    let hr = ctx.theme.symbols.md.hr_char;
    let unit = visible_width(hr).max(1);
    let count = ctx.width / unit;
    Line::from(Span::styled(
        hr.repeat(count),
        Style::new().fg(ctx.theme.colors.md_hr),
    ))
}

// ============================================================================
// 渲染：表格
// ============================================================================

/// 表格里单个「不可断词」被计入列最小宽度的上限（列）。
///
/// 出处 oh-my-pi `packages/tui/src/components/markdown.ts:2853` 的 `maxUnbrokenWordWidth`。
/// 前提：一个 40 列的 URL 或哈希不该把整张表挤到只剩它一列——超过这个宽度的词宁可
/// 被切断，也要给其余列留出可读宽度。这个上限直接决定窄表格走「按比例分配」还是
/// 「全退到 1 再重分」那条分支。
const MAX_UNBROKEN_WORD_WIDTH: usize = 30;

/// 表格（`packages/tui/src/components/markdown.ts:2820-3020`）。用
/// `theme.symbols.box_sharp` 画框，布局 `│ cell │ cell │`，边框开销 = `3n+1`
/// （`n+1` 根竖线 + 每个单元格左右各一个空格）。每两行之间都画一条分隔线，不只是
/// 表头下面那一条。表头单元格整体 BOLD（含右侧补白）。
///
/// 列宽算法见 [`compute_column_widths`]。可用格宽 < 列数时整表放弃，回退到
/// [`render_table_fallback`]。单元格左对齐 + 右补空格，忽略 markdown 的列对齐标记
/// （与 omp 一致）。
fn render_table(
    head: &[Vec<Inline>],
    rows: &[Vec<Vec<Inline>>],
    ctx: RenderCtx<'_>,
) -> Vec<Line<'static>> {
    let n = head.len().max(rows.iter().map(Vec::len).max().unwrap_or(0));
    if n == 0 {
        return Vec::new();
    }
    let overhead = 3usize.saturating_mul(n).saturating_add(1);
    let budget = ctx.width.saturating_sub(overhead);
    if budget < n {
        return render_table_fallback(head, rows, ctx);
    }

    let mut natural = vec![0usize; n];
    let mut min_w = vec![1usize; n];
    for row in std::iter::once(head).chain(rows.iter().map(Vec::as_slice)) {
        for (col, cell) in row.iter().enumerate() {
            let line = Line::from(render_inline(cell, ctx.opts.base, ctx.theme));
            let natural_width = line_width(&line);
            // 最长不可断词决定该列的**最小宽度**：比它窄就一定会把词切断。
            // 上限的理由见 `MAX_UNBROKEN_WORD_WIDTH`。
            let longest_word = inline_plain_text(cell)
                .split_whitespace()
                .map(visible_width)
                .max()
                .unwrap_or(0)
                .clamp(1, MAX_UNBROKEN_WORD_WIDTH);
            if let Some(slot) = natural.get_mut(col) {
                *slot = (*slot).max(natural_width);
            }
            if let Some(slot) = min_w.get_mut(col) {
                *slot = (*slot).max(longest_word);
            }
        }
    }

    let widths = compute_column_widths(&natural, &min_w, budget, n);
    let border_style = Style::new().fg(ctx.theme.colors.border);
    let sym = ctx.theme.symbols.box_sharp;

    let mut lines = vec![border_row(
        &widths,
        sym.top_left,
        sym.horizontal,
        sym.tee_down,
        sym.top_right,
        border_style,
    )];
    lines.extend(table_row(head, &widths, ctx, true, sym, border_style));
    lines.push(border_row(
        &widths,
        sym.tee_right,
        sym.horizontal,
        sym.cross,
        sym.tee_left,
        border_style,
    ));
    for (i, row) in rows.iter().enumerate() {
        lines.extend(table_row(row, &widths, ctx, false, sym, border_style));
        if i.saturating_add(1) < rows.len() {
            lines.push(border_row(
                &widths,
                sym.tee_right,
                sym.horizontal,
                sym.cross,
                sym.tee_left,
                border_style,
            ));
        }
    }
    lines.push(border_row(
        &widths,
        sym.bottom_left,
        sym.horizontal,
        sym.tee_up,
        sym.bottom_right,
        border_style,
    ));
    lines
}

/// 列宽分配。三段式，均以 `budget = width - overhead`（表格可用于单元格内容的总列数）
/// 为基准：
///
/// 1. `Σmin > budget`：连最小宽度都放不下，全部退到 1 列，再按各列
///    `(min_i - 1)` 为权重比例分配剩余预算，四舍五入产生的余数从左到右各 +1
///    （权重和为 0 时退化为纯 round-robin，等价于均分）。
/// 2. `Σnatural ≤ budget`：自然宽度已经够放，直接用自然宽度，不强行摊满剩余预算
///    （表格可以比可用宽度窄，比强行拉伸更符合直觉）。
/// 3. 否则：每列先给 `min_i`，剩余预算按 `(natural_i - min_i)` 比例分配增量，
///    余数同样从左到右各 +1（这一步的余数分配规则原文只在分支 1 里明确提到，这里按
///    同样的公平性考虑类推过来，避免因为取整损失几列可用预算）。
fn compute_column_widths(
    natural: &[usize],
    min_w: &[usize],
    budget: usize,
    n: usize,
) -> Vec<usize> {
    let sum_min: usize = min_w.iter().sum();
    let sum_natural: usize = natural.iter().sum();

    if sum_min > budget {
        let mut widths = vec![1usize; n];
        let remaining = budget.saturating_sub(n);
        let weights: Vec<usize> = min_w.iter().map(|m| m.saturating_sub(1)).collect();
        apply_weighted_extra(&mut widths, remaining, &weights);
        widths
    } else if sum_natural <= budget {
        natural.to_vec()
    } else {
        let mut widths = min_w.to_vec();
        let extra_budget = budget.saturating_sub(sum_min);
        let diff: Vec<usize> = natural
            .iter()
            .zip(min_w.iter())
            .map(|(nat, min)| nat.saturating_sub(*min))
            .collect();
        apply_weighted_extra(&mut widths, extra_budget, &diff);
        widths
    }
}

/// 把 `extra` 按 `weights` 比例分配加到 `widths` 上；`weights` 全零时退化为纯左到右
/// round-robin（等价于均分）。整数除法产生的余数统一从左到右各 +1。
fn apply_weighted_extra(widths: &mut [usize], extra: usize, weights: &[usize]) {
    let sum_w: usize = weights.iter().sum();
    if sum_w == 0 {
        distribute_leftover(widths, extra);
        return;
    }
    let mut assigned = 0usize;
    for (w, weight) in widths.iter_mut().zip(weights.iter()) {
        let share = extra.saturating_mul(*weight) / sum_w;
        *w = w.saturating_add(share);
        assigned = assigned.saturating_add(share);
    }
    distribute_leftover(widths, extra.saturating_sub(assigned));
}

fn distribute_leftover(widths: &mut [usize], leftover: usize) {
    let n = widths.len();
    if n == 0 {
        return;
    }
    for k in 0..leftover {
        if let Some(slot) = widths.get_mut(k % n) {
            *slot = slot.saturating_add(1);
        }
    }
}

fn border_row(
    widths: &[usize],
    left: &str,
    fill: &str,
    mid: &str,
    right: &str,
    style: Style,
) -> Line<'static> {
    let unit = visible_width(fill).max(1);
    let mut s = String::from(left);
    let last = widths.len().saturating_sub(1);
    for (i, w) in widths.iter().enumerate() {
        let count = w.saturating_add(2) / unit;
        for _ in 0..count {
            s.push_str(fill);
        }
        s.push_str(if i == last { right } else { mid });
    }
    Line::from(Span::styled(s, style))
}

/// 渲染表格的一整行（表头或正文行），可能跨多条物理行——某个单元格换行更多时，
/// 同一行的其它单元格用空行补齐到相同高度。
fn table_row(
    cells: &[Vec<Inline>],
    widths: &[usize],
    ctx: RenderCtx<'_>,
    is_header: bool,
    sym: BoxSymbols,
    border_style: Style,
) -> Vec<Line<'static>> {
    let cell_style = if is_header {
        ctx.opts
            .base
            .patch(Style::new().add_modifier(Modifier::BOLD))
    } else {
        ctx.opts.base
    };

    let rendered: Vec<Line<'static>> = widths
        .iter()
        .enumerate()
        .map(|(col, _)| {
            let content = cells.get(col).map_or(&[][..], Vec::as_slice);
            Line::from(render_inline(content, cell_style, ctx.theme))
        })
        .collect();

    let row_height = widths
        .iter()
        .zip(rendered.iter())
        .map(|(w, line)| line_rows(line, (*w).max(1)))
        .max()
        .unwrap_or(1);

    let cell_physical: Vec<Vec<Line<'static>>> = widths
        .iter()
        .zip(rendered.iter())
        .map(|(w, line)| {
            let mut wrapped = wrap_line(line, (*w).max(1));
            while wrapped.len() < row_height {
                wrapped.push(Line::default());
            }
            wrapped
        })
        .collect();

    (0..row_height)
        .map(|r| {
            let mut spans = vec![Span::styled(sym.vertical.to_owned(), border_style)];
            for (col, w) in widths.iter().enumerate() {
                spans.push(Span::styled(" ".to_owned(), cell_style));
                let cell_line = cell_physical.get(col).and_then(|rows| rows.get(r));
                let content_width = cell_line.map_or(0, line_width);
                if let Some(l) = cell_line {
                    spans.extend(l.spans.iter().cloned());
                }
                let pad = w.saturating_sub(content_width);
                spans.push(Span::styled(" ".repeat(pad.saturating_add(1)), cell_style));
                spans.push(Span::styled(sym.vertical.to_owned(), border_style));
            }
            Line::from(spans)
        })
        .collect()
}

/// 可用格宽 < 列数时整表放弃，直接把每行原始文本（各单元格用 `" | "` 拼接）交给
/// `wrap_line` 按 `width` 硬换行——极窄终端下画不出一张能看的表格，不如退回纯文本。
fn render_table_fallback(
    head: &[Vec<Inline>],
    rows: &[Vec<Vec<Inline>>],
    ctx: RenderCtx<'_>,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for row in std::iter::once(head).chain(rows.iter().map(Vec::as_slice)) {
        let text = row
            .iter()
            .map(|c| inline_plain_text(c))
            .collect::<Vec<_>>()
            .join(" | ");
        let line = Line::from(Span::styled(text, ctx.opts.base));
        lines.extend(wrap_line(&line, ctx.width.max(1)));
    }
    lines
}

// ============================================================================
// 渲染：内联
// ============================================================================

/// 段落一类「纯内联内容 → 换行成若干 `Line`」的通用路径：先按硬换行切成若干段
/// （[`split_hard_breaks`]），每段独立渲染再各自交给 [`wrap_line`]。
fn render_inline_wrapped(
    inlines: &[Inline],
    width: usize,
    base: Style,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let width = width.max(1);
    let mut lines = Vec::new();
    for run in split_hard_breaks(inlines) {
        let line = Line::from(render_inline(run, base, theme));
        lines.extend(wrap_line(&line, width));
    }
    if lines.is_empty() {
        lines.push(Line::default());
    }
    lines
}

/// 按顶层 [`Inline::HardBreak`] 切分成若干子切片；只看顶层，不递归进
/// `Emphasis`/`Strong`/`Strikethrough` 内部——聊天场景里格式化文本内嵌硬换行极其
/// 罕见，嵌套时退化为按空格处理（见 [`render_inline_one`]），不值得为此打断样式栈。
fn split_hard_breaks(inlines: &[Inline]) -> Vec<&[Inline]> {
    let mut runs = Vec::new();
    let mut start = 0usize;
    for (i, inline) in inlines.iter().enumerate() {
        if matches!(inline, Inline::HardBreak) {
            if let Some(slice) = inlines.get(start..i) {
                runs.push(slice);
            }
            start = i.saturating_add(1);
        }
    }
    if let Some(slice) = inlines.get(start..) {
        runs.push(slice);
    }
    runs
}

fn render_inline(inlines: &[Inline], base: Style, theme: &Theme) -> Vec<Span<'static>> {
    let mut out = Vec::new();
    for inline in inlines {
        render_inline_one(inline, base, theme, &mut out);
    }
    out
}

fn render_inline_one(inline: &Inline, base: Style, theme: &Theme, out: &mut Vec<Span<'static>>) {
    match inline {
        Inline::Text(s) => push_text(out, s, base),
        // 行内代码（`markdown.ts:2566-2570`）：只换前景色 `md_code`，无背景、不保留
        // 反引号、前后不加空格。
        Inline::Code(s) => push_text(out, s, base.patch(Style::new().fg(theme.colors.md_code))),
        Inline::Emphasis(children) => {
            out.extend(render_inline(
                children,
                base.patch(Style::new().add_modifier(Modifier::ITALIC)),
                theme,
            ));
        }
        Inline::Strong(children) => {
            out.extend(render_inline(
                children,
                base.patch(Style::new().add_modifier(Modifier::BOLD)),
                theme,
            ));
        }
        Inline::Strikethrough(children) => {
            out.extend(render_inline(
                children,
                base.patch(Style::new().add_modifier(Modifier::CROSSED_OUT)),
                theme,
            ));
        }
        Inline::Link { text, url } => render_link(text, url, base, theme, out),
        // 顶层硬换行已经被 `split_hard_breaks` 消费掉；这里只会遇到嵌套在强调/加粗
        // 内部的硬换行，退化为空格。
        Inline::SoftBreak | Inline::HardBreak => push_text(out, " ", base),
    }
}

fn push_text(out: &mut Vec<Span<'static>>, text: &str, style: Style) {
    if !text.is_empty() {
        out.push(Span::styled(text.to_owned(), style));
    }
}

/// 链接（`markdown.ts:2572-2589`）：文字 = `md_link` + `Modifier::UNDERLINED`；
/// URL = `md_link_url` 色、带圆括号，与文字间一空格；文字与 href 相等（或 `mailto:`
/// 去前缀后相等）时只输出文字，不重复展示 URL。
fn render_link(
    text: &[Inline],
    url: &str,
    base: Style,
    theme: &Theme,
    out: &mut Vec<Span<'static>>,
) {
    let link_style = base.patch(
        Style::new()
            .fg(theme.colors.md_link)
            .add_modifier(Modifier::UNDERLINED),
    );
    if text.is_empty() {
        push_text(out, url, link_style);
    } else {
        out.extend(render_inline(text, link_style, theme));
    }

    let text_plain = inline_plain_text(text);
    let url_without_mailto = url.strip_prefix("mailto:").unwrap_or(url);
    let same = text_plain == url || text_plain == url_without_mailto;
    if !same && !url.is_empty() {
        let url_style = base.patch(Style::new().fg(theme.colors.md_link_url));
        push_text(out, " ", base);
        push_text(out, &format!("({url})"), url_style);
    }
}

fn inline_plain_text(inlines: &[Inline]) -> String {
    let mut out = String::new();
    collect_plain_text(inlines, &mut out);
    out
}

fn collect_plain_text(inlines: &[Inline], out: &mut String) {
    for inline in inlines {
        match inline {
            Inline::Text(t) | Inline::Code(t) => out.push_str(t),
            Inline::Emphasis(c) | Inline::Strong(c) | Inline::Strikethrough(c) => {
                collect_plain_text(c, out);
            }
            Inline::Link { text, .. } => collect_plain_text(text, out),
            Inline::SoftBreak | Inline::HardBreak => out.push(' '),
        }
    }
}

// ============================================================================
// 行前缀工具：列表悬挂缩进、引用竖条、代码块缩进共用
// ============================================================================

fn prefix_line(mut line: Line<'static>, prefix: &[Span<'static>]) -> Line<'static> {
    let mut spans = prefix.to_vec();
    spans.append(&mut line.spans);
    Line {
        spans,
        style: line.style,
        alignment: line.alignment,
    }
}

/// 首行贴 `first_prefix`、其余物理行贴 `rest_prefix`——列表用它实现「首行 bullet、
/// 续行悬挂缩进」，引用块用它实现「每行都要重复竖条」（此时 `first_prefix ==
/// rest_prefix`）。
fn indent_lines(
    lines: Vec<Line<'static>>,
    first_prefix: &[Span<'static>],
    rest_prefix: &[Span<'static>],
) -> Vec<Line<'static>> {
    lines
        .into_iter()
        .enumerate()
        .map(|(i, line)| prefix_line(line, if i == 0 { first_prefix } else { rest_prefix }))
        .collect()
}

#[cfg(test)]
mod tests {
    use ratatui::style::{Modifier, Style};
    use zcode_text::width::visible_width;

    use super::{MarkdownOptions, render_markdown};
    use crate::theme::{BuiltinTheme, ColorMode, SymbolPreset, Theme};
    use crate::wrap::line_width;

    fn dark_theme() -> Theme {
        BuiltinTheme::Dark
            .load(ColorMode::TrueColor, SymbolPreset::Unicode)
            .expect("内置暗色主题必须能解析")
    }

    fn opts() -> MarkdownOptions {
        MarkdownOptions {
            code_block_indent: 0,
            base: Style::default(),
            highlight: false,
        }
    }

    fn line_text(line: &ratatui::text::Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    // ---- 标题三级形态 ----

    #[test]
    fn heading_levels_render_distinct_forms() {
        let theme = dark_theme();
        let lines = render_markdown("# H1\n\n## H2\n\n### H3\n", 80, &theme, &opts());
        // 块间距：三个标题两两之间各插一条空行。
        assert_eq!(lines.len(), 5);

        assert_eq!(line_text(&lines[0]), "H1");
        let h1_style = lines[0].spans[0].style;
        assert_eq!(h1_style.fg, Some(theme.colors.md_heading));
        assert!(h1_style.add_modifier.contains(Modifier::BOLD));
        assert!(h1_style.add_modifier.contains(Modifier::UNDERLINED));

        assert_eq!(line_text(&lines[1]), "");
        assert_eq!(line_text(&lines[2]), "H2");
        let h2_style = lines[2].spans[0].style;
        assert_eq!(h2_style.fg, Some(theme.colors.md_heading));
        assert!(h2_style.add_modifier.contains(Modifier::BOLD));
        assert!(!h2_style.add_modifier.contains(Modifier::UNDERLINED));

        assert_eq!(line_text(&lines[3]), "");
        // H3 保留字面 `### ` 前缀，不像 H1/H2 那样剥掉。
        assert_eq!(line_text(&lines[4]), "### H3");
    }

    // ---- 行内代码不带反引号 ----

    #[test]
    fn inline_code_has_no_backticks_and_own_color() {
        let theme = dark_theme();
        let lines = render_markdown("Use `foo` here.\n", 80, &theme, &opts());
        assert_eq!(lines.len(), 1);
        let text = line_text(&lines[0]);
        assert_eq!(text, "Use foo here.");
        assert!(!text.contains('`'));
        let code_span = lines[0]
            .spans
            .iter()
            .find(|s| s.content.as_ref() == "foo")
            .expect("行内代码 span 必须存在");
        assert_eq!(code_span.style.fg, Some(theme.colors.md_code));
    }

    // ---- 代码块围栏与缩进 ----

    #[test]
    fn code_block_fence_and_indent() {
        let theme = dark_theme();
        let options = MarkdownOptions {
            code_block_indent: 2,
            base: Style::default(),
            highlight: false,
        };
        let lines = render_markdown("```rust\nlet x = 1;\n```\n", 80, &theme, &options);
        assert_eq!(lines.len(), 3);
        assert_eq!(line_text(&lines[0]), "```rust");
        assert_eq!(
            lines[0].spans[0].style.fg,
            Some(theme.colors.md_code_block_border)
        );
        assert_eq!(line_text(&lines[1]), "  let x = 1;");
        assert_eq!(line_text(&lines[2]), "```");
        assert_eq!(
            lines[2].spans[0].style.fg,
            Some(theme.colors.md_code_block_border)
        );
    }

    #[test]
    fn code_block_without_lang_has_empty_fence_and_no_indent_by_default() {
        let theme = dark_theme();
        let lines = render_markdown("```\nplain\n```\n", 80, &theme, &opts());
        assert_eq!(lines.len(), 3);
        assert_eq!(line_text(&lines[0]), "```");
        assert_eq!(line_text(&lines[1]), "plain");
        assert_eq!(line_text(&lines[2]), "```");
    }

    // ---- 列表悬挂缩进（"10. " 这种 4 列前缀） ----

    #[test]
    fn ordered_list_hanging_indent_matches_bullet_width() {
        let theme = dark_theme();
        // bullet "10. " 宽 4 列；width=14 时内容宽 = 14-4=10，长文本会在第 10 个字符处断行。
        let md = "10. abcdefghijklmno\n11. next\n";
        let lines = render_markdown(md, 14, &theme, &opts());
        assert_eq!(visible_width("10. "), 4);
        assert_eq!(line_text(&lines[0]), "10. abcdefghij");
        // 续行悬挂缩进必须等于 bullet 的实际显示宽度（4 列空格），不是固定 2 列。
        assert_eq!(line_text(&lines[1]), "    klmno");
        assert_eq!(line_text(&lines[2]), "11. next");
    }

    #[test]
    fn unordered_list_bullet_comes_from_theme_symbols() {
        let theme = dark_theme();
        let lines = render_markdown("- item a\n- item b\n", 80, &theme, &opts());
        assert_eq!(lines.len(), 2);
        let bullet = theme.symbols.md.bullet;
        assert_eq!(line_text(&lines[0]), format!("{bullet} item a"));
        assert_eq!(line_text(&lines[1]), format!("{bullet} item b"));
    }

    #[test]
    fn task_list_uses_choice_symbols() {
        let theme = dark_theme();
        let lines = render_markdown("- [ ] todo\n- [x] done\n", 80, &theme, &opts());
        assert_eq!(lines.len(), 2);
        assert_eq!(
            line_text(&lines[0]),
            format!("{} todo", theme.symbols.choice.unchecked)
        );
        assert_eq!(
            line_text(&lines[1]),
            format!("{} done", theme.symbols.choice.checked)
        );
    }

    // ---- 引用块前缀与内容宽 ----

    #[test]
    fn blockquote_prefix_and_content_width() {
        let theme = dark_theme();
        let border = theme.symbols.md.quote_border;
        let border_width = visible_width(border) + 1; // 竖条 + 一个空格
        let width = 20usize;
        let lines = render_markdown(
            "> quoted text that wraps under narrow width\n",
            u16::try_from(width).unwrap_or(u16::MAX),
            &theme,
            &opts(),
        );
        assert!(!lines.is_empty());
        for line in &lines {
            let text = line_text(line);
            assert!(
                text.starts_with(border),
                "每一行都必须重复引用前缀：{text:?}"
            );
            assert!(
                line_width(line) <= width,
                "行宽不能超过传入的 width：{line:?}"
            );
        }
        // 内容宽 = max(1, width - 2)：整行宽度不应超过 border 宽度 + 内容宽。
        let content_width = width.saturating_sub(2).max(1);
        for line in &lines {
            assert!(line_width(line) <= border_width + content_width);
        }
    }

    // ---- 链接同文不重复输出 URL ----

    #[test]
    fn link_with_matching_text_and_url_shown_once() {
        let theme = dark_theme();
        let lines = render_markdown(
            "[https://example.com](https://example.com)\n",
            80,
            &theme,
            &opts(),
        );
        assert_eq!(lines.len(), 1);
        assert_eq!(line_text(&lines[0]), "https://example.com");
    }

    #[test]
    fn link_with_mailto_prefix_stripped_still_dedups() {
        let theme = dark_theme();
        let lines = render_markdown("[a@b.com](mailto:a@b.com)\n", 80, &theme, &opts());
        assert_eq!(lines.len(), 1);
        assert_eq!(line_text(&lines[0]), "a@b.com");
    }

    #[test]
    fn link_with_distinct_text_shows_url_in_parens() {
        let theme = dark_theme();
        let lines = render_markdown("[foo](http://foo.test)\n", 80, &theme, &opts());
        assert_eq!(lines.len(), 1);
        assert_eq!(line_text(&lines[0]), "foo (http://foo.test)");
        let url_span = lines[0]
            .spans
            .iter()
            .find(|s| s.content.as_ref() == "(http://foo.test)")
            .expect("URL span 必须存在");
        assert_eq!(url_span.style.fg, Some(theme.colors.md_link_url));
    }

    // ---- 表格列宽分配与极窄时回退 ----

    #[test]
    fn table_natural_width_used_when_it_fits() {
        let theme = dark_theme();
        let md = "| A | B |\n| --- | --- |\n| 1 | 12345678 |\n";
        // n=2，overhead=3*2+1=7；natural=[1,8]，sum=9；width=30 时 budget=23 >= 9，直接用 natural。
        let lines = render_markdown(md, 30, &theme, &opts());
        // 4 行：顶边框、表头、分隔线、一行数据、底边框 = 5 行。
        assert_eq!(lines.len(), 5);
        for line in &lines {
            assert_eq!(
                line_width(line),
                1 + 8 + 7,
                "每行总宽必须等于 natural 宽度之和加边框开销：{line:?}"
            );
        }
    }

    #[test]
    fn table_falls_back_to_wrapped_text_when_too_narrow() {
        let theme = dark_theme();
        let md = "| Name | Description |\n| --- | --- |\n| Foo | Bar |\n";
        // n=2，overhead=7，budget < 2 需要 width <= 8。
        let lines = render_markdown(md, 8, &theme, &opts());
        let border_char = theme.symbols.box_sharp.vertical;
        for line in &lines {
            assert!(
                !line_text(line).contains(border_char),
                "退化路径不应再画表格边框：{line:?}"
            );
            assert!(line_width(line) <= 8);
        }
    }

    // ---- 块间空行不双插 ----

    #[test]
    fn blank_lines_between_blocks_are_not_doubled() {
        let theme = dark_theme();
        let md = "Para one.\n\n- item a\n- item b\n\nPara two.\n";
        let lines = render_markdown(md, 80, &theme, &opts());
        let texts: Vec<String> = lines.iter().map(line_text).collect();
        // 段落后紧跟列表：不加空行。
        assert_eq!(texts[0], "Para one.");
        assert_ne!(texts[1], "", "段落后紧跟列表不应插入空行");
        // 列表内部（item a / item b 之间）不加空行。
        let bullet = theme.symbols.md.bullet;
        assert_eq!(texts[1], format!("{bullet} item a"));
        assert_eq!(texts[2], format!("{bullet} item b"));
        // 列表后面还有别的块：按通用规则插一条空行，且只插一条（不会看到连续两个空行）。
        assert_eq!(texts[3], "");
        assert_eq!(texts[4], "Para two.");
        for pair in texts.windows(2) {
            assert!(
                !(pair[0].is_empty() && pair[1].is_empty()),
                "不应出现连续两行空行"
            );
        }
    }

    // ---- 宽度为 0 / 极窄终端不 panic ----

    #[test]
    fn zero_width_does_not_panic() {
        let theme = dark_theme();
        let md = "# H\n\nplain **bold** text\n\n- item\n\n> quote\n\n```rust\ncode\n```\n\n| a | b |\n| - | - |\n| 1 | 2 |\n";
        let lines = render_markdown(md, 0, &theme, &opts());
        assert!(!lines.is_empty());
    }
}
