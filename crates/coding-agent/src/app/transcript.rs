//! transcript 的语义模型（[`Block`]）与"状态 → 行"的纯渲染函数（[`render_block`]）。
//!
//! [`render_block`] 不触碰终端、不触碰 [`zcode_tui`] 的任何可变状态，只吃 `&Block`、
//! 宽度和 [`Theme`]，吐 `Vec<Line<'static>>`——可以直接用普通单元测试断言，不需要
//! 真终端。[`BlockComponent`] 是它与 [`zcode_tui::Composer`] 之间的适配层，本身不含
//! 渲染逻辑。
//!
//! # 视觉形态（对标 oh-my-pi）
//!
//! | 块 | 形态 | 出处 |
//! | --- | --- | --- |
//! | 用户 | **无前缀字符**，整行铺满 `userMessageBg`，左右各 1 列 padding，上下各 1 行全宽背景空行 | `packages/coding-agent/src/modes/components/user-message.ts:46-51` |
//! | 助手 | 无前缀，左 1 列 padding、上下不留白，正文过 markdown，代码块不额外缩进 | `assistant-message.ts:879-883` |
//! | 思考 | `thinkingText` 色 + 斜体的 markdown | `assistant-message.ts:902-905` |
//! | 工具 | 圆角卡片：状态头嵌顶边 + 输出区，按状态铺 `toolPendingBg`/`SuccessBg`/`ErrorBg` | `packages/coding-agent/src/tui/output-block.ts:64-206` |
//!
//! 「用户消息没有 `›` 前缀」不是遗漏：说话人靠**背景色**区分（用户有底色、助手没有），
//! 这样多行消息不需要每行都对齐一个装饰字符，长段落读起来是一整块。
//!
//! # 工具卡片的状态图标为什么不转
//!
//! 上游默认卡片头挂 spinner，但它的 write/edit 渲染器**刻意不挂**
//! （`packages/coding-agent/src/tools/write.ts:1506-1516` 的注释）：头行是块的第一行，
//! 而 transcript 往终端历史的提交是**前缀式**的，头行每 80 ms 变一次字节会把提交
//! 边界永久钉死在块顶。本仓的 `insert_history` 提交语义相同（见
//! `crates/tui/src/ledger.rs` 的 C/W/B 账本），所以三态一律用静态图标，动画只留给
//! 状态行——它本来就在 pinned live region 里，重画不进历史。

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
#[cfg(test)]
use zcode_protocol::wire::types::EntryId;
use zcode_protocol::wire::types::{
    AssistantContent, CallId, CompactionReason, DisplayRole, Entry, EntryKind, Message,
    ToolResultContent, UserContent,
};

use zcode_tui::card::{
    Card, Section, StatusLine, pad_line, patch_line, render_status_line, truncate_line,
};
use zcode_tui::markdown::{MarkdownOptions, render_markdown};
use zcode_tui::theme::Theme;
use zcode_tui::{Component, ComponentId};

/// 一个工具调用块当前的执行状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToolStatus {
    /// 正在执行：块处于 live 状态，可能被账本判定为可变尾部。
    Running,
    /// 执行完成，无错误。
    Done,
    /// 执行完成，返回了错误结果。
    Failed,
}

/// transcript 里的一个语义块。
#[derive(Debug, Clone)]
pub(crate) enum Block {
    /// 用户输入。
    User {
        /// 内容块（文本 + 内联图片，图片只显示占位）。
        content: Vec<UserContent>,
    },
    /// 助手输出，`streaming == true` 时仍可能变化。
    Assistant {
        /// 内容块：文本、思考、（极少数）已加密思考。工具调用另走 [`Block::Tool`]，
        /// 不在这里重复渲染，避免同一次调用出现两条。
        content: Vec<AssistantContent>,
        /// 是否仍在流式输出中。
        streaming: bool,
        /// 展示节奏游标：流式期间只展示拼接文本的前 `revealed` 个字符
        /// （由 [`crate::app::reveal::RevealPacer`] 驱动，见任务第 4 条"到达 ≠
        /// 展示"）。`None` 表示展示全部——已定稿的历史消息、以及尚未开始节奏
        /// 控制的早期状态都是这个值。
        revealed: Option<usize>,
    },
    /// 一次工具调用：名字 + 累积输出（来自 `ToolProgress` 的增量拼接）+ 状态。
    Tool {
        /// 调用 id，用于 debug 展示与去重，不参与渲染。
        #[allow(dead_code)]
        call_id: CallId,
        /// 工具名。
        name: String,
        /// 累积输出文本。
        output: String,
        /// 当前状态。
        status: ToolStatus,
    },
    /// 系统提示行：模型切换、压缩摘要、连接状态、错误提示等。
    System {
        /// 展示文本，可能跨行。
        text: String,
    },
}

