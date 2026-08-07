//! 四条发射路径的调度器。
//!
//! 每帧的流水线是固定的：`compose` → `audit` → `commit` → `emit`
//! （对照 `oh-my-pi/docs/tui-runtime-internals.md` 的同名流水线）。
//! 这里只负责**选哪条路径**与**按该路径的约束发字节**：排版归 [`crate::compose`]，
//! 窗口数学归 [`crate::ledger`]，历史注入的 escape 细节归 [`crate::insert_history`]。
//!
//! # 路径表
//!
//! 来源 `oh-my-pi/docs/tui-core-renderer.md:95-98`。
//!
//! | 路径 | 发出的字节 | 触发条件 |
//! | --- | --- | --- |
//! | [`EmitPath::FullPaint`] | 清屏（可选 ED3）+ 从 home 重放 committed prefix + 整窗 | 首帧、resize、`reset_display` |
//! | [`EmitPath::ScrollAppend`] | 历史注入 + 窗口 diff | 有行滚出窗口 |
//! | [`EmitPath::InWindowDiff`] | 只有窗口 diff（**零绝对定位**） | 无滚动、无提交 |
//! | [`EmitPath::SeamRewrite`] | 重锚定后的历史注入 + 整窗重写 | 审计发现 committed prefix 漂移 |
//! | [`EmitPath::PlainStdout`] | 纯文本行，无任何 escape | `interactive_output == false` |
//!
//! 最后一条不属于原表的四条 ANSI 路径：`interactive_output == false` 时**不做部分降级**
//! （无 VT 时相对光标移动同样不被解析），只有"完整 TUI"和"纯 stdout"两态
//! （`plans/tui/README.md:83`）。
//!
//! # 为什么 full paint 允许绝对 home
//!
//! 不变量 2（普通增量帧零绝对定位）**只约束 update 路径**——`in-window diff` 与
//! `scroll-append` 的窗口重绘（`plans/tui/architecture.md:221`）。`full_paint` 的定义本身
//! 就是 "home + committed chunk + 整窗，可选 ED3"（同上 `:57`）：它先清掉可见屏幕，
//! 于是整个可见区域都归它所有，绝对定位不会拽动任何读者视图。
//!
//! 代价是真实的：每次 resize 都把整个 committed transcript 从 home 重放一遍，
//! 长会话下会往 scrollback 再推一份。oh-my-pi 用 `#truncateLargeConptyFrame` 给 ConPTY
//! 打了补丁，本仓按 `plans/tui/modules.md:71` 的决定**不抄**那个性能补丁——
//! 等真的观察到 Windows 全量重放卡顿再说。

use std::io::{self, Write};

use ratatui::backend::Backend;
use ratatui::layout::Rect;
use ratatui::text::Line;

use crate::audit::{AuditOutcome, audit_committed_prefix};
use crate::caps::{self, OutputCaps};
use crate::compose::{Component, Composer};
use crate::insert_history::{
    HistoryLineWrapPolicy, insert_history_lines, insert_history_mode, write_history_line,
};
use crate::ledger::{Ledger, WindowPlan};
use crate::terminal::Terminal;
use crate::wrap;

/// 本帧实际走的发射路径。测试与诊断用；不参与渲染决策。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EmitPath {
    /// 清屏后从 home 重放整个 committed prefix。**唯一允许发 ED3 的路径。**
    FullPaint,
    /// 有行滚出窗口：先把它们注入 scrollback，再画窗口。
    ScrollAppend,
    /// 无提交：只有窗口内的 cell diff，全程相对光标移动。
    InWindowDiff,
    /// committed prefix 漂移后重锚定：旧副本留在历史里（duplication, never loss）。
    SeamRewrite,
    /// 非交互输出：直写纯文本，不进 inline TUI。
    PlainStdout,
}

