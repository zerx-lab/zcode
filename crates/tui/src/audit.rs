//! committed-prefix 审计：比对账本记录的"已提交行的精确字节"（tape）与本帧
//! compose 结果，判断 exact 区是否被重排。
//!
//! 原始出处 `oh-my-pi/docs/tui-core-renderer.md:41-58, 95-120`，实现是
//! `oh-my-pi/packages/tui/src/tui.ts:3264-3276`（`#auditCommittedPrefix`）与
//! `:862-911`（`findCommittedPrefixResync`）。设计事实源
//! `plans/tui/architecture.md` 第 2 节（`:13-90`）与第 5 节（`:197-216`）。
//!
//! # 只审计 exact 区
//!
//! 审计范围是 `[0, exact_end)`，`exact_end = min(C, B)` 由
//! [`crate::ledger::WindowPlan::exact_end`] 给出。`[B, C)` 的 frozen 区不在这个
//! 函数的视野内——它是"滚出去那一刻的快照"，其内容此后必然与仍在变化的组件源
//! 不符，这是设计如此，不是 bug（见 `ledger` 模块文档"三区语义"一节）。
//!
//! # 为什么按值比较 `Line` 是对的
//!
//! `committed` 是账本认定"已经进入终端 scrollback、不可再改写"的那份内容；
//! `frame` 是本帧 compose 的结果。两者在 exact 区理应逐行相等——这段范围内的
//! 组件已经把内容声明为 FINAL。ratatui 0.30 的 [`Line`] 派生了 `PartialEq`
//! （比较 `style` 与全部 `spans`，`Span` 同样按 `style` + 内容比较），这里直接
//! 用它做逐行比较是正确的：**样式变化同样会改变已提交行的视觉**（例如主题切换、
//! 或者一个"已完成"状态被错误地又改了颜色），跟内容文本变化一样都属于对
//! "已提交"承诺的违反，必须同样触发 [`AuditOutcome::ReAnchor`]，不能只比字符串
//! 而放过纯样式的分歧。
//!
//! 简化说明：oh-my-pi 的 `findCommittedPrefixResync` 在 verified 区之上还叠加了
//! 一段"尾部采样容忍单处失配"的逻辑（`tui.ts:848-855`），用来吸收"仍在动画的行
//! 已经进入历史"这种情况。这里不需要那层容忍：三区划分已经把"仍可能变"的行
//! 隔到了 frozen 区（`[B, C)`），进入本函数视野的 exact 区 `[0, min(C,B))` 定义上
//! 就是组件声明为 FINAL、不会再变的行，逐字节精确比较才是契约本身，不是近似。
//!
//! # 绝不在这里发 ED3
//!
//! `oh-my-pi` 的 divergence rebuild（committed-prefix 结构性 resync 时清屏重放）
//! 默认关闭（`tui.scrollbackRebuild = false`，见 `architecture.md:197-216`）：
//! 流式渲染中的内容漂移不修历史，宁可在 scrollback 里留一行 stale，也不要为了
//! "修正"一处漂移去做一次全量重放。本函数只负责判断"是否需要 re-anchor"以及
//! "从哪一行开始"，具体的 re-anchor 执行是调用方对
//! [`crate::ledger::Ledger::re_anchor`] 的调用；ED3 的唯一 callsite 是
//! `emit::Emitter::full_paint`，且只由用户手势触发，本模块不参与那条路径。

use ratatui::text::Line;

/// committed-prefix 审计结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditOutcome {
    /// exact 区与已提交内容完全一致（或 exact 区为空，无需比对）。
    Clean,
    /// exact 区第 `row` 行与已提交内容不符。调用方应
    /// `Ledger::re_anchor(row)`，让 `[row, ..)` 在下一帧重新提交。
    ReAnchor {
        /// 第一处发现分歧的 frame 行下标。
        row: usize,
    },
}

