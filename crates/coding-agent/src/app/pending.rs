//! 待回答队列：审批弹窗与 stdin 询问。
//!
//! 两者共享同一套"挂在 session 上、reconnect 后必须能重建"的语义（协议文档
//! `crates/protocol/src/wire/request.rs:268-280`）。本模块只管**本地展示状态**：
//! 真正的"重连后拉取"由 [`crate::app::state::AppState`] 在收到
//! `Reply::Subscribed`/`Reply::Pending` 时调用 [`PendingState::seed`] 完成
//! （对应任务要求的第 6 条：opencode 漏掉的 `permission.list` 重拉步骤，
//! 本仓通过 `Request::PendingList` 补上，落点在 `state.rs`）。
//!
//! 一次只展示队首一项（先到先展示），其余在标题里显示"还有 N 条待处理"，
//! 避免同时弹出多个弹窗抢输入焦点。

use std::collections::VecDeque;

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use zcode_protocol::wire::types::{ApprovalId, PendingApproval, PendingStdin, StdinId};

use crate::app::ids::PENDING_COMPONENT;
use zcode_text::width::{display_col_to_byte, visible_width};
use zcode_tui::card::{
    Card, Section, StatusLine, card_content_width, render_card, render_status_line,
};
use zcode_tui::theme::Theme;
use zcode_tui::wrap::wrap_line;
use zcode_tui::{Component, ComponentId};

/// 取 `text` 末尾至多 `width` 显示列的内容；不足则原样返回。
///
/// 切点经 [`display_col_to_byte`] 定位，永远落在合法 UTF-8 与 grapheme 边界上，
/// 不会把宽字符切半。
fn tail_by_width(text: &str, width: usize) -> String {
    let total = visible_width(text);
    if total <= width {
        return text.to_owned();
    }
    let start = display_col_to_byte(text, total.saturating_sub(width));
    text.get(start..).unwrap_or(text).to_owned()
}

/// 队首待回答项的引用视图，供渲染与按键处理共用。
#[derive(Debug, Clone, Copy)]
pub(crate) enum Front<'a> {
    /// 一条待审批。
    Approval(&'a PendingApproval),
    /// 一条待 stdin 输入。
    Stdin(&'a PendingStdin),
}

/// 待审批 / 待 stdin 队列。
#[derive(Debug, Default)]
pub(crate) struct PendingState {
    approvals: VecDeque<PendingApproval>,
    stdin: VecDeque<PendingStdin>,
    /// 当前正在编辑的 stdin 回复文本（仅在队首是一条 stdin 询问时有意义）。
    stdin_input: String,
    revision: u64,
}

impl PendingState {
    /// 用一次 `Pending` 快照（来自 `Reply::Subscribed`/`Reply::Pending`）整体替换
    /// 本地状态。**必须**在每次 `Subscribe`/`PendingList` 回应后调用，这是
    /// "重连后能重建弹窗"的唯一入口。
    pub(crate) fn seed(&mut self, approvals: Vec<PendingApproval>, stdin: Vec<PendingStdin>) {
        self.approvals = approvals.into();
        self.stdin = stdin.into();
        self.stdin_input.clear();
        self.revision = self.revision.saturating_add(1);
    }

    /// `Event::ApprovalRequested` 到达：追加一条。
    pub(crate) fn push_approval(&mut self, item: PendingApproval) {
        self.approvals.push_back(item);
        self.revision = self.revision.saturating_add(1);
    }

    /// `Event::ApprovalResolved` 到达：按 id 移除（可能是别的客户端处理的）。
    pub(crate) fn remove_approval(&mut self, id: &ApprovalId) {
        let before = self.approvals.len();
        self.approvals.retain(|p| &p.request_id != id);
        if self.approvals.len() != before {
            self.revision = self.revision.saturating_add(1);
        }
    }

    /// `Event::StdinRequested` 到达：追加一条。
    pub(crate) fn push_stdin(&mut self, item: PendingStdin) {
        self.stdin.push_back(item);
        self.revision = self.revision.saturating_add(1);
    }

    /// `Event::StdinResolved` 到达：按 id 移除。
    pub(crate) fn remove_stdin(&mut self, id: &StdinId) {
        let before = self.stdin.len();
        self.stdin.retain(|p| &p.request_id != id);
        if self.stdin.len() != before {
            self.stdin_input.clear();
            self.revision = self.revision.saturating_add(1);
        }
    }

    /// 是否没有任何待回答项。
    #[must_use]
    pub(crate) fn is_empty(&self) -> bool {
        self.approvals.is_empty() && self.stdin.is_empty()
    }

    /// 队首项：审批优先于 stdin（审批通常阻塞的是更危险的操作）。
    #[must_use]
    pub(crate) fn front(&self) -> Option<Front<'_>> {
        if let Some(item) = self.approvals.front() {
            return Some(Front::Approval(item));
        }
        self.stdin.front().map(Front::Stdin)
    }

    /// 除队首外还排着多少条。
    #[must_use]
    pub(crate) fn queued_behind_front(&self) -> usize {
        let total = self.approvals.len().saturating_add(self.stdin.len());
        total.saturating_sub(1)
    }

    /// 当前 stdin 回复缓冲区（只在队首是 stdin 询问时有意义）。
    #[must_use]
    pub(crate) fn stdin_input(&self) -> &str {
        &self.stdin_input
    }

    /// 向 stdin 回复缓冲区追加一个字符。
    pub(crate) fn stdin_push_char(&mut self, c: char) {
        self.stdin_input.push(c);
        self.revision = self.revision.saturating_add(1);
    }

    /// 从 stdin 回复缓冲区退格。
    pub(crate) fn stdin_backspace(&mut self) {
        if self.stdin_input.pop().is_some() {
            self.revision = self.revision.saturating_add(1);
        }
    }
}