impl Block {
    /// 该块此刻是否仍可能变化（供 [`BlockComponent::live_boundary`] 与
    /// [`crate::app::state::AppState::live_region_height`] 共用）。
    pub(crate) fn is_live(&self) -> bool {
        match self {
            Self::Assistant { streaming, .. } => *streaming,
            Self::Tool { status, .. } => matches!(status, ToolStatus::Running),
            Self::User { .. } | Self::System { .. } => false,
        }
    }
}

/// 单个仍在直播的块最多同时占用的可见行数。
///
/// 量级借自 oh-my-pi `packages/coding-agent/src/tools/render-utils.ts:215-224`
/// （`PREVIEW_WINDOW_RESERVED_ROWS = 20`）。**注意上游那个 20 是从终端高度里扣掉的
/// 预留量**（`previewWindowRows() = max(6, rows - 20)`），不是窗口高度本身；本仓
/// 这里当固定窗口高度用，是有意简化——`render_block` 拿不到终端高度。
///
/// 因此它**不是**「活跃区不超屏」的保证：24 行终端上，折叠提示 1 + 本 cap 20 +
/// 状态行 1 + 带边框的输入框 3 就已经是 25 行。真正兜底的是
/// [`zcode_tui::Emitter::render`] 的溢出保护——活跃区高过屏幕时本帧降级 unpinned，
/// 让超出的行以冻结快照提交进历史而不是消失（duplication, never loss）。
///
/// 这个 cap 剩下的作用是**体感**：不让一个刷屏的工具输出把整块屏幕占满，
/// 使状态行与输入框在常见高度（24-50 行）下仍留在视野里。
pub(crate) const LIVE_BLOCK_TAIL_CAP: usize = 20;

/// transcript 里的一条记录：稳定 id + 版本号 + 内容。
#[derive(Debug, Clone)]
pub(crate) struct TranscriptEntry {
    pub(crate) id: ComponentId,
    pub(crate) revision: u64,
    pub(crate) block: Block,
}

/// 把一条协议 [`Entry`]（[`Reply::Subscribed`](zcode_protocol::wire::Reply::Subscribed)
/// 或 [`Reply::History`](zcode_protocol::wire::Reply::History) 里的一条历史）转换成
/// 可展示的 [`Block`]。`None` 表示这条 entry 不产生独立的 transcript 行
/// （目前只有 `TitleChange`：标题变化体现在会话头部，不在正文里重复）。
pub(crate) fn entry_to_block(entry: &Entry, show_thinking: bool) -> Option<Block> {
    match &entry.kind {
        EntryKind::SessionInit { cwd, model } => Some(Block::System {
            text: format!("会话开始 · {cwd} · {model}"),
        }),
        EntryKind::Message { message } => Some(message_to_block(message, show_thinking)),
        EntryKind::ModelChange { model } => Some(Block::System {
            text: format!("模型切换为 {model}"),
        }),
        EntryKind::TitleChange { .. } => None,
        EntryKind::Compaction {
            summary, reason, ..
        } => Some(Block::System {
            text: format!("{} · {summary}", compaction_reason_label(*reason)),
        }),
    }
}

fn compaction_reason_label(reason: CompactionReason) -> &'static str {
    match reason {
        CompactionReason::Threshold => "上下文已自动压缩",
        CompactionReason::Overflow => "上下文超限，已压缩",
        CompactionReason::Manual => "已手动压缩上下文",
        CompactionReason::Unknown => "上下文已压缩",
    }
}

/// 把一条完整（已定稿）的 [`Message`] 转换成 [`Block`]。用于 `MessageEnd` 落定后的
/// 替换，以及从历史条目还原。
///
/// `display_role` 不是装饰：`Some(DisplayRole::System)` 的 user 消息在 API 层是用户消息，
/// 在 UI 层**必须**显示成系统消息，否则用户会以为那句话是自己说的
/// （契约见 `crates/agent/src/session/message.rs:212-217`）。会话开头那段
/// `<system-reminder>` 环境上下文正是这一类——真机上它曾顶着 `›` 前缀出现，
/// 看起来像是用户自己打了十行 git status。
pub(crate) fn message_to_block(message: &Message, show_thinking: bool) -> Block {
    match message {
        Message::User {
            content,
            display_role: Some(DisplayRole::System),
        } => Block::System {
            text: user_content_text(content),
        },
        Message::User { content, .. } => Block::User {
            content: content.clone(),
        },
        Message::Assistant { content, .. } => Block::Assistant {
            content: filter_thinking(content, show_thinking),
            streaming: false,
            revealed: None,
        },
        Message::ToolResult {
            tool_name,
            content,
            is_error,
            tool_call_id,
        } => Block::Tool {
            call_id: tool_call_id.clone(),
            name: tool_name.clone(),
            output: tool_result_text(content),
            status: if *is_error {
                ToolStatus::Failed
            } else {
                ToolStatus::Done
            },
        },
    }
}