/// 审计已提交的 exact 前缀 `[0, exact_end)`。
///
/// - `committed`：账本记录的"已提交行的精确字节"（tape）。
/// - `frame`：本帧 compose 结果。
/// - `exact_end`：exact 区末尾，取 [`crate::ledger::WindowPlan::exact_end`]。
///
/// `committed` 比 `exact_end` 短（提交记录缺失）或 `frame` 比 `exact_end` 短
/// （帧塌缩进已提交行）都算 divergence，返回 `ReAnchor { row }`，`row` 取两者
/// 中较短的那个长度（先出现的逐行比较分歧优先于长度分歧）。
#[must_use]
pub fn audit_committed_prefix(
    committed: &[Line<'static>],
    frame: &[Line<'static>],
    exact_end: usize,
) -> AuditOutcome {
    if exact_end == 0 {
        return AuditOutcome::Clean;
    }
    // 三者中最短的长度：既不越界读 `committed`/`frame`，又能在循环走完后
    // 用它和 `exact_end` 的差值判断"长度本身就是分歧"（对应 tape 记录缺失
    // 或本帧塌缩进已提交区两种情况）。
    let limit = exact_end.min(committed.len()).min(frame.len());
    for (row, (c, f)) in committed.iter().zip(frame.iter()).enumerate().take(limit) {
        if c != f {
            return AuditOutcome::ReAnchor { row };
        }
    }
    if limit < exact_end {
        return AuditOutcome::ReAnchor { row: limit };
    }
    AuditOutcome::Clean
}

#[cfg(test)]
mod tests {
    use super::{AuditOutcome, audit_committed_prefix};
    use ratatui::style::{Color, Style};
    use ratatui::text::Line;

    fn plain_lines(texts: &[&str]) -> Vec<Line<'static>> {
        texts.iter().map(|t| Line::from((*t).to_owned())).collect()
    }

    /// `exact_end == 0` 恒 `Clean`：首帧（`C == 0`），或 `B == 0`（全 live）。
    #[test]
    fn zero_exact_end_is_always_clean() {
        let committed = plain_lines(&["a", "b"]);
        let frame = plain_lines(&["x", "y"]);
        assert_eq!(
            audit_committed_prefix(&committed, &frame, 0),
            AuditOutcome::Clean
        );
    }

    /// 完全相同 → `Clean`。
    #[test]
    fn identical_prefix_is_clean() {
        let committed = plain_lines(&["a", "b", "c"]);
        let frame = plain_lines(&["a", "b", "c", "d"]);
        assert_eq!(
            audit_committed_prefix(&committed, &frame, 3),
            AuditOutcome::Clean
        );
    }

    /// 第 k 行内容不同 → `ReAnchor { row: k }`。
    #[test]
    fn content_mismatch_reanchors_at_first_divergent_row() {
        let committed = plain_lines(&["a", "b", "c"]);
        let frame = plain_lines(&["a", "X", "c"]);
        assert_eq!(
            audit_committed_prefix(&committed, &frame, 3),
            AuditOutcome::ReAnchor { row: 1 }
        );
    }

    /// 第 k 行内容相同但 `Style` 不同 → 同样 `ReAnchor { row: k }`：
    /// 样式变化同样破坏了"已提交"的视觉承诺。
    #[test]
    fn style_only_mismatch_reanchors() {
        let committed = plain_lines(&["a", "b", "c"]);
        let mut frame = plain_lines(&["a", "b", "c"]);
        if let Some(line) = frame.get_mut(1) {
            *line = Line::styled("b", Style::default().fg(Color::Red));
        }
        assert_eq!(
            audit_committed_prefix(&committed, &frame, 3),
            AuditOutcome::ReAnchor { row: 1 }
        );
    }

    /// frozen 区（下标 `>= exact_end`）不同 → 仍 `Clean`（豁免生效）。
    #[test]
    fn frozen_zone_divergence_is_exempt() {
        let committed = plain_lines(&["a", "b", "STALE"]);
        let frame = plain_lines(&["a", "b", "FRESH"]);
        // exact_end == 2：第 2 行（下标 2）属于 frozen 区，不参与比对。
        assert_eq!(
            audit_committed_prefix(&committed, &frame, 2),
            AuditOutcome::Clean
        );
    }

    /// `frame` 比 `exact_end` 短（帧塌缩进已提交行）→ `ReAnchor { row: frame.len() }`。
    #[test]
    fn frame_shorter_than_exact_end_reanchors_at_frame_length() {
        let committed = plain_lines(&["a", "b", "c"]);
        let frame = plain_lines(&["a", "b"]);
        assert_eq!(
            audit_committed_prefix(&committed, &frame, 3),
            AuditOutcome::ReAnchor { row: 2 }
        );
    }

    /// `committed` 比 `exact_end` 短（提交记录缺失）→ `ReAnchor { row: committed.len() }`。
    #[test]
    fn committed_shorter_than_exact_end_reanchors_at_committed_length() {
        let committed = plain_lines(&["a"]);
        let frame = plain_lines(&["a", "b", "c"]);
        assert_eq!(
            audit_committed_prefix(&committed, &frame, 3),
            AuditOutcome::ReAnchor { row: 1 }
        );
    }
}
