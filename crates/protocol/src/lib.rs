//! `zcode-protocol`：客户端（TUI / 编辑器插件 / 移动端）与 agent 运行时之间的 wire 协议。
//!
//! # 这个 crate 就是边界
//!
//! 依赖方向是 **`tui -> protocol <- runtime`**，两侧都不许绕过它互相直连。
//! 因此**所有 wire 类型都归本 crate 所有**：`Request` / `Event` 的变体定义在这里，
//! 领域类型（agent 状态、工具调用、模型描述符）与 wire 类型之间的互转是 host adapter 的职责。
//! 把变体定义权让给运行时会立刻毁掉边界——客户端为了反序列化就必须依赖运行时。
//!
//! jcode 在这条边上翻过车：`crates/jcode-tui/src/lib.rs:23` 用 `pub use jcode_app_core::*`
//! 把整个运行时转出，自陈动机是"让 `crate::<module>` 路径原样解析"——为了不改 import 而放弃
//! 编译边界，结果 TUI 能直接摸到 `crate::server` / `crate::agent`；而它的 CI 边界检查
//! （`scripts/check_dependency_boundaries.py:26-51`）只护 `*-types` crate，看不见这个洞。
//!
//! **教训：协议边界要么由编译器与 CI 强制，要么就不存在。**
//!
//! # 落地范围
//!
//! | 模块 | 内容 |
//! |---|---|
//! | [`version`] | 协议版本、[`Hello`] 握手、主版本协商 |
//! | [`envelope`] | 每帧信封：版本、单调 id、`reply_to` |
//! | [`frame`] | NDJSON 编解码与增量解帧器 |
//! | [`error`] | 结构化协议错误帧与错误码 |
//! | [`wire`] | payload 变体：[`Request`] / [`Reply`] / [`Event`] 与它们的领域投影 |
//!
//! 前四层与 agent 领域无关，是协议**自有**的部分；[`wire`] 是领域投影，
//! 与 `zcode-agent` 的落盘类型是两套形状，互转由 host adapter 负责。
//!
//! # 传输是可替换的
//!
//! 本 crate 不碰字节管道。跨进程走 `zcode_utils::transport`；进程内直接传 `enum` 走 channel，
//! **不序列化**。
//!
//! opencode 的反面教材很直接：它的 Worker RPC 为了"和 HTTP 一致"，把每个请求体
//! `await request.text()` 整体字符串化再解（`packages/opencode/src/util/rpc.ts:8`）。
//! **只有跨进程边界才付序列化成本。**
//!
//! # 未知变体：Event 可丢，Request 不可
//!
//! 常驻运行时必然遇到"新客户端 + 旧运行时"与"旧客户端 + 新运行时"。两个方向的处理**不对称**：
//!
//! - **推送类（`Event`、[`error::ErrorCode`]）**：internally tagged + `#[serde(other)]` 兜底，
//!   静默跳过。没人在等它的回音。
//! - **请求类（`Request`）**：**绝不可跳过**。请求方在等 `reply_to` 指向自己 `id` 的那一帧；
//!   跳过等于让调用方永久挂着。认不出来也必须回
//!   [`ProtocolError::unsupported_request`]。理由与实证见 [`error`] 的模块文档。
//!
//! 推送侧的兜底形状：
//!
//! ```
//! # use serde::Deserialize;
//! #[derive(Debug, Deserialize)]
//! #[serde(tag = "type", rename_all = "snake_case")]
//! enum Event {
//!     TextDelta { text: String },
//!     /// 对端比本端新时收到的未知事件。**静默跳过**，不得报错。
//!     #[serde(other)]
//!     Unknown,
//! }
//!
//! let from_newer_peer = br#"{"type":"quantum_delta","spin":"up"}"#;
//! assert!(matches!(
//!     serde_json::from_slice::<Event>(from_newer_peer)?,
//!     Event::Unknown
//! ));
//! # Ok::<(), serde_json::Error>(())
//! ```
//!
//! 抄源：jcode `crates/jcode-harness-api/src/events.rs:113-115`（规定 client 必须静默跳过）。
//! 反面是 opencode 的 `gracefulFetch`：把 404 伪造成空对象来兼容版本
//! （`packages/cli/src/tui.ts:36-45`）——版本协商该显式做，不该靠状态码猜。

pub mod envelope;
pub mod error;
pub mod frame;
pub mod version;
pub mod wire;

pub use envelope::{Envelope, IdGen};
pub use error::{ErrorCode, ProtocolError};
pub use frame::{FrameDecoder, FrameError, MAX_FRAME_BYTES, encode};
pub use version::{Hello, PROTOCOL_VERSION, Version, VersionMismatch};
pub use wire::{ClientFrame, Event, RawEnvelope, Reply, Request, ServerFrame};
