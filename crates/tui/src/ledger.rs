//! C/W/B 账本：`committed_rows`（C）、`window_top`（W）、`boundary`（B）三量，
//! 与它们之间的窗口/提交数学。
//!
//! 来源与背景见 `plans/tui/architecture.md` 第 2 节（`:13-90`），原始出处是
//! `oh-my-pi/docs/tui-core-renderer.md:41-58`，实现是
//! `oh-my-pi/packages/tui/src/tui.ts:3049-3153`（提交数学本体）与 `:3093`
//! （pinned 分支 `chunkTo = liveRegionPinned ? Math.min(windowTop, finalBoundary) : windowTop`）。
//!
//! # 三量的含义（`architecture.md:19-23`）
//!
//! - **C**（`committed_rows`）：composed frame 的 `[0, C)` 行已经写进终端原生
//!   scrollback。普通 emitter 绝不重写这一段。
//! - **W**（`window_top`）：映射到 grid row 0 的 frame 行；可见窗口是
//!   `[W, W + h)`，`h` 是 viewport 高度。
//! - **B**（`boundary`）：第一条仍可能变化的行，由组件树每帧上报（见
//!   `compose::ComposeOutcome::boundary`）。
//!
//! # C 与 B 无序关系（`architecture.md:29`）
//!
//! **C 可以超过 B。** B 之后仍可变的行一旦滚出可见窗口，就以"滚出去那一刻
//! 屏幕上的样子"作为**冻结视觉快照**提交——只有 pinned live region 才把可变
//! 尾部继续留在 viewport 内（不提交，等它自己降级为 exact）。unpinned 的可变
//! 行没有这种豁免：终端原生 scrollback 一旦滚出可见区就不可再改写，账本必须
//! 承认这个事实。这也是为什么普通（unpinned）帧的 `commit_end` 直接取到 `W`
//! 而不是卡在 `B`——如果卡在 `B`，`C` 会永远追不上已经滚出屏幕的 `W`，下一帧
//! 历史注入（`insert_history`）就会把 `[C, W)` 这段已经不在终端里的内容重复
//! 写一遍。
//!
//! # 三区语义（`architecture.md:37-43`）
//!
//! ```text
//! [0, min(C,B))   exact   —— 组件声明 FINAL，逐字节精确，每帧参与审计
//! [B, C)          frozen  —— 滚出窗口时仍是 live 的行，提交的是快照，豁免审计
//! [W, W+h)        window  —— 当前可见窗口
//! ```
//!
//! frozen 区必须豁免审计：否则一个仍在收缩/滚动的预览框（比如工具调用的流式
//! 输出）会在每一帧都触发一次 `re_anchor`——它的内容本来就还没定稿，跟"已提交
//! 内容被破坏"是两回事，不该按同一条报警路径处理。
//!
//! # `L` 恒定 ⇒ 零字节进历史（`architecture.md:66-89`）
//!
//! 只要 composed frame 长度 `L` 帧间不变，`W = max(C, L - h)` 就帧间不变，
//! `commit_end` 随之不变，于是没有新行进入历史——这正是"固定高度活跃区"
//! （sliding tail window）必须走 in-window diff 而不是 scroll-append 的原因：
//! 帧内几乎每行都在变，但账本层面完全静止。
//!
//! **违反的代价是真实事故**（`architecture.md:87`，`oh-my-pi` `CHANGELOG.md:2342`）：
//! 一个流式框如果没有把高度钉死在 viewport 内，可变尾部会滚到 `W` 之上；
//! 按三区规则，这些行滚出窗口时仍是 live 的，被当作冻结快照逐帧提交一次，
//! scrollback 里就堆出几十条重复的 banner。修复只有两条路：把框高度钉进
//! viewport（本 crate 对流式框的默认要求），或把该区域声明为 pinned live
//! region（换成可变尾部原地重绘，代价是它离开屏幕后不可回看）。
//!
//! # `re_anchor`：duplication, never loss
//!
//! `audit::audit_committed_prefix` 发现 exact 区被重排时，唯一的修复手段是把
//! `C` 退回到分歧行，让 `[row, ..)` 在下一帧重新提交。旧副本已经写进了终端
//! scrollback，退回 `C` 不会（也不能）删除它——multiplexer 下 ED3 不可用
//! （`architecture.md:208-216`），"留一份重复"永远比"内容丢失"或"擦错东西"安全。