/// 影响历史注入模式选择的两个 env 事实，启动时各读一次。
///
/// 单独成结构体是因为它们只在 [`insert_history_mode`] 这一处一起被消费，
/// 而且都是**不可变**的进程级事实——与 `full_paint_pending` 那类每帧翻转的状态位
/// 混在一个扁平的 struct 里容易被误当成可写标志。
///
/// **注意它们不是启动时能力。** 模式选择仍在每个 batch 做：env 不会中途变，
/// 但 wrap policy 会（`plans/tui/platform.md:228-246`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HistoryEnv {
    /// `ZELLIJ` / `ZELLIJ_SESSION_NAME` / `ZELLIJ_VERSION` 任一存在。
    is_zellij: bool,
    /// 逃生舱 `ZTUI_NO_SCROLL_REGION=1`：真机上发现某终端不照做滚动区时，
    /// 不改代码就能绕到非滚动区路径。
    no_scroll_region: bool,
}

/// 下一帧要走 full paint 的**理由**。决定发不发 ED3。
///
/// 依据 `plans/tui/architecture.md:197-206`：显式用户手势（session 替换、resize、
/// `reset_display`）默认启用 ED3，divergence rebuild 默认关闭。首帧不在手势之列——
/// 启动时终端里是用户自己的 shell 历史，擦掉它既不是用户要求的，也不属于我们。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FullPaintReason {
    /// 首帧：viewport 尺寸还没定过。清可见屏幕，但**不碰 scrollback**。
    FirstPaint,
    /// 显式手势：session 替换 / resize / `reset_display`。
    /// `caps.scrollback_purge` 为真时补 ED3；multiplexer 下降级为重放 + 重复段。
    Gesture,
}

/// transcript-first 渲染引擎。
///
/// 持有 [`Terminal`]、[`Composer`]、[`Ledger`] 与"已提交行的精确副本"，
/// 是这四者之间唯一的协调点。
pub struct Emitter<B>
where
    B: Backend<Error = io::Error> + Write,
{
    terminal: Terminal<B>,
    caps: OutputCaps,
    composer: Composer,
    ledger: Ledger,
    /// 已提交行的精确副本（tape）。审计比对的就是它与本帧 exact 区的逐行相等性。
    ///
    /// 整段 `[0, C)` 都要留着，不能只留 exact 区：re-anchor 可以退到任意行。
    /// 这份拷贝是审计能力的固有成本。
    committed_tape: Vec<Line<'static>>,
    /// 下一帧强制走 [`EmitPath::FullPaint`]，以及**为什么**——理由决定发不发 ED3。
    pending_full_paint: Option<FullPaintReason>,
    /// 启动时读一次的 env 事实，见 [`HistoryEnv`]。
    env: HistoryEnv,
    wrap_policy: HistoryLineWrapPolicy,
    last_path: Option<EmitPath>,
    /// 调用方请求的 pinned 值。账本里那份可能被本帧的溢出保护临时改掉，
    /// 所以请求值必须单独留一份，否则下一帧恢复不回来。
    pinned: bool,
}

impl<B> std::fmt::Debug for Emitter<B>
where
    B: Backend<Error = io::Error> + Write,
{
    // 手写而非 derive：`Terminal<B>` 的 `Debug` 会要求 `B: Debug`，
    // 而后端常常是不实现 `Debug` 的 writer 包装，不该把这个约束传染给调用方。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Emitter")
            .field("caps", &self.caps)
            .field("ledger", &self.ledger)
            .field("committed_rows", &self.committed_tape.len())
            .field("pending_full_paint", &self.pending_full_paint)
            .field("env", &self.env)
            .field("wrap_policy", &self.wrap_policy)
            .field("last_path", &self.last_path)
            .finish_non_exhaustive()
    }
}