/// 按 `show_thinking` 过滤助手内容里的思考块。
///
/// 在**入库**这一步滤掉而不是渲染时滤掉：`RevealPacer` 的 backlog 按内容字符数算，
/// 留着不显示的思考文本会让流式展示节奏莫名其妙地卡顿。
fn filter_thinking(content: &[AssistantContent], show_thinking: bool) -> Vec<AssistantContent> {
    if show_thinking {
        return content.to_vec();
    }
    content
        .iter()
        .filter(|item| {
            !matches!(
                item,
                AssistantContent::Thinking { .. } | AssistantContent::RedactedThinking { .. }
            )
        })
        .cloned()
        .collect()
}

fn tool_result_text(content: &[ToolResultContent]) -> String {
    let mut out = String::new();
    for item in content {
        match item {
            ToolResultContent::Text { text } => {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(text);
            }
            ToolResultContent::Image { .. } => {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str("[图片]");
            }
        }
    }
    out
}

/// 用户气泡与助手正文的左右内边距（列）。
///
/// 出处 oh-my-pi：用户消息 `user-message.ts:46` 与助手消息 `assistant-message.ts:880`
/// 都传 `paddingX = 1`。1 列足够让文字不贴住背景块边缘，又不浪费窄终端的横向空间。
const CONTENT_PADDING: usize = 1;

/// 助手正文里代码块的额外左缩进（列）。
///
/// 上游 `Markdown` 组件默认 2（`markdown.ts:1479`），但助手消息**显式传 0**
/// （`assistant-message.ts:880`）：助手正文本身已经有 1 列 padding，代码围栏再缩
/// 2 列会让代码在窄终端里被挤掉 3 列。
const ASSISTANT_CODE_INDENT: u16 = 0;

/// 助手内容里一段连续的同类文本。
///
/// 分段的意义在于 thinking 与正文要走**不同的 markdown 样式**（斜体 + 弱色 vs
/// 正常），而 `revealed` 游标是按整块拼接文本的字符数算的——两者必须切在同一个
/// 坐标系里，否则流式展示到一半时思考块会闪。
#[derive(Debug, Clone, PartialEq, Eq)]
enum Segment {
    /// 正文。
    Text(String),
    /// 思考内容。
    Thinking(String),
}

/// 把助手内容切成分段，并按 `revealed`（已展示的字符数）截断。
///
/// `revealed == None` 表示全部展示（历史消息、已定稿消息）。字符计数与
/// [`assistant_char_len`] 严格一致：两者遍历同一批内容、按同样规则累加，
/// 所以「节奏游标算的字符数」与「实际渲染的文本」永远是同一份数据的两个视图。
fn assistant_segments(content: &[AssistantContent], revealed: Option<usize>) -> Vec<Segment> {
    let mut budget = revealed.unwrap_or(usize::MAX);
    let mut out = Vec::new();
    for item in content {
        if budget == 0 {
            break;
        }
        let (text, is_thinking) = match item {
            AssistantContent::Text { text } => (text.as_str(), false),
            AssistantContent::Thinking { text, .. } => (text.as_str(), true),
            // 已加密的思考只能整块显示或整块折叠，绝不截断——契约如此。
            AssistantContent::RedactedThinking { .. } => (REDACTED_THINKING, true),
            AssistantContent::ToolCall { .. } => continue,
        };
        let taken: String = text.chars().take(budget).collect();
        budget = budget.saturating_sub(taken.chars().count());
        if taken.is_empty() {
            continue;
        }
        out.push(if is_thinking {
            Segment::Thinking(taken)
        } else {
            Segment::Text(taken)
        });
    }
    out
}

/// 已加密思考的占位文本。
const REDACTED_THINKING: &str = "[思考内容已由提供商加密]";

/// 纯函数：把一个 [`Block`] 渲染成若干条已按 `width` 排好版的 `Line`。
///
/// 不访问任何全局/终端状态，可以直接单元测试。
#[must_use]
pub(crate) fn render_block(block: &Block, width: u16, theme: &Theme) -> Vec<Line<'static>> {
    let width = usize::from(width.max(1));
    let mut out = match block {
        Block::User { content } => render_user(content, width, theme),
        Block::Assistant {
            content,
            revealed,
            streaming,
        } => render_assistant(content, *revealed, *streaming, width, theme),
        Block::Tool {
            name,
            output,
            status,
            ..
        } => render_tool(name, output, *status, width, theme),
        Block::System { text } => render_system(text, width, theme),
    };
    if block.is_live() && out.len() > LIVE_BLOCK_TAIL_CAP {
        let hidden = out.len().saturating_sub(LIVE_BLOCK_TAIL_CAP);
        let header = Line::from(Span::styled(
            format!(
                "{}{} 已折叠 {hidden} 行（定稿后完整可见）",
                " ".repeat(CONTENT_PADDING),
                theme.symbols.format.ellipsis
            ),
            theme.dim(),
        ));
        let tail_start = out.len().saturating_sub(LIVE_BLOCK_TAIL_CAP);
        let mut windowed = Vec::with_capacity(LIVE_BLOCK_TAIL_CAP.saturating_add(1));
        windowed.push(header);
        windowed.extend(out.drain(tail_start..));
        return windowed;
    }
    out
}