/// 一帧的窗口与提交计划。所有行号都是 composed frame（`compose::Composer::frame`）
/// 上的行下标，不是屏幕坐标。
///
/// 数学见模块文档；来源 `oh-my-pi/packages/tui/src/tui.ts:3088-3152`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowPlan {
    /// 映射到 grid row 0 的 frame 行，即 `W`。
    pub window_top: usize,
    /// 本帧结束时 `C` 应该到达的值。
    pub commit_end: usize,
    /// 可见窗口高度（即调用 [`Ledger::plan`] 时传入的 `viewport_height`）。
    pub window_height: usize,
    /// exact 区末尾 `min(C, B)`：只有这段参与 [`crate::audit::audit_committed_prefix`] 审计。
    pub exact_end: usize,
}

/// C/W/B 三量账本。语义见模块文档与 `plans/tui/architecture.md` 第 2 节。
///
/// `boundary`/`pinned` 由 compose 的输出每帧驱动（[`Ledger::set_boundary`] /
/// [`Ledger::set_pinned`]）；`committed_rows`/`window_top` 只能通过
/// [`Ledger::plan`] + [`Ledger::apply`]（有窗口概念的路径）、[`Ledger::re_anchor`]
/// （审计重排）或 [`Ledger::commit_to`]（无窗口概念的纯文本路径，只推进 `C`）
/// 推进，绝不由调用方直接赋值。
#[derive(Debug, Clone, Default)]
pub struct Ledger {
    /// C：composed frame `[0, committed_rows)` 已进入终端历史。
    committed_rows: usize,
    /// W：映射到 grid row 0 的 frame 行。
    window_top: usize,
    /// B：第一条仍可能变化的行，由组件树每帧上报。
    boundary: usize,
    /// live region 是否 pinned：pinned 时可变尾部留在 viewport 内，不提交。
    pinned: bool,
}

impl Ledger {
    /// 新建一个全零账本（首次渲染前的初始状态）。
    #[must_use]
    pub const fn new() -> Self {
        Self {
            committed_rows: 0,
            window_top: 0,
            boundary: 0,
            pinned: false,
        }
    }

    /// C：`[0, committed_rows())` 已进入终端历史。
    #[must_use]
    pub const fn committed_rows(&self) -> usize {
        self.committed_rows
    }

    /// W：映射到 grid row 0 的 frame 行；可见窗口是 `[window_top(), window_top() + h)`。
    #[must_use]
    pub const fn window_top(&self) -> usize {
        self.window_top
    }

    /// B：第一条仍可能变化的行。
    #[must_use]
    pub const fn boundary(&self) -> usize {
        self.boundary
    }

    /// live region 是否 pinned。
    #[must_use]
    pub const fn is_pinned(&self) -> bool {
        self.pinned
    }

    /// 更新 B。由 compose 结果（`compose::ComposeOutcome::boundary`）逐帧驱动。
    pub fn set_boundary(&mut self, boundary: usize) {
        self.boundary = boundary;
    }

    /// 更新 pinned 策略。由根组件的 live-region 声明逐帧驱动。
    pub fn set_pinned(&mut self, pinned: bool) {
        self.pinned = pinned;
    }

