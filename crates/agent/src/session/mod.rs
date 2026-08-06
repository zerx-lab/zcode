//! 会话：数据模型与 JSONL 条目树存储。
//!
//! | 模块 | 职责 |
//! | --- | --- |
//! | [`message`] | 落盘态消息，以及与 [`zcode_ai`] 传输态消息的互转 |
//! | [`entry`] | 会话条目：JSONL 的一行，同时是会话树的一个节点 |
//! | [`store`] | 内存中的条目树与文件落盘 |
//!
//! # 事实来源
//!
//! 会话文件是历史的**唯一**事实来源。[`crate::event`] 的事件流是可丢的 UI 增量，
//! 任何需要精确重建的东西都从这里读。

pub mod entry;
pub mod message;
pub mod store;

pub use crate::session::entry::*;
pub use crate::session::message::*;
pub use crate::session::store::*;