/// 用户气泡：整行铺满 `userMessageBg`，上下各一行全宽背景空行。
///
/// 背景铺**到容器右边缘**而不是文本宽度
/// （`packages/tui/src/components/markdown.ts:1966` → `packages/tui/src/utils.ts:588-595`）：
/// 只铺文本宽会让气泡右侧成锯齿。
fn render_user(content: &[UserContent], width: usize, theme: &Theme) -> Vec<Line<'static>> {
    let bubble = Style::new().bg(theme.bg.user_message);
    let inner = width.saturating_sub(CONTENT_PADDING * 2).max(1);
    let body = render_markdown(
        &user_content_text(content),
        u16::try_from(inner).unwrap_or(u16::MAX),
        theme,
        &MarkdownOptions {
            base: Style::new().fg(theme.colors.user_message_text),
            // 用户气泡里的代码块跟随上游默认（缩 2 列）：气泡有底色，代码块比
            // 围栏窄一格才能一眼看出边界（`markdown.ts:2276-2311`）。
            code_block_indent: 2,
            highlight: true,
        },
    );
    let mut out = Vec::with_capacity(body.len().saturating_add(2));
    out.push(blank(width, bubble));
    for line in body {
        out.push(patch_line(indent(line, CONTENT_PADDING, width), bubble));
    }
    out.push(blank(width, bubble));
    out
}

/// 助手正文：无背景、无前缀，左 1 列 padding。
///
/// `paddingY = 0` 是刻意的（`assistant-message.ts:880` 的注释）：助手文本后面常常
/// 紧跟工具卡片，带上下 padding 会在卡片上方多出一行。块间距归容器管。
fn render_assistant(
    content: &[AssistantContent],
    revealed: Option<usize>,
    streaming: bool,
    width: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let inner = width.saturating_sub(CONTENT_PADDING * 2).max(1);
    let inner_u16 = u16::try_from(inner).unwrap_or(u16::MAX);
    let segments = assistant_segments(content, revealed);
    if segments.is_empty() {
        // 流式刚开始、尚无字符：仍然占一行。0 行的组件在 compose 里等价于"不存在"，
        // 状态行会失去它本该标注的 live 锚点。
        return vec![Line::default()];
    }
    let mut out = Vec::new();
    for segment in segments {
        if !out.is_empty() {
            out.push(Line::default());
        }
        let (text, opts) = match &segment {
            Segment::Text(text) => (
                text,
                MarkdownOptions {
                    base: theme.text(),
                    code_block_indent: ASSISTANT_CODE_INDENT,
                    // 流式期间不高亮：这个块每帧都重渲染，而 syntect 的成本随代码
                    // 行数线性涨——实测 40 行 7.9 ms、200 行 40 ms
                    // （`cargo run -p zcode-tui --release --example render_cost`），
                    // 后者单块就吃穿 30 fps 的整个帧预算。定稿后那一帧再上色。
                    // 上游同样在流式期间跳过高亮：`highlightCode && (!transientRenderCache
                    // || renderingFrozenPrefix)`（`packages/tui/src/components/markdown.ts:2008`）。
                    highlight: !streaming,
                },
            ),
            Segment::Thinking(text) => (
                text,
                MarkdownOptions {
                    base: Style::new()
                        .fg(theme.colors.thinking_text)
                        .add_modifier(Modifier::ITALIC),
                    code_block_indent: ASSISTANT_CODE_INDENT,
                    // 思考内容里的代码块不高亮：它是模型的草稿，语法色会喧宾夺主，
                    // 而且它每帧都在变，重新 tokenize 是纯浪费（与上面正文段
                    // 同一条理由，见 `markdown.ts:2008`）。
                    highlight: false,
                },
            ),
        };
        for line in render_markdown(text, inner_u16, theme, &opts) {
            out.push(indent(line, CONTENT_PADDING, width));
        }
    }
    out
}