    /// 纯函数：给定本帧 `frame_rows`（`L`）与 `viewport_height`（`h`），计算窗口/
    /// 提交计划，不修改账本自身状态。数学（模块文档已展开推导）：
    ///
    /// ```text
    /// W          = max(C, L.saturating_sub(h))
    /// chunk_to   = if pinned { min(W, B) } else { W }
    /// commit_end = max(C, chunk_to)
    /// exact_end  = min(C, B)
    /// ```
    #[must_use]
    pub fn plan(&self, frame_rows: usize, viewport_height: usize) -> WindowPlan {
        let window_top = self
            .committed_rows
            .max(frame_rows.saturating_sub(viewport_height));
        let chunk_to = if self.pinned {
            window_top.min(self.boundary)
        } else {
            window_top
        };
        let commit_end = self.committed_rows.max(chunk_to);
        let exact_end = self.committed_rows.min(self.boundary);
        WindowPlan {
            window_top,
            commit_end,
            window_height: viewport_height,
            exact_end,
        }
    }

    /// 把 `plan` 的结果写回账本：`committed_rows` 前进到 `plan.commit_end`，
    /// `window_top` 更新为 `plan.window_top`。`boundary`/`pinned` 不受影响
    /// （它们由 [`Ledger::set_boundary`] / [`Ledger::set_pinned`] 独立驱动）。
    pub fn apply(&mut self, plan: &WindowPlan) {
        self.committed_rows = plan.commit_end;
        self.window_top = plan.window_top;
    }

    /// 把 `C` 直接推进到 `end`（`self.committed_rows = self.committed_rows.max(end)`），
    /// 绝不倒退；`window_top`/`boundary`/`pinned` 不受影响。
    ///
    /// 与 [`Ledger::apply`] 的分工：`apply` 消费 [`WindowPlan`]，同时推进 `C` 与
    /// `W`，服务于有窗口概念的交互式路径。非交互输出路径（`emit::PlainStdout`
    /// 之类：没有可见窗口，只把 `[C, B)` 当纯文本整段写出去）没有 `W` 可言，
    /// 硬凑一个假 `WindowPlan` 再 `apply` 只会污染 `window_top` 的语义——它就
    /// 不该在这条路径上被改写。`commit_to` 只做账本最原始的那一半：确认
    /// `end` 之前的内容已经离开了程序可控范围。
    pub fn commit_to(&mut self, end: usize) {
        self.committed_rows = self.committed_rows.max(end);
    }

    /// 审计（[`crate::audit::audit_committed_prefix`]）发现 exact 区在 `row` 行
    /// 被重排：把 `C` 退回 `row`，让 `[row, ..)` 在下一帧重新提交。只降 `C`，
    /// 不动 `window_top`/`boundary`/`pinned`——`window_top` 会在下一次 [`Ledger::plan`]
    /// 里根据新的 `C` 重新算出，不需要在这里预判。
    ///
    /// 旧副本已经写进了终端 scrollback，这里不会、也不能删除它：
    /// duplication, never loss（见模块文档）。
    pub fn re_anchor(&mut self, row: usize) {
        self.committed_rows = self.committed_rows.min(row);
    }

    /// 全部归零：C/W/B 与 pinned 都回到初始状态。用于 session 替换等需要
    /// 从头重新开始记账的场景。
    pub fn reset(&mut self) {
        self.committed_rows = 0;
        self.window_top = 0;
        self.boundary = 0;
        self.pinned = false;
    }
}

#[cfg(test)]
mod tests {
    use super::{Ledger, WindowPlan};

    /// 首帧 `C = 0`，`L <= h` 时窗口贴顶、一个字节都不进历史。
    #[test]
    fn first_frame_within_viewport_commits_nothing() {
        let ledger = Ledger::new();
        let plan = ledger.plan(3, 10);
        assert_eq!(
            plan,
            WindowPlan {
                window_top: 0,
                commit_end: 0,
                window_height: 10,
                exact_end: 0
            }
        );
    }

    /// `L > h` 时窗口顶与提交末尾都落在 `L - h`。
    #[test]
    fn first_frame_taller_than_viewport_commits_scrolled_off_prefix() {
        let ledger = Ledger::new();
        let plan = ledger.plan(20, 5);
        assert_eq!(plan.window_top, 15);
        assert_eq!(plan.commit_end, 15);
    }

