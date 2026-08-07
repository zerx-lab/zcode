//! 状态行：spinner + 处理中提示。永远上报 `live_boundary() == Some(0)`——
//! 它是 transcript 里"活跃尾部"的默认锚点：只要它出现在组件列表里，
//! 输入框、待办弹窗就自动被保护在 pinned live region 内，不会被误提交进
//! scrollback（`compose::ComposeOutcome::boundary` 的"topmost 组件优先"语义，
//! 见 `crates/tui/src/compose.rs:260-262` 与 `plans/tui/architecture.md:23`）。
//!
//! spinner 帧序列与符号档位绑定（`theme.symbols.spinner.status`），**不在这里
//! 硬编码**：unicode 档 8 帧、nerd 档 12 帧、ascii 档 4 帧，写死任一个数都会在
//! 换档后越界或只用到前几帧。帧推进节奏仍由 [`crate::app::redraw::SPINNER_INTERVAL`]
//! 决定——上游同样把「帧集」和「节奏」分开放
//! （oh-my-pi `packages/coding-agent/src/modes/theme/theme.ts:986-999` 存帧，
//! `packages/tui/src/components/loader.ts:6` 定 80 ms）。

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::app::ids::STATUS_COMPONENT;
use zcode_tui::theme::{SpinnerKind, Theme};
use zcode_tui::{Component, ComponentId};

/// 状态行此刻展示的内容。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StatusKind {
    /// 尚未完成 `Subscribe` 握手：第一帧在连接完成之前就已经画出
    /// （`plans/runtime-boundary/implementation.md:86-87`），这是那段等待期间
    /// 展示的状态。
    Connecting,
    /// 空闲：没有进行中的 turn。
    Idle,
    /// 正在等待模型响应或流式输出中。
    Thinking,
    /// 一个工具正在执行。
    RunningTool {
        /// 工具名，用于展示。
        name: String,
    },
    /// 上一轮以错误结束，展示错误提示直到下一次交互。
    Error {
        /// 面向用户的错误文本。
        message: String,
    },
    /// Ctrl-C 已按下一次，等待第二次确认退出。
    ConfirmQuit,
}

/// 纯函数：状态 → 一行文本。不访问时钟——spinner 帧由调用方传入的 `tick` 决定。
///
/// 提示文案里的按键说明用 `dim`（一个**颜色**，不是 `Modifier::DIM`），与
/// `theme` 模块文档解释的取舍一致。
#[must_use]
pub(crate) fn render_status_line(kind: &StatusKind, tick: u64, theme: &Theme) -> Line<'static> {
    let spinner = |label: String| {
        Line::from(vec![
            Span::styled(
                format!("{} ", theme.spinner_frame(SpinnerKind::Status, tick)),
                Style::new().fg(theme.colors.accent),
            ),
            Span::styled(label, theme.muted()),
        ])
    };
    match kind {
        StatusKind::Idle => Line::from(Span::styled(
            format!(
                "就绪{sep}Enter 发送{sep}Esc 取消/清空{sep}Ctrl-C 两次退出",
                sep = theme.symbols.sep.dot
            ),
            theme.dim(),
        )),
        StatusKind::Connecting => spinner("正在连接…".to_owned()),
        StatusKind::Thinking => spinner("思考中…".to_owned()),
        StatusKind::RunningTool { name } => spinner(format!("正在执行 {name}…")),
        StatusKind::Error { message } => Line::from(vec![
            Span::styled(
                format!("{} ", theme.symbols.status.error),
                Style::new().fg(theme.colors.error),
            ),
            Span::styled(message.clone(), Style::new().fg(theme.colors.error)),
        ]),
        StatusKind::ConfirmQuit => Line::from(vec![
            Span::styled(
                format!("{} ", theme.symbols.status.warning),
                Style::new().fg(theme.colors.warning),
            ),
            Span::styled(
                "再按一次 Ctrl-C 退出".to_owned(),
                Style::new().fg(theme.colors.warning),
            ),
        ]),
    }
}

/// 是否处于"正在动画"状态（决定 [`crate::app::redraw::next_tick_interval`] 走
/// spinner 节奏还是 idle 节奏）。
#[must_use]
pub(crate) fn is_animating(kind: &StatusKind) -> bool {
    matches!(
        kind,
        StatusKind::Connecting | StatusKind::Thinking | StatusKind::RunningTool { .. }
    )
}