/// 工具调用：圆角卡片，状态头嵌在顶边上。
fn render_tool(
    name: &str,
    output: &str,
    status: ToolStatus,
    width: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let (glyph, glyph_style, bg) = match status {
        ToolStatus::Running => (
            theme.symbols.status.running,
            Style::new().fg(theme.colors.warning),
            theme.bg.tool_pending,
        ),
        ToolStatus::Done => (
            theme.symbols.status.success,
            Style::new().fg(theme.colors.success),
            theme.bg.tool_success,
        ),
        ToolStatus::Failed => (
            theme.symbols.status.error,
            Style::new().fg(theme.colors.error),
            theme.bg.tool_error,
        ),
    };
    let header = render_status_line(
        &StatusLine {
            icon: Some(Span::styled(glyph.to_owned(), glyph_style)),
            title: name,
            title_style: theme.title(),
            description: None,
            badge: None,
            meta: &[],
        },
        theme,
    );
    let content_width =
        zcode_tui::card::card_content_width(u16::try_from(width).unwrap_or(u16::MAX), 1, 1);
    let body: Vec<Line<'static>> = output
        .lines()
        .flat_map(|raw| {
            zcode_tui::wrap::wrap_line(
                &Line::from(Span::styled(
                    raw.to_owned(),
                    Style::new().fg(theme.colors.tool_output),
                )),
                usize::from(content_width),
            )
        })
        .collect();
    let sections = [Section {
        label: None,
        lines: &body,
    }];
    zcode_tui::card::render_card(
        &Card {
            header: Some(header),
            sections: &sections,
            border: Style::new().fg(theme.colors.border_muted),
            bg: Some(bg),
            padding_left: 1,
            padding_right: 1,
        },
        u16::try_from(width).unwrap_or(u16::MAX),
        theme,
    )
}

/// 系统提示行：信息图标 + 弱色文本，左 1 列 padding，与助手正文对齐。
fn render_system(text: &str, width: usize, theme: &Theme) -> Vec<Line<'static>> {
    let inner = width.saturating_sub(CONTENT_PADDING * 2).max(1);
    let prefix = format!("{} ", theme.symbols.status.info);
    let hang = " ".repeat(zcode_text::width::visible_width(&prefix));
    let mut out = Vec::new();
    for (idx, raw) in text.split('\n').enumerate() {
        let head = if idx == 0 {
            prefix.as_str()
        } else {
            hang.as_str()
        };
        let styled = Line::from(Span::styled(format!("{head}{raw}"), theme.muted()));
        for line in zcode_tui::wrap::wrap_line(&styled, inner) {
            out.push(indent(line, CONTENT_PADDING, width));
        }
    }
    out
}

/// 一行全宽空白，带 `style`。
fn blank(width: usize, style: Style) -> Line<'static> {
    Line::from(Span::styled(" ".repeat(width), style))
}

/// 给一行加左缩进，并把它**钳制到恰好 `width` 列**：不足补白、超出按显示宽度截断。
///
/// 补白与缩进都不带样式，由调用方按需 patch——气泡要让它们跟着上背景色，助手正文
/// 则不需要。
///
/// 为什么要截断：markdown 的表格排版在某些宽度下会产出比 `width` 略宽的行
/// （列宽分配的整数余数），而 `pad_line` 只补不切。一行超宽就会被终端软换行，
/// 于是本层数出的行数与终端实际占用的行数对不上——账本按逻辑行推进 viewport 锚点，
/// 差一行就是后续内容被覆盖。宁可切掉表格最右一列的边框，也不能让行数账本失真。
fn indent(line: Line<'static>, pad: usize, width: usize) -> Line<'static> {
    let mut spans = Vec::with_capacity(line.spans.len().saturating_add(2));
    spans.push(Span::raw(" ".repeat(pad)));
    spans.extend(line.spans);
    let joined = Line {
        style: line.style,
        alignment: line.alignment,
        spans,
    };
    let clipped = truncate_line(&joined, width, "");
    pad_line(clipped, width, Style::default())
}

fn user_content_text(content: &[UserContent]) -> String {
    let mut out = String::new();
    for item in content {
        match item {
            UserContent::Text { text } => {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(text);
            }
            UserContent::Image { .. } => {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str("[图片]");
            }
        }
    }
    out
}

/// 助手内容参与展示节奏的字符数，供 [`crate::app::state::AppState::tick`] 计算
/// backlog（到达字符数 − 已展示字符数）。
///
/// 与 [`assistant_segments`] 严格同源：遍历同一批内容、按同样规则累加**裸文本**
/// 字符（不含前缀、缩进、段间空行这些渲染期装饰），所以「节奏游标算的字符数」
/// 与「实际被 `revealed` 截断的文本」是同一个坐标系。两边一旦漂移，`RevealPacer`
/// 就会按偏大或偏小的 backlog 推进游标，流式输出肉眼可见地闪跳。
/// 工具调用不计入——它由独立的 [`Block::Tool`] 渲染。
#[must_use]
pub(crate) fn assistant_char_len(content: &[AssistantContent]) -> usize {
    content
        .iter()
        .map(|item| match item {
            AssistantContent::Text { text } | AssistantContent::Thinking { text, .. } => {
                text.chars().count()
            }
            AssistantContent::RedactedThinking { .. } => REDACTED_THINKING.chars().count(),
            AssistantContent::ToolCall { .. } => 0,
        })
        .sum()
}

/// [`Block`] 在 [`zcode_tui::Composer`] 里的组件外观。
#[derive(Debug)]
pub(crate) struct BlockComponent<'a> {
    entry: &'a TranscriptEntry,
    theme: &'a Theme,
}