    /// `L` 恒定的连续帧：第 2、3 帧 `commit_end == C`，零新增提交。
    /// 这是固定高度活跃区必须走 in-window diff 的不变式（`architecture.md:66-70`）。
    #[test]
    fn stable_frame_height_commits_nothing_after_first_frame() {
        let mut ledger = Ledger::new();
        let plan1 = ledger.plan(20, 5);
        ledger.apply(&plan1);
        let committed_after_first = ledger.committed_rows();

        for _ in 0..2 {
            let plan = ledger.plan(20, 5);
            assert_eq!(plan.commit_end, ledger.committed_rows());
            ledger.apply(&plan);
        }
        assert_eq!(ledger.committed_rows(), committed_after_first);
    }

    /// pinned 且 `B < W`：可变尾部留在 viewport 内，`commit_end == max(C, B)`。
    #[test]
    fn pinned_live_region_clips_mutable_suffix_at_boundary() {
        let mut ledger = Ledger::new();
        ledger.set_pinned(true);
        ledger.set_boundary(8); // B(8) < W(15)
        let plan = ledger.plan(20, 5);
        assert_eq!(plan.window_top, 15);
        assert_eq!(plan.commit_end, ledger.committed_rows().max(8));
    }

    /// unpinned 且 `B < W`：C 超过 B，滚出窗口的可变行作为冻结快照提交，
    /// `commit_end == W`。
    #[test]
    fn unpinned_live_region_commits_frozen_snapshot_past_boundary() {
        let mut ledger = Ledger::new();
        ledger.set_boundary(8); // B(8) < W(15)，unpinned（默认）
        let plan = ledger.plan(20, 5);
        assert_eq!(plan.commit_end, plan.window_top);
    }

    /// `re_anchor(row)` 后 `C == min(旧C, row)`；下一帧 `commit_end` 重新覆盖
    /// `[row, W)`（duplication, never loss：旧副本留在历史里，未被删除）。
    #[test]
    fn re_anchor_reopens_committed_prefix_for_recommit() {
        let mut ledger = Ledger::new();
        let plan1 = ledger.plan(20, 5);
        ledger.apply(&plan1);
        let plan2 = ledger.plan(20, 5); // 高度不变，零增量提交
        ledger.apply(&plan2);
        let committed_before = ledger.committed_rows();
        assert_eq!(committed_before, 15);

        ledger.re_anchor(10);
        assert_eq!(ledger.committed_rows(), committed_before.min(10));

        let plan3 = ledger.plan(20, 5);
        assert_eq!(plan3.window_top, committed_before);
        assert_eq!(plan3.commit_end, committed_before);
        ledger.apply(&plan3);
        assert_eq!(ledger.committed_rows(), committed_before);
    }

    /// `L` 收缩到小于 `C`（帧塌缩）时 `window_top`/`commit_end` 不倒退。
    #[test]
    fn frame_collapse_does_not_regress_window_or_commit() {
        let mut ledger = Ledger::new();
        let plan1 = ledger.plan(20, 5);
        ledger.apply(&plan1);
        let committed = ledger.committed_rows();

        let plan2 = ledger.plan(3, 5); // 帧塌缩：L(3) < C(15)
        assert!(plan2.window_top >= committed);
        assert!(plan2.commit_end >= committed);
    }

    /// `commit_to` 只推进不倒退：先推到较大的值，再用较小的值调用不改变 `C`；
    /// 用更大的值调用则照常推进，且不动 `window_top`/`boundary`/`pinned`。
    #[test]
    fn commit_to_never_regresses_committed_rows() {
        let mut ledger = Ledger::new();
        ledger.set_boundary(7);
        ledger.set_pinned(true);
        ledger.commit_to(12);
        assert_eq!(ledger.committed_rows(), 12);

        ledger.commit_to(5); // 小于当前 C，必须原地不动
        assert_eq!(ledger.committed_rows(), 12);
        assert_eq!(ledger.window_top(), 0);
        assert_eq!(ledger.boundary(), 7);
        assert!(ledger.is_pinned());

        ledger.commit_to(20); // 大于当前 C，照常推进
        assert_eq!(ledger.committed_rows(), 20);
    }
}