/// 状态行组件：固定 id，`revision` 由调用方传入（每次真正需要重绘时 +1）。
#[derive(Debug)]
pub(crate) struct StatusComponent<'a> {
    kind: StatusKind,
    tick: u64,
    revision: u64,
    theme: &'a Theme,
}

impl<'a> StatusComponent<'a> {
    /// 新建一次渲染快照。`revision` 通常是"这条状态自身的版本号"与"动画 tick"的
    /// 组合，调用方负责保证内容变化时 revision 也变化。
    pub(crate) fn new(kind: StatusKind, tick: u64, revision: u64, theme: &'a Theme) -> Self {
        Self {
            kind,
            tick,
            revision,
            theme,
        }
    }
}

impl Component for StatusComponent<'_> {
    fn id(&self) -> ComponentId {
        STATUS_COMPONENT
    }

    fn revision(&self) -> u64 {
        self.revision
    }

    fn render(&self, _width: u16) -> Vec<Line<'static>> {
        vec![render_status_line(&self.kind, self.tick, self.theme)]
    }

    fn live_boundary(&self) -> Option<usize> {
        // 状态行永远是活跃尾部的锚点，见模块文档。
        Some(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_theme;

    fn text_of(line: &Line<'static>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    /// 帧序列来自主题，取模覆盖整圈；帧数随符号档位变，所以按主题里的实际长度断言。
    #[test]
    fn spinner_cycles_through_all_frames_of_the_active_preset() {
        let theme = test_theme();
        let frames = theme.symbols.spinner.status;
        let seen: std::collections::HashSet<&str> = (0..frames.len() * 2)
            .map(|i| theme.spinner_frame(SpinnerKind::Status, u64::try_from(i).unwrap_or(0)))
            .collect();
        assert_eq!(seen.len(), frames.len());
    }

    /// ascii 档的帧数与 unicode 档不同：状态行取帧必须跟着档位走，不能写死。
    #[test]
    fn spinner_follows_the_symbol_preset() {
        let ascii = zcode_tui::BuiltinTheme::Dark
            .load(
                zcode_tui::ColorMode::TrueColor,
                zcode_tui::SymbolPreset::Ascii,
            )
            .expect("内置暗色主题必须能加载");
        let line = render_status_line(&StatusKind::Thinking, 0, &ascii);
        let text = text_of(&line);
        assert!(
            ascii
                .symbols
                .spinner
                .status
                .iter()
                .any(|frame| text.starts_with(frame)),
            "ascii 档必须用 ascii 帧：{text}"
        );
    }

    #[test]
    fn idle_and_error_are_not_animating() {
        assert!(!is_animating(&StatusKind::Idle));
        assert!(!is_animating(&StatusKind::Error {
            message: "x".to_owned()
        }));
        assert!(!is_animating(&StatusKind::ConfirmQuit));
    }

    #[test]
    fn thinking_and_tool_are_animating() {
        assert!(is_animating(&StatusKind::Thinking));
        assert!(is_animating(&StatusKind::RunningTool {
            name: "bash".to_owned()
        }));
    }

    /// 错误与退出确认必须用主题的 error / warning 色，不能沿用常规文本色——
    /// 它们是「必须被看见」的状态。
    #[test]
    fn error_and_confirm_use_alert_colors() {
        let theme = test_theme();
        let err = render_status_line(
            &StatusKind::Error {
                message: "炸了".to_owned(),
            },
            0,
            &theme,
        );
        assert!(
            err.spans
                .iter()
                .all(|s| s.style.fg == Some(theme.colors.error))
        );
        let quit = render_status_line(&StatusKind::ConfirmQuit, 0, &theme);
        assert!(
            quit.spans
                .iter()
                .all(|s| s.style.fg == Some(theme.colors.warning))
        );
    }

    #[test]
    fn status_component_always_reports_live_boundary() {
        let theme = test_theme();
        let component = StatusComponent::new(StatusKind::Idle, 0, 1, &theme);
        assert_eq!(component.live_boundary(), Some(0));
    }
}