impl<'a> BlockComponent<'a> {
    /// 包一层 `&TranscriptEntry`，供 [`zcode_tui::Composer::compose`] 调用。
    ///
    /// `theme` 借用而非克隆：一帧里所有块共用同一份主题，克隆 60 多个 `Color`
    /// ×块数纯属浪费。主题在整个进程生命周期内不变，借用不会打断 `Composer`
    /// 的 revision 复用。
    pub(crate) fn new(entry: &'a TranscriptEntry, theme: &'a Theme) -> Self {
        Self { entry, theme }
    }
}

impl Component for BlockComponent<'_> {
    fn id(&self) -> ComponentId {
        self.entry.id
    }

    fn revision(&self) -> u64 {
        self.entry.revision
    }

    fn render(&self, width: u16) -> Vec<Line<'static>> {
        render_block(&self.entry.block, width, self.theme)
    }

    fn live_boundary(&self) -> Option<usize> {
        self.entry.block.is_live().then_some(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_theme;
    use zcode_tui::wrap::line_width;

    fn text_of(lines: &[Line<'static>]) -> String {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// 用户消息靠**整行铺满的背景色**与助手区分，不靠前缀字符。
    ///
    /// 三条同时成立才算对：文字在；每一行都恰好占满 `width`（否则气泡右侧成锯齿）；
    /// 每一行的每个 cell 都带背景（否则会缺角）。
    #[test]
    fn user_bubble_fills_every_row_with_background() {
        let theme = test_theme();
        let block = Block::User {
            content: vec![UserContent::Text {
                text: "帮我修个 bug".to_owned(),
            }],
        };
        let lines = render_block(&block, 40, &theme);
        assert!(text_of(&lines).contains("帮我修个 bug"));
        assert!(lines.len() >= 3, "至少上空行 + 正文 + 下空行：{lines:?}");
        for line in &lines {
            assert_eq!(line_width(line), 40, "气泡每行必须铺满整宽：{line:?}");
            for span in &line.spans {
                assert_eq!(
                    span.style.bg,
                    Some(theme.bg.user_message),
                    "气泡里的每个 span 都要带底色：{span:?}"
                );
            }
        }
    }

    /// 助手正文不铺背景——说话人区分只靠「用户有底色」这一个信号。
    #[test]
    fn assistant_body_has_no_background() {
        let theme = test_theme();
        let block = Block::Assistant {
            content: vec![AssistantContent::Text {
                text: "好的".to_owned(),
            }],
            streaming: false,
            revealed: None,
        };
        for line in render_block(&block, 40, &theme) {
            for span in &line.spans {
                assert_eq!(span.style.bg, None, "助手正文不该有底色：{span:?}");
            }
        }
    }

    #[test]
    fn running_tool_output_beyond_cap_gets_windowed_with_hidden_count() {
        let theme = test_theme();
        let output = (0..30)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let block = Block::Tool {
            call_id: CallId::from("call_1"),
            name: "bash".to_owned(),
            output,
            status: ToolStatus::Running,
        };
        let lines = render_block(&block, 80, &theme);
        // 1 条折叠提示 + 上限 LIVE_BLOCK_TAIL_CAP 行。
        assert_eq!(lines.len(), LIVE_BLOCK_TAIL_CAP + 1);
        let text = text_of(&lines);
        assert!(text.contains("已折叠"));
        assert!(text.contains("line 29"), "尾部应保留最新内容: {text}");
        assert!(!text.contains("line 0 "), "开头的旧内容应被折叠掉: {text}");
    }

    /// 已定稿的工具块不做尾窗折叠：它不在 live region 里，撑高也不会卡住渲染。
    #[test]
    fn finished_tool_output_is_never_windowed() {
        let theme = test_theme();
        let output = (0..30)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let block = Block::Tool {
            call_id: CallId::from("call_1"),
            name: "bash".to_owned(),
            output,
            status: ToolStatus::Done,
        };
        let lines = render_block(&block, 80, &theme);
        let text = text_of(&lines);
        for i in 0..30 {
            assert!(text.contains(&format!("line {i}")), "第 {i} 行不该丢");
        }
    }

    /// 工具卡片：三态各有自己的图标、边框、底色，且每行必须恰好占满宽度。
    #[test]
    fn tool_card_is_framed_and_state_colored() {
        let theme = test_theme();
        for (status, glyph, bg) in [
            (
                ToolStatus::Running,
                theme.symbols.status.running,
                theme.bg.tool_pending,
            ),
            (
                ToolStatus::Done,
                theme.symbols.status.success,
                theme.bg.tool_success,
            ),
            (
                ToolStatus::Failed,
                theme.symbols.status.error,
                theme.bg.tool_error,
            ),
        ] {
            let block = Block::Tool {
                call_id: CallId::from("call_1"),
                name: "bash".to_owned(),
                output: "line one\nline two".to_owned(),
                status,
            };
            let lines = render_block(&block, 40, &theme);
            let text = text_of(&lines);
            assert!(text.contains(glyph), "{status:?} 少了状态图标：{text}");
            assert!(text.contains("bash"), "{status:?} 少了工具名：{text}");
            assert!(text.contains("line one") && text.contains("line two"));
            assert!(
                text.contains(theme.symbols.box_round.top_left),
                "{status:?} 少了圆角边框：{text}"
            );
            for line in &lines {
                assert_eq!(line_width(line), 40, "{status:?} 卡片行宽不齐：{line:?}");
                for span in &line.spans {
                    assert_eq!(span.style.bg, Some(bg), "{status:?} 底色不对：{span:?}");
                }
            }
        }
    }

    #[test]
    fn assistant_block_hides_tool_call_content() {
        let theme = test_theme();
        let block = Block::Assistant {
            content: vec![
                AssistantContent::Text {
                    text: "好的，我先看看代码。".to_owned(),
                },
                AssistantContent::ToolCall {
                    id: CallId::from("call_1"),
                    name: "read".to_owned(),
                    arguments: "{}".to_owned(),
                },
            ],
            streaming: false,
            revealed: None,
        };
        let text = text_of(&render_block(&block, 80, &theme));
        assert!(text.contains("好的，我先看看代码。"));
        assert!(!text.contains("read"));
    }

    #[test]
    fn empty_streaming_assistant_still_renders_one_row() {
        let theme = test_theme();
        let block = Block::Assistant {
            content: vec![],
            streaming: true,
            revealed: None,
        };
        assert_eq!(render_block(&block, 80, &theme).len(), 1);
    }

    #[test]
    fn streaming_assistant_truncates_to_revealed_char_count() {
        let theme = test_theme();
        let block = Block::Assistant {
            content: vec![AssistantContent::Text {
                text: "hello world".to_owned(),
            }],
            streaming: true,
            revealed: Some(5),
        };
        assert!(text_of(&render_block(&block, 80, &theme)).contains("hello"));
        assert!(!text_of(&render_block(&block, 80, &theme)).contains("world"));
    }

    /// 节奏游标的字符数与实际被截断的文本必须落在同一个坐标系。
    ///
    /// 两边一旦漂移，`RevealPacer` 就会按偏大或偏小的 backlog 推进游标：偏大则
    /// 一次吐完（失去逐字效果），偏小则永远追不上（结尾几个字卡住不出来）。
    /// 这条契约没有别的地方能强制，只能钉在这里。
    #[test]
    fn reveal_budget_matches_rendered_characters() {
        let content = vec![
            AssistantContent::Text {
                text: "abc".to_owned(),
            },
            AssistantContent::Thinking {
                text: "def".to_owned(),
                signature: None,
            },
            AssistantContent::ToolCall {
                id: CallId::from("c1"),
                name: "read".to_owned(),
                arguments: "{}".to_owned(),
            },
        ];
        let total = assistant_char_len(&content);
        assert_eq!(total, 6, "工具调用不计入，思考按裸文本计入");

        // 把预算推到总数时，两个段落都完整出现；少一个字符就必须少一个字符。
        let full = assistant_segments(&content, Some(total));
        assert_eq!(
            full,
            vec![
                Segment::Text("abc".to_owned()),
                Segment::Thinking("def".to_owned())
            ]
        );
        assert_eq!(
            assistant_segments(&content, Some(total - 1)),
            vec![
                Segment::Text("abc".to_owned()),
                Segment::Thinking("de".to_owned())
            ]
        );
        assert_eq!(assistant_segments(&content, Some(0)), Vec::new());
    }

    /// 已加密的思考只能整块显示，且它的占位文本必须计进节奏预算——否则游标会
    /// 因为「渲染出的字符比预算多」而永远追不上。
    #[test]
    fn redacted_thinking_is_counted_as_its_placeholder() {
        let content = vec![AssistantContent::RedactedThinking {
            data: "opaque".to_owned(),
        }];
        assert_eq!(
            assistant_char_len(&content),
            REDACTED_THINKING.chars().count()
        );
        assert_eq!(
            assistant_segments(&content, None),
            vec![Segment::Thinking(REDACTED_THINKING.to_owned())]
        );
    }

    #[test]
    fn running_blocks_report_live_boundary_zero() {
        let theme = test_theme();
        let entry = TranscriptEntry {
            id: ComponentId(99),
            revision: 1,
            block: Block::Tool {
                call_id: CallId::from("call_1"),
                name: "bash".to_owned(),
                output: String::new(),
                status: ToolStatus::Running,
            },
        };
        assert_eq!(BlockComponent::new(&entry, &theme).live_boundary(), Some(0));
    }

    #[test]
    fn finished_blocks_report_no_live_boundary() {
        let theme = test_theme();
        let entry = TranscriptEntry {
            id: ComponentId(99),
            revision: 1,
            block: Block::User {
                content: vec![UserContent::Text {
                    text: "hi".to_owned(),
                }],
            },
        };
        assert_eq!(BlockComponent::new(&entry, &theme).live_boundary(), None);
    }

    #[test]
    fn entry_kind_title_change_produces_no_block() {
        let entry = Entry {
            id: EntryId::from("e1"),
            parent_id: None,
            timestamp_ms: 0,
            kind: EntryKind::TitleChange {
                title: "新标题".to_owned(),
            },
        };
        assert!(entry_to_block(&entry, false).is_none());
    }

    /// `display_role: System` 的 user 消息必须渲染成系统行，**不能**当成用户气泡。
    ///
    /// 真机回归：会话开头那段 `<system-reminder>` 环境上下文（cwd / 日期 / git status）
    /// 曾经带着用户装饰出现在 transcript 里，看起来像用户自己打了十行 git status。
    /// 契约见 `crates/agent/src/session/message.rs:212-217`。
    #[test]
    fn system_display_role_renders_as_a_system_line_not_user_input() {
        let theme = test_theme();
        let message = Message::User {
            content: vec![UserContent::Text {
                text: "# Session Context\n\nDate: 2026-08-06".to_owned(),
            }],
            display_role: Some(DisplayRole::System),
        };
        let block = message_to_block(&message, false);
        assert!(
            matches!(block, Block::System { .. }),
            "带 System 展示角色的 user 消息必须变成系统行，实得 {block:?}"
        );

        let lines = render_block(&block, 60, &theme);
        let text = text_of(&lines);
        assert!(
            text.contains(theme.symbols.status.info),
            "必须带系统行图标：{text}"
        );
        for line in &lines {
            for span in &line.spans {
                assert_ne!(
                    span.style.bg,
                    Some(theme.bg.user_message),
                    "系统行绝不能铺用户气泡底色：{span:?}"
                );
            }
        }
    }

    /// 没有展示角色的普通 user 消息仍然是用户块——上一条修复不得误伤真实用户输入。
    #[test]
    fn plain_user_message_stays_a_user_block() {
        let message = Message::User {
            content: vec![UserContent::Text {
                text: "帮我改一下".to_owned(),
            }],
            display_role: None,
        };
        assert!(matches!(
            message_to_block(&message, false),
            Block::User { .. }
        ));
    }

    /// `show_thinking == false` 时思考内容不进 transcript。
    ///
    /// 真机回归：headless 一直遵守 `config.ui.show_thinking`（默认关），TUI 却无条件
    /// 把思考画出来，同一个配置项两个客户端行为不一致。
    #[test]
    fn thinking_is_dropped_when_disabled_and_kept_when_enabled() {
        let theme = test_theme();
        let message = Message::Assistant {
            content: vec![
                AssistantContent::Thinking {
                    text: "先看看目录".to_owned(),
                    signature: None,
                },
                AssistantContent::Text {
                    text: "当前目录下有 a.txt".to_owned(),
                },
            ],
            model: None,
            usage: zcode_protocol::wire::types::Usage::default(),
            stop_reason: zcode_protocol::wire::types::StopReason::Stop,
        };

        let hidden = text_of(&render_block(
            &message_to_block(&message, false),
            60,
            &theme,
        ));
        assert!(
            !hidden.contains("先看看目录"),
            "关掉时不得出现思考：{hidden}"
        );
        assert!(hidden.contains("a.txt"), "正文必须保留：{hidden}");

        let shown = text_of(&render_block(&message_to_block(&message, true), 60, &theme));
        assert!(shown.contains("先看看目录"), "打开时必须出现思考：{shown}");
    }

    /// 思考段用斜体 + `thinkingText` 色，与正文区分；正文不带斜体。
    #[test]
    fn thinking_is_italic_and_dimmer_than_body() {
        let theme = test_theme();
        let block = Block::Assistant {
            content: vec![
                AssistantContent::Thinking {
                    text: "思路".to_owned(),
                    signature: None,
                },
                AssistantContent::Text {
                    text: "结论".to_owned(),
                },
            ],
            streaming: false,
            revealed: None,
        };
        let lines = render_block(&block, 40, &theme);
        let italic_text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .filter(|s| s.style.add_modifier.contains(Modifier::ITALIC))
            .map(|s| s.content.as_ref())
            .collect();
        assert!(italic_text.contains("思路"), "思考必须是斜体：{lines:?}");
        assert!(!italic_text.contains("结论"), "正文不该是斜体：{lines:?}");
    }
}
