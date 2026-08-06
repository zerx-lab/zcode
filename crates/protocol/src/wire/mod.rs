//! wire 变体：客户端与运行时之间实际传输的 payload。
//!
//! | 模块 | 内容 |
//! |---|---|
//! | [`types`] | 领域投影：会话条目树、消息、用量、待回答项 |
//! | [`request`] | [`Request`]（客户端 → 运行时）与 [`Reply`] |
//! | [`event`] | [`Event`]（运行时 → 客户端的可丢推送） |
//!
//! # 两个方向各一个 payload 枚举
//!
//! 每一帧的 payload 是 [`ClientFrame`] 或 [`ServerFrame`]，套在 [`crate::Envelope`] 里。
//! 两者都用 `kind` 做内部 tag，内层枚举各自用 `type`，标签不冲突，线上形状是一层扁平对象：
//!
//! ```text
//! {"v":1,"id":7,"payload":{"kind":"request","type":"cancel","session":"ses_1"}}
//! ```
//!
//! # 请求只有一个方向
//!
//! 运行时**从不**向客户端发请求。需要用户回答的事情（审批、stdin）一律走
//! "推一条 `*Requested` 事件 + 客户端主动回一条 [`Request`] + pending 列表随时可重拉"。
//!
//! 理由是这两个上游缺陷同源——**待回答状态挂在连接上**：
//!
//! - opencode 的权限 pending 只在内存且重连不重拉，SSE 在 `permission.asked` 之后断开则
//!   服务端工具永久挂着而 UI 无显示（`packages/opencode/src/permission/index.ts:98-107`）；
//! - jcode 的 stdin oneshot 存在每连接的 map 里，连接一断工具侧立刻收 `Err`
//!   （`crates/jcode-app-core/src/server/client_lifecycle.rs:630-666`）。
//!
//! 挂在 session 上就都没有了。代价是运行时要为每个 pending 维护一份可查询状态，
//! 而不是一个躺在栈上的 `oneshot`。
//!
//! # payload 解析失败仍然要能回应
//!
//! 未知 [`Request`] 必须收到 [`crate::ErrorCode::UnsupportedRequest`]，但"解不出 payload"
//! 时枚举本身已经没法用了——**信封必须先于 payload 解出来**，否则连 `reply_to` 该填什么都
//! 不知道。所以服务端的读路径是两段式：
//!
//! ```no_run
//! use zcode_protocol::{ErrorCode, ProtocolError, RawEnvelope, wire::ClientFrame};
//!
//! # fn demo(line: &[u8]) -> Result<(), serde_json::Error> {
//! let raw: RawEnvelope = serde_json::from_slice(line)?;
//! match raw.parse_payload::<ClientFrame>() {
//!     Ok(frame) => { /* 正常分发 */ }
//!     Err(_) => {
//!         // 拿得到 raw.id，因此一定能回一帧带 reply_to 的错误。
//!         let probe = raw.probe();
//!         let _error = if probe.kind.as_deref() == Some("request") {
//!             ProtocolError::unsupported_request(probe.name.as_deref().unwrap_or("<unnamed>"))
//!         } else {
//!             ProtocolError::new(ErrorCode::MalformedFrame, "payload 结构不符")
//!         };
//!     }
//! }
//! # Ok(())
//! # }
//! ```

pub mod event;
pub mod request;
pub mod types;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

pub use crate::wire::event::Event;
pub use crate::wire::request::{FrameProbe, Pending, Reply, Request};
pub use crate::wire::types::{
    ApprovalId, ApprovalMode, ApprovalReply, AssistantContent, CallId, ClientId, CompactionReason,
    DisplayRole, Entry, EntryId, EntryKind, Image, Message, PendingApproval, PendingStdin, Policy,
    SessionId, SessionSummary, StdinId, StopReason, Tier, ToolProgress, ToolResultContent, Usage,
    UserContent,
};

use crate::envelope::Envelope;
use crate::error::ProtocolError;
use crate::version::{ClientAuth, ClientHello, ServerHello};

/// 客户端 → 运行时的一帧 payload。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ClientFrame {
    /// 握手第 1 帧：只出 nonce，**不带凭据**。
    Hello(ClientHello),
    /// 握手第 3 帧：校验过服务端应答之后才发。
    Auth(ClientAuth),
    /// 一条请求。必须收到恰好一帧回应。
    Request(Request),
    /// 客户端侧检出的协议错误（版本不匹配、服务端应答校验失败、帧结构不符）。
    Error(ProtocolError),
}

/// 运行时 → 客户端的一帧 payload。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ServerFrame {
    /// 握手第 2 帧：出 nonce，同时应答客户端的挑战。
    Hello(ServerHello),
    /// 对某条请求的回应。信封的 `reply_to` 必须指向那条请求。
    Reply(Reply),
    /// 可丢的推送。
    Event(Event),
    /// 协议错误。作为回应时同样要带 `reply_to`。
    Error(ProtocolError),
}