impl<B> Emitter<B>
where
    B: Backend<Error = io::Error> + Write,
{
    /// 用已建好的 [`Terminal`] 与启动时判定的能力建引擎。
    ///
    /// `caps` 必须来自 [`OutputCaps::probe`]，且**全程只读**——不变量 5。
    #[must_use]
    pub fn new(terminal: Terminal<B>, caps: OutputCaps) -> Self {
        Self {
            terminal,
            caps,
            composer: Composer::new(),
            ledger: Ledger::new(),
            committed_tape: Vec::new(),
            // 首帧必须走 full paint：viewport 尺寸还没定过。
            pending_full_paint: Some(FullPaintReason::FirstPaint),
            env: HistoryEnv {
                is_zellij: caps::is_zellij(),
                no_scroll_region: caps::force_no_scroll_region(),
            },
            wrap_policy: HistoryLineWrapPolicy::PreWrap,
            last_path: None,
            pinned: false,
        }
    }

    /// 底层终端的只读引用。
    pub const fn terminal(&self) -> &Terminal<B> {
        &self.terminal
    }

    /// 底层终端的可变引用。
    ///
    /// 供 Unix job control 在挂起/恢复时重锚定 viewport 用。用它直接发字节会绕过
    /// 本引擎的账本，破坏不变量 1/2/3。
    pub fn terminal_mut(&mut self) -> &mut Terminal<B> {
        &mut self.terminal
    }

    /// 启动时判定的输出能力。
    pub const fn caps(&self) -> OutputCaps {
        self.caps
    }

    /// C/W/B 账本的只读视图。
    pub const fn ledger(&self) -> &Ledger {
        &self.ledger
    }

    /// 上一帧实际走的路径。首帧之前为 `None`。
    pub const fn last_path(&self) -> Option<EmitPath> {
        self.last_path
    }

    /// 历史行的换行策略。[`HistoryLineWrapPolicy::Terminal`] 把换行交给终端以保留
    /// soft-wrap 元数据（终端选择复制能拿到原始源文本），代价是在 Zellij 下必须改走
    /// [`crate::insert_history::InsertHistoryMode::ZellijRaw`]。
    pub fn set_wrap_policy(&mut self, policy: HistoryLineWrapPolicy) {
        self.wrap_policy = policy;
    }

    /// 活跃区是否 pinned。pinned 时可变尾部留在 viewport 内，不会被冻结提交。
    ///
    /// 固定高度仪表盘用 `true`；流式框用 `false` 并自己把高度钉进 viewport
    /// （`plans/tui/architecture.md:74-89` 记录了违反它的真实事故：框超出 viewport 时
    /// 可变尾部滚到 commit window 之上，每帧提交一份新快照，往 scrollback 堆重复 banner）。
    ///
    /// 这是**请求值**：活跃区高过整块屏幕时 [`Emitter::render`] 会本帧降级为
    /// unpinned，理由见那里。
    pub fn set_pinned(&mut self, pinned: bool) {
        self.pinned = pinned;
        self.ledger.set_pinned(pinned);
    }

    /// 显式手势触发整帧重画：session 替换 / 分支 / resume，以及调用方自己发现的
    /// geometry 变化（自动 resize 由 [`Emitter::render`] 内部处理，不必再调这里）。
    ///
    /// 与首帧的区别在于**会擦 scrollback**：手势本身已经把用户钉在 tail 上，
    /// 历史 snap 可以接受（`plans/tui/architecture.md:197-206`）。
    /// multiplexer 下 ED3 不可用，自动降级为在陈旧片段下方重新提交。
    pub fn request_full_paint(&mut self) {
        self.pending_full_paint = Some(FullPaintReason::Gesture);
    }

    /// `ctrl+o` 那条重路径：让折叠/展开作用于**已滚出屏幕**的内容。
    ///
    /// DECSTBM 无法更新已提交行，唯一途径是擦掉整个 scrollback 并重放
    /// （`plans/tui/architecture.md:179-196`）。ED3 在 multiplexer 下不安全，
    /// 此时降级为"重锚定 + 在陈旧片段下方重新提交"——duplication, never loss
    /// （`oh-my-pi/packages/tui/src/tui.ts:12-13`）。tmux 下因此会看到重复段，
    /// 这比"toggle 毫无反应"好，也比"擦错东西"安全。
    pub fn reset_display(&mut self) {
        self.pending_full_paint = Some(FullPaintReason::Gesture);
        // 组件的展开状态变了，缓存的行按旧状态渲染过，必须整体作废。
        self.composer.reset();
    }

    /// 收起活跃区，把光标停在它原来的顶行，让后续输出（通常是 shell 提示符）
    /// 从一片干净的地方开始。
    ///
    /// **退出路径必须调它。** 活跃区里画的是输入框、状态行这类"还会变"的东西，它们
    /// 从来没被提交进 scrollback；进程一走，这些字节就原样烙在终端上——用户会看到
    /// shell 提示符下方挂着半个圆角框，且框的 SGR 还没关，后面每一行都染上边框色。
    ///
    /// 已经提交进 scrollback 的 transcript 不受影响：这里只清 viewport 及其下方，
    /// 那是本来就属于活跃区的区域。
    ///
    /// # Errors
    ///
    /// 终端写入失败。调用方通常在 `Drop` 里调它，只记日志、不传播——退出路径要尽力
    /// 把终端还原，一步失败不该连累后面几步。
    pub fn shutdown(&mut self) -> io::Result<()> {
        let top = self.terminal.viewport_area().as_position();
        self.terminal.clear_after_position(top)?;
        Write::flush(self.terminal.backend_mut())?;
        Ok(())
    }

    /// 渲染一帧。返回本帧实际走的路径。
    ///
    /// # 活跃区高度不是调用方给的
    ///
    /// 它由 [`ComposeOutcome::boundary`] 决定：`total_rows - boundary` 恰好是「第一条
    /// 仍可能变化的行到帧尾」的行数。`min_viewport_height` 只是**下限**，给那种
    /// 「框高度固定、内容在框里滚动」的调用方用；正常 transcript 客户端传 `1` 即可。
    ///
    /// 早期版本让调用方自己数活跃区行数，那是个错误设计：调用方要把状态行、弹窗、
    /// 输入框、仍在直播的块各渲染一遍才能数出行数——既是双倍渲染成本，又必然与
    /// `compose` 的实际结果漂移。漂移一旦发生，viewport 装不下活跃内容，顶部几行
    /// 既没进历史也没画进窗口，表现是**消息凭空消失**，且不会自愈。
    ///
    /// 唯一的事实来源只能是 `compose` 自己刚算出来的那份行账本。
    pub fn render(
        &mut self,
        components: &[&dyn Component],
        min_viewport_height: u16,
    ) -> io::Result<EmitPath> {
        if self.terminal.autoresize()? {
            // 终端尺寸变了：composer 的缓存按旧宽度渲染，账本的 W 也按旧高度算过，
            // 两者都必须整体作废，否则窗口切片会落在错的行上。
            //
            // resize 算 geometry rebuild，是三类默认发 ED3 的显式手势之一
            // （`plans/tui/architecture.md:197-206`；上游判定见
            // `oh-my-pi/packages/tui/src/tui.ts:3186` 的
            // `(replaceRequested || geometryRebuild) && !isMultiplexerSession()`）。
            // 不擦就意味着每次 resize 都把 [0, C) 再往 scrollback 追加一份。
            self.pending_full_paint = Some(FullPaintReason::Gesture);
            self.composer.reset();
        }
        let screen = self.terminal.last_known_screen_size();
        let width = screen.width.max(1);

        let outcome = self.composer.compose(components, width);
        self.ledger.set_boundary(outcome.boundary);

        if !self.caps.interactive_output {
            return self.emit_plain(outcome.boundary);
        }

        // 活跃区 = 从 boundary 到帧尾。`total_rows >= boundary` 由 `compose` 保证
        // （boundary 取自某个组件的起始行 + 它自报的偏移，已 clamp 到该组件行数）。
        let live_rows =
            u16::try_from(outcome.total_rows.saturating_sub(outcome.boundary)).unwrap_or(u16::MAX);
        let requested = live_rows.max(min_viewport_height);
        let height = requested.clamp(1, screen.height.max(1));

        // **活跃区比整块屏幕还高**：`height` 被 clamp 到屏幕高度，于是
        // `W = L - height > B`。pinned 分支把 `commit_end` 卡在 `B`
        // （`ledger.rs:157-158`），`[B, W)` 这段就既没进 scrollback、也没落在窗口里
        // ——凭空消失，而且不会自愈。
        //
        // 屏幕装不下的东西必须有个去处，本引擎的既定取舍是 duplication, never loss
        // （`ledger.rs:20-27`）：本帧降级 unpinned，让滚出窗口的可变行以"滚出那一刻的
        // 样子"作为冻结快照提交进历史。代价是这些行之后不可再改写；收益是它们还在。
        //
        // 只改账本里那份，不动 `self.pinned`：下一帧一旦装得下就自动恢复 pinned。
        let fits = requested <= screen.height.max(1);
        if self.pinned && !fits {
            tracing::debug!(
                live_rows,
                screen_height = screen.height,
                "活跃区高于屏幕，本帧降级 unpinned 以免丢行"
            );
        }
        self.ledger.set_pinned(self.pinned && fits);

        self.sync_viewport_geometry(width, height);
        let rows = usize::from(height);

        let path = if let Some(reason) = self.pending_full_paint.take() {
            // full paint 不复用旧的 C：committed prefix 会被整段重放，
            // 所以先把账本退回 0 再算窗口（对照 `tui.ts:3090-3093` 的
            // `committedPrefixResliced = true` 分支）。
            //
            // 这一步同时就是 multiplexer 下的降级路径：拿不到 ED3 时，重放照做，
            // 于是陈旧片段留在 scrollback、新内容接在它下方 —— duplication, never loss。
            self.ledger.re_anchor(0);
            self.committed_tape.clear();
            let plan = self.ledger.plan(outcome.total_rows, rows);
            self.emit_full_paint(&plan, reason)?;
            self.ledger.apply(&plan);
            self.draw_window(&plan)?;
            EmitPath::FullPaint
        } else {
            let mut plan = self.ledger.plan(outcome.total_rows, rows);
            let mut seam = false;
            if let AuditOutcome::ReAnchor { row } =
                audit_committed_prefix(&self.committed_tape, self.composer.frame(), plan.exact_end)
            {
                tracing::debug!(row, "committed prefix 漂移，重锚定");
                self.ledger.re_anchor(row);
                self.committed_tape.truncate(row);
                // C 退了，窗口与提交计划必须按新的 C 重算。
                plan = self.ledger.plan(outcome.total_rows, rows);
                seam = true;
            }

            let path = if seam {
                // 重锚定后 viewport 里残留的是按旧行号画的内容，back buffer 已不可信。
                self.terminal.invalidate_viewport();
                EmitPath::SeamRewrite
            } else if plan.commit_end > self.ledger.committed_rows() {
                EmitPath::ScrollAppend
            } else {
                EmitPath::InWindowDiff
            };
            self.emit_commit_chunk(&plan)?;
            self.ledger.apply(&plan);
            self.draw_window(&plan)?;
            path
        };

        self.last_path = Some(path);
        Ok(path)
    }

    /// 把 viewport 的宽高对齐到当前屏幕，并保证它整体落在屏幕内。
    ///
    /// `y` 是历史注入推着走的（[`crate::insert_history`] 的 `Standard` 路径会把它往下推），
    /// 这里只做 clamp，**绝不主动上移**——上移会让下一帧覆盖掉已经进过历史的行。
    fn sync_viewport_geometry(&mut self, width: u16, height: u16) {
        let screen = self.terminal.last_known_screen_size();
        let area = self.terminal.viewport_area();
        let next = Rect {
            x: 0,
            y: area.y.min(screen.height.saturating_sub(height)),
            width,
            height,
        };
        if area != next {
            self.terminal.set_viewport_area(next);
        }
    }

    /// 非交互输出：把新定稿的行（`[C, B)`）当纯文本写出去，viewport 不存在。
    ///
    /// 不发任何 escape：这条路径下相对光标移动同样不被解析，部分降级只会产出乱码。
    /// 只写到 `B` 而不是整帧——`B` 之后的行还会变，提前写出去就成了不可撤回的噪声。
    fn emit_plain(&mut self, boundary: usize) -> io::Result<EmitPath> {
        let start = self.ledger.committed_rows();
        if boundary > start {
            let Self {
                terminal,
                composer,
                committed_tape,
                ledger,
                ..
            } = self;
            let finalized = composer.frame().iter().take(boundary).skip(start);
            let mut text = String::new();
            for line in finalized.clone() {
                for span in &line.spans {
                    text.push_str(&zcode_text::width::sanitize_text(&span.content));
                }
                text.push('\n');
            }
            let backend = terminal.backend_mut();
            backend.write_all(text.as_bytes())?;
            Write::flush(backend)?;
            committed_tape.extend(finalized.cloned());
            ledger.commit_to(boundary);
        }
        self.last_path = Some(EmitPath::PlainStdout);
        Ok(EmitPath::PlainStdout)
    }

    /// 清屏并从 home 重放 `[0, commit_end)`，再预留 viewport 的空行。
    ///
    /// **全 crate 唯一的 ED3（`CSI 3 J`）字节就在下面这个分支里。** `terminal.rs` 里一个都没有，
    /// 正因为 `terminal` 模块是公开的：一个公开的 ED3 方法等于给外部调用者一条绕过
    /// "用户手势 + [`OutputCaps::scrollback_purge`]"检查的后门
    /// （`plans/tui/modules.md:34`、`plans/tui/README.md:87`）。
    ///
    /// 两种形态都先经 [`Terminal::clear_visible_screen`]（复位滚动区 + SGR + home + ED2 + home）；
    /// [`FullPaintReason::Gesture`] 且 [`OutputCaps::scrollback_purge`] 为真时再补一条 ED3。
    /// ED3 单独发出时不移动光标、不改可见屏幕，所以不需要额外的状态修补。
    ///
    /// 拿不到 ED3（multiplexer）时不做任何补偿：重放照做，陈旧片段留在 scrollback、
    /// 新内容接在它下方——duplication, never loss（`plans/tui/architecture.md:208-216`）。
    ///
    /// 保留 ED2 是与 oh-my-pi 的一处刻意分歧：它的 window 高度恒等于终端高度，重放覆盖
    /// 每一个可见行，所以能只发 ED3 以免在没有同步输出（DEC 2026）的终端上露出一帧空屏
    /// （`oh-my-pi/packages/tui/src/tui.ts:3670-3674`）；而本引擎的 viewport 高度由调用方决定、
    /// 可以小于屏幕，重放覆盖不到 viewport 以下的行，少了 ED2 那些行会留着陈旧内容。
    ///
    /// 重放**不走** [`insert_history_lines`]：DECSTBM 注入是为了"viewport 一个字节都不重绘"，
    /// 而 full paint 刚刚把整个可见屏幕清掉了，直接从 home 打印更简单也更省字节。
    fn emit_full_paint(&mut self, plan: &WindowPlan, reason: FullPaintReason) -> io::Result<()> {
        self.terminal.clear_visible_screen()?;
        if reason == FullPaintReason::Gesture && self.caps.scrollback_purge {
            let backend = self.terminal.backend_mut();
            backend.write_all(b"\x1b[3J")?;
            Write::flush(backend)?;
        }

        let area = self.terminal.viewport_area();
        let wrap_width = usize::from(area.width.max(1));
        let screen = self.terminal.last_known_screen_size();

        let Self {
            terminal,
            composer,
            committed_tape,
            ..
        } = self;
        let prefix = composer.frame().iter().take(plan.commit_end);
        let mut physical_rows = 0usize;
        {
            let backend = terminal.backend_mut();
            // `\r\n` 是**分隔符**而不是终止符：写完最后一行时光标停在那一行上，
            // 于是「已写行数」与「光标所在行」始终差 1，预留空行的算法不需要分支
            // （照抄 codex `insert_history.rs:169-181` 的形状）。
            //
            // prefix 与预留空行共用同一个 `rows_written` 计数，所以 prefix 为空时
            // **不会**先发一个 `\r\n` 把 viewport 顶推到 row 1——那正是 top 算成 0
            // 却从 row 1 开始画的一行错位。
            let mut rows_written = 0usize;
            for line in prefix.clone() {
                for wrapped in wrap::wrap_line(line, wrap_width) {
                    if rows_written > 0 {
                        backend.write_all(b"\r\n")?;
                    }
                    write_history_line(backend, &wrapped, wrap_width)?;
                    rows_written = rows_written.saturating_add(1);
                    physical_rows = physical_rows.saturating_add(1);
                }
            }
            // 预留 viewport 的空行；溢出屏高的部分由终端自己滚进 scrollback。
            for _ in 0..area.height {
                if rows_written > 0 {
                    backend.write_all(b"\r\n")?;
                }
                backend.write_all(b"\x1b[K")?;
                rows_written = rows_written.saturating_add(1);
            }
            Write::flush(backend)?;
        }
        committed_tape.extend(prefix.cloned());

        let printed = u16::try_from(physical_rows).unwrap_or(u16::MAX);
        let top = printed.min(screen.height.saturating_sub(area.height));
        terminal.set_viewport_area(Rect { y: top, ..area });
        // 屏幕刚被清过，back buffer 里的内容与真实屏幕已经无关。
        terminal.invalidate_viewport();
        // 光标停在预留空行的最后一行，而 `Terminal` 的 `last_known_cursor_pos` 还是清屏前的值。
        // 必须在**本路径内**用绝对定位把两者对齐：下一次 `insert_history_lines` 要靠这个值
        // 恢复光标（cursor-position-neutral 契约），账本失真会让它把光标丢到屏幕别处。
        terminal.set_cursor_position(Rect { y: top, ..area }.as_position())?;
        Ok(())
    }

    /// 把 `[C, commit_end)` 注入 scrollback，并把这些行记进 tape。
    ///
    /// 注入模式在这里 **per-batch** 计算：同一会话内 wrap policy 会变，
    /// 做成启动时的 `scroll_regions: bool` 会走错路径（`plans/tui/modules.md:55-57`）。
    fn emit_commit_chunk(&mut self, plan: &WindowPlan) -> io::Result<()> {
        let start = self.ledger.committed_rows();
        if plan.commit_end <= start {
            return Ok(());
        }
        let chunk: Vec<Line<'static>> = self
            .composer
            .frame()
            .iter()
            .take(plan.commit_end)
            .skip(start)
            .cloned()
            .collect();
        let mode = insert_history_mode(
            self.env.is_zellij,
            self.env.no_scroll_region,
            self.wrap_policy,
        );
        insert_history_lines(&mut self.terminal, &chunk, mode, self.wrap_policy)?;
        self.committed_tape.extend(chunk);
        Ok(())
    }

    /// 画可见窗口 `[W, W+h)`。走 [`Terminal::draw`] 的 cell diff，全程相对光标移动。
    fn draw_window(&mut self, plan: &WindowPlan) -> io::Result<()> {
        let Self {
            terminal, composer, ..
        } = self;
        let frame_rows = composer.frame();
        let top = plan.window_top;
        let height = plan.window_height;
        terminal.draw(|frame| {
            let area = frame.area();
            let buf = frame.buffer_mut();
            for (offset, line) in frame_rows.iter().skip(top).take(height).enumerate() {
                let Ok(dy) = u16::try_from(offset) else {
                    break;
                };
                if dy >= area.height {
                    break;
                }
                buf.set_line(area.x, area.y.saturating_add(dy), line, area.width);
            }
        })
    }
}