/// 纯函数：队首项 → 卡片。不访问任何状态之外的输入。
///
/// 用卡片而非裸文本行：审批/stdin 是**必须被看见并回应**的阻塞项，边框把它从
/// 连续的 transcript 里切出来。边框色区分语义——审批是要人做决定的，用 `warning`；
/// stdin 只是在等输入，用 `borderAccent`。
#[must_use]
pub(crate) fn render_front(
    front: Front<'_>,
    stdin_input: &str,
    queued_behind: usize,
    width: u16,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let content_width = usize::from(card_content_width(width, 1, 1));
    let mut meta: Vec<String> = Vec::new();
    if queued_behind > 0 {
        meta.push(format!("还有 {queued_behind} 条待处理"));
    }
    let meta_refs: Vec<&str> = meta.iter().map(String::as_str).collect();
    let mut body: Vec<Line<'static>> = Vec::new();

    let (icon, icon_color, title, description, border) = match front {
        Front::Approval(approval) => (
            theme.symbols.status.warning,
            theme.colors.warning,
            "审批请求",
            Some(approval.tool_name.as_str()),
            theme.colors.warning,
        ),
        Front::Stdin(stdin) => (
            theme.symbols.status.pending,
            theme.colors.border_accent,
            if stdin.is_password {
                "密码输入"
            } else {
                "等待输入"
            },
            None,
            theme.colors.border_accent,
        ),
    };

    match front {
        Front::Approval(approval) => {
            for raw in approval.prompt.split('\n') {
                body.extend(wrap_line(
                    &Line::from(Span::styled(raw.to_owned(), theme.text())),
                    content_width,
                ));
            }
            body.push(Line::default());
            // 按键提示必须**换行**而不是被硬截断：卡片的内容行兜底走
            // `truncate_line(.., "")`（空省略号，纯硬切），窄终端下会显示成
            // 「[y] 允许一次  [a] 本会话内总」——用户看不到 `[n] 拒绝`，而且没有任何
            // 迹象表明后面还有字。这是**阻塞性询问**，看不全等于答不了。
            body.extend(wrap_line(
                &Line::from(Span::styled(
                    "[y] 允许一次  [a] 本会话内总是允许  [n] 拒绝".to_owned(),
                    theme.dim(),
                )),
                content_width,
            ));
        }
        Front::Stdin(stdin) => {
            if !stdin.prompt.is_empty() {
                for raw in stdin.prompt.split('\n') {
                    body.extend(wrap_line(
                        &Line::from(Span::styled(raw.to_owned(), theme.text())),
                        content_width,
                    ));
                }
            }
            // 密码不回显原文，只按字符数出等长掩码——掩码字符与 markdown 的项目
            // 符号无关，用固定的 `*` 而不是主题符号，避免换档后长度跟着变。
            let shown = if stdin.is_password {
                "*".repeat(stdin_input.chars().count())
            } else {
                stdin_input.to_owned()
            };
            // 回显做**尾部跟随**而不是换行：输入是单行语义，让它随字数往下堆行会把
            // 卡片越撑越高；只保留末尾能放下的部分，用户始终看得见自己刚敲的字。
            let prompt_glyph = format!("{} ", theme.symbols.nav.cursor);
            let room = content_width.saturating_sub(visible_width(&prompt_glyph));
            let tail = tail_by_width(&shown, room);
            body.push(Line::from(vec![
                Span::styled(prompt_glyph, Style::default().fg(theme.colors.accent)),
                Span::styled(tail, theme.text()),
            ]));
        }
    }

    let header = render_status_line(
        &StatusLine {
            icon: Some(Span::styled(
                icon.to_owned(),
                Style::default().fg(icon_color),
            )),
            title,
            title_style: theme.title(),
            description,
            badge: None,
            meta: &meta_refs,
        },
        theme,
    );
    let sections = [Section {
        label: None,
        lines: &body,
    }];
    render_card(
        &Card {
            header: Some(header),
            sections: &sections,
            border: Style::default().fg(border),
            bg: Some(theme.bg.custom_message),
            padding_left: 1,
            padding_right: 1,
        },
        width,
        theme,
    )
}

