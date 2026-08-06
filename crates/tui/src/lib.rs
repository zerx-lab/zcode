//! `zcode-tui`：transcript 落在终端原生 scrollback，底部保留一块可重绘的活跃区。
//!
//! 不进 alternate screen，因此换来原生滚动、原生选择复制、退出后内容保留、
//! tmux detach 存活。代价与取舍见 `plans/tui/architecture.md`。
//!
//! # 分层
//!
//! | 模块 | 职责 |
//! | --- | --- |
//! | [`caps`] | 启动时能力判定 + Windows VT 启用（判定与施加分离） |
//! | [`terminal`] | fork 自 `ratatui::Terminal`：viewport 是可变 [`Rect`](ratatui::layout::Rect)，且暴露裸 writer |
//! | [`wrap`] | span 感知的按显示宽度换行，宽度全部经 `zcode-text` |
//! | [`insert_history`] | 把已定稿行写进 scrollback：DECSTBM 注入 + per-batch 模式选择 |
//! | [`compose`] | segment 账本：未变组件的行直接复用，只重排 stable prefix 之后 |
//! | [`ledger`] | C/W/B 账本：哪些行已提交、窗口落在哪、哪一行起仍可变 |
//! | [`audit`] | committed-prefix 审计与 re-anchor（duplication, never loss） |
//! | [`emit`] | 四条发射路径；`full_paint` 是全 crate 唯一的 ED3 callsite |
//! | `job_control`（`cfg(unix)`） | Unix 的 Ctrl-Z 挂起与恢复后重锚定 |
//! | `terminal_probe`（`cfg(unix)`） | Unix 的 `CSI 6n` 光标位置探测（`job_control` 的硬依赖） |
//!
//! # 五条不变量
//!
//! 违反其中任何一条产生的 bug 都很难在本机复现，改渲染路径前请先读
//! `plans/tui/README.md` 的对应条目：
//!
//! 1. **ED3（`CSI 3J`）单一 callsite。** 只出现在手势驱动的
//!    [`emit::Emitter`] 的 full paint 分支里，永不在增量路径，永不在 multiplexer 下。
//! 2. **普通增量帧零绝对定位。** in-window diff 与 scroll-append 的窗口重绘只用相对
//!    光标移动；`MoveTo(0, 0)` / `Clear` 会把已向上滚动的读者猛拽回底部。
//! 3. **历史注入是唯一允许绝对定位的增量路径**，且豁免按模式分层：
//!    [`insert_history::InsertHistoryMode::Standard`] 靠成对的滚动区把影响锁在 viewport 之上，
//!    [`insert_history::InsertHistoryMode::ZellijRaw`] 靠"先清 viewport + 同一 draw pass 内完整替换"。
//!    两套保护机制不可混用；共同要求是 cursor-position-neutral。
//! 4. **Windows VT 启用独立于 crossterm**，在任何 escape 之前，且跑过子进程后重新施加。
//! 5. **能力判定与 mode 施加分离。** 启动时定、全程只读，不在渲染路径反复探测终端。

pub mod audit;
pub mod caps;
pub mod compose;
pub mod emit;
pub mod insert_history;
pub mod ledger;
pub mod terminal;
pub mod wrap;

#[cfg(unix)]
pub mod job_control;
#[cfg(unix)]
pub mod terminal_probe;

pub use crate::audit::AuditOutcome;
pub use crate::caps::OutputCaps;
pub use crate::compose::{Component, ComponentId, ComposeOutcome, Composer};
pub use crate::emit::{EmitPath, Emitter};
pub use crate::insert_history::{HistoryLineWrapPolicy, InsertHistoryMode};
pub use crate::ledger::{Ledger, WindowPlan};
pub use crate::terminal::{Frame, Terminal};
