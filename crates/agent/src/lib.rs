//! `zcode-agent`：支持工具调用与状态管理的 Agent 运行时。
//!
//! # 分层
//!
//! | 模块 | 职责 |
//! | --- | --- |
//! | [`id`] | 会话 / 条目标识符：字典序即时间序，进程内严格单调 |
//! | [`interrupt`] | 中断信号：同步可读 + 异步可等 + epoch 保护的延迟复位 |
//! | [`cancel`] | 会话 → 在飞 turn / 后台作业的中断信号表，取消沿子会话递归 |
//! | [`event`] | 运行时事件流；慢消费者收 `Resync` 而不是断流 |
//! | [`session`] | 会话数据模型与 JSONL 条目树存储 |
//! | [`tool`] | 工具契约、注册表、批次调度 |
//! | [`approval`] | 审批裁决（tier × policy）与询问回环 |
//! | [`stdin`] | 工具执行途中要一行 stdin 的 oneshot-by-request-id 回环 |
//! | [`context`] | token 估算、压缩触发与保留策略 |
//! | [`turn`] | turn 循环：驱动提供商流、执行工具、维护历史 |
//!
//! # 这个 crate 不做什么
//!
//! - **不认识具体工具**。`read` / `bash` / `edit` 都住在 `zcode`（CLI crate），
//!   本 crate 只定义 [`tool::Tool`] 契约。
//! - **不碰 wire 协议**。`Request` / `Event` 变体归 `zcode-protocol` 所有，
//!   领域类型与 wire 类型的互转是 host adapter 的职责（见 `rule://zcode-architecture`）。
//! - **不依赖任何渲染栈**。运行时活在 daemon 进程里，`ratatui` / `crossterm` 绝不出现。
//!
//! # 事实来源的分工
//!
//! [`event`] 是**可丢**的 UI 增量流；[`session`] 才是历史的事实来源。任何需要精确重建的
//! 状态都要从会话存储读，不能靠攒事件。这条分工是"慢客户端不该造成历史丢失"的前提。

pub mod approval;
pub mod cancel;
pub mod context;
pub mod error;
pub mod event;
pub mod id;
pub mod interrupt;
pub mod session;
pub mod stdin;
pub mod tool;
pub mod turn;

pub use crate::approval::*;
pub use crate::cancel::*;
pub use crate::error::*;
pub use crate::event::*;
pub use crate::id::*;
pub use crate::interrupt::*;
pub use crate::session::*;
pub use crate::stdin::*;
pub use crate::tool::*;
pub use crate::turn::*;