/// 待办弹窗组件：固定 id，`revision` 跟随 [`PendingState`]。
#[derive(Debug)]
pub(crate) struct PendingComponent<'a> {
    state: &'a PendingState,
    theme: &'a Theme,
}

impl<'a> PendingComponent<'a> {
    /// 包一层 `&PendingState`，为空队列返回 `None`——空弹窗不该占用一行空白。
    pub(crate) fn new(state: &'a PendingState, theme: &'a Theme) -> Option<Self> {
        (!state.is_empty()).then_some(Self { state, theme })
    }
}

impl Component for PendingComponent<'_> {
    fn id(&self) -> ComponentId {
        PENDING_COMPONENT
    }

    fn revision(&self) -> u64 {
        self.state.revision
    }

    fn render(&self, width: u16) -> Vec<Line<'static>> {
        self.state
            .front()
            .map(|front| {
                render_front(
                    front,
                    self.state.stdin_input(),
                    self.state.queued_behind_front(),
                    width,
                    self.theme,
                )
            })
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approval(id: &str) -> PendingApproval {
        PendingApproval {
            request_id: ApprovalId::from(id),
            call_id: zcode_protocol::wire::types::CallId::from("call"),
            tool_name: "bash".to_owned(),
            scope: "bash".to_owned(),
            prompt: "运行 `rm -rf /tmp/x`".to_owned(),
        }
    }

    #[test]
    fn seed_replaces_state_and_clears_stdin_input() {
        let mut pending = PendingState::default();
        pending.stdin_push_char('x');
        pending.seed(vec![approval("a1")], vec![]);
        assert_eq!(pending.stdin_input(), "");
        assert!(matches!(pending.front(), Some(Front::Approval(_))));
    }

    #[test]
    fn approvals_take_priority_over_stdin() {
        let mut pending = PendingState::default();
        pending.push_stdin(PendingStdin {
            request_id: StdinId::from("s1"),
            call_id: zcode_protocol::wire::types::CallId::from("call"),
            prompt: String::new(),
            is_password: false,
        });
        pending.push_approval(approval("a1"));
        assert!(matches!(pending.front(), Some(Front::Approval(_))));
        assert_eq!(pending.queued_behind_front(), 1);
    }

    #[test]
    fn remove_by_id_drops_only_matching_entry() {
        let mut pending = PendingState::default();
        pending.push_approval(approval("a1"));
        pending.push_approval(approval("a2"));
        pending.remove_approval(&ApprovalId::from("a1"));
        assert!(matches!(
            pending.front(),
            Some(Front::Approval(p)) if p.request_id.as_str() == "a2"
        ));
    }

    #[test]
    fn resolving_unknown_id_is_a_noop() {
        let mut pending = PendingState::default();
        pending.push_approval(approval("a1"));
        let before = pending.revision;
        pending.remove_approval(&ApprovalId::from("does-not-exist"));
        assert_eq!(pending.revision, before);
    }

    #[test]
    fn password_stdin_masks_render_output() {
        let theme = crate::app::test_theme();
        let stdin = PendingStdin {
            request_id: StdinId::from("s1"),
            call_id: zcode_protocol::wire::types::CallId::from("call"),
            prompt: "密码:".to_owned(),
            is_password: true,
        };
        let lines = render_front(Front::Stdin(&stdin), "secret", 0, 40, &theme);
        let text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect();
        assert!(!text.contains("secret"));
        assert!(text.contains("******"));
    }

    /// 审批弹窗必须是**带边框的卡片**：它是阻塞项，要从连续的 transcript 里被切出来。
    /// 每行铺满整宽，否则右边框参差。
    #[test]
    fn approval_renders_as_a_framed_card() {
        let theme = crate::app::test_theme();
        let approval = PendingApproval {
            request_id: ApprovalId::from("a1"),
            call_id: zcode_protocol::wire::types::CallId::from("call"),
            tool_name: "bash".to_owned(),
            scope: "bash".to_owned(),
            prompt: "要执行 rm -rf /tmp/x 吗？".to_owned(),
        };
        let lines = render_front(Front::Approval(&approval), "", 2, 50, &theme);
        let text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect();
        assert!(
            text.contains(theme.symbols.box_round.top_left),
            "缺边框：{text}"
        );
        assert!(text.contains("审批请求"), "缺标题：{text}");
        assert!(text.contains("bash"), "缺工具名：{text}");
        assert!(text.contains("还有 2 条待处理"), "缺队列提示：{text}");
        assert!(text.contains("[y] 允许一次"), "缺按键提示：{text}");
        for line in &lines {
            assert_eq!(
                zcode_tui::wrap::line_width(line),
                50,
                "卡片每行必须铺满整宽：{line:?}"
            );
        }
    }
}