/// payload 尚未解析的信封。
///
/// 读路径**必须**先解它：payload 解析失败时，`id` 是回一帧带 `reply_to` 的错误所必需的。
pub type RawEnvelope = Envelope<Box<RawValue>>;

impl Envelope<Box<RawValue>> {
    /// 解析 payload。
    pub fn parse_payload<T: DeserializeOwned>(&self) -> Result<T, serde_json::Error> {
        serde_json::from_str(self.payload.get())
    }

    /// payload 解析失败后，看一眼它到底是什么，用来选错误码。
    ///
    /// 探针自身解析失败时返回全 `None`——它是错误路径上的最后一道防线，不再向上抛。
    #[must_use]
    pub fn probe(&self) -> FrameProbe {
        serde_json::from_str(self.payload.get()).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::{ClientFrame, RawEnvelope, Request, ServerFrame};
    use crate::envelope::Envelope;
    use crate::error::ErrorCode;
    use crate::version::{ClientAuth, ClientHello, Hello, Nonce, Proof, ServerHello};
    use crate::wire::event::Event;
    use crate::wire::types::SessionId;

    #[test]
    fn frame_tags_nest_without_colliding() {
        let frame = ClientFrame::Request(Request::Cancel {
            session: SessionId::from("ses_1"),
        });
        let json = serde_json::to_value(&frame).expect("帧必须可序列化");
        assert_eq!(json["kind"], "request");
        assert_eq!(json["type"], "cancel");
        assert_eq!(json["session"], "ses_1");
        assert_eq!(
            serde_json::from_value::<ClientFrame>(json).expect("帧必须可回读"),
            frame
        );
    }

    #[test]
    fn server_frame_absorbs_unknown_event_but_not_unknown_kind() {
        let unknown_event = r#"{"kind":"event","type":"quantum_delta","session":"ses_1"}"#;
        assert_eq!(
            serde_json::from_str::<ServerFrame>(unknown_event).expect("未知事件必须被吸收"),
            ServerFrame::Event(Event::Unknown)
        );

        // 未知 kind 没有兜底：它意味着对端大版本不同，属于握手该拦下的情况。
        assert!(serde_json::from_str::<ServerFrame>(r#"{"kind":"telepathy"}"#).is_err());
    }

    #[test]
    fn raw_envelope_keeps_id_when_payload_is_undecodable() {
        let line = serde_json::to_vec(&Envelope::new(
            42,
            serde_json::json!({"kind": "request", "type": "teleport", "target": "mars"}),
        ))
        .expect("帧必须可序列化");

        let raw: RawEnvelope = serde_json::from_slice(&line).expect("信封必须先解出来");
        assert_eq!(raw.id, 42);
        assert!(
            raw.parse_payload::<ClientFrame>().is_err(),
            "未知请求必须解析失败"
        );

        let probe = raw.probe();
        assert_eq!(probe.kind.as_deref(), Some("request"));
        assert_eq!(probe.name.as_deref(), Some("teleport"));

        // 有了 id 才能回一帧带 reply_to 的 UnsupportedRequest。
        let error = crate::ProtocolError::unsupported_request("teleport");
        assert_eq!(error.code, ErrorCode::UnsupportedRequest);
        let reply = Envelope::reply_to(1, raw.id, ServerFrame::Error(error));
        assert_eq!(reply.reply_to, Some(42));
    }

    #[test]
    fn handshake_frames_round_trip_and_client_hello_carries_no_credential() {
        let client = ClientFrame::Hello(ClientHello {
            hello: Hello::local("zcode-test"),
            nonce: Nonce("bm9uY2VfYw".to_owned()),
        });
        let text = serde_json::to_string(&client).expect("帧必须可序列化");
        assert!(text.contains(r#""kind":"hello""#));
        assert!(text.contains(r#""nonce":"bm9uY2VfYw""#));
        assert!(
            !text.contains("proof"),
            "客户端首帧绝不能带凭据：抢占 pipe 的进程一次连接就能收走它"
        );
        assert_eq!(
            serde_json::from_str::<ClientFrame>(&text).expect("帧必须可回读"),
            client
        );

        let server = ServerFrame::Hello(ServerHello {
            hello: Hello::local("zcode-daemon"),
            nonce: Nonce("bm9uY2Vfcw".to_owned()),
            proof: Proof("cHJvb2Y".to_owned()),
        });
        let text = serde_json::to_string(&server).expect("帧必须可序列化");
        assert_eq!(
            serde_json::from_str::<ServerFrame>(&text).expect("帧必须可回读"),
            server
        );

        let auth = ClientFrame::Auth(ClientAuth {
            proof: Proof("cHJvb2Yy".to_owned()),
        });
        let text = serde_json::to_string(&auth).expect("帧必须可序列化");
        assert!(text.contains(r#""kind":"auth""#));
        assert_eq!(
            serde_json::from_str::<ClientFrame>(&text).expect("帧必须可回读"),
            auth
        );
    }
}
