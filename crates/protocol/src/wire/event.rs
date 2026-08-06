//! 运行时推给客户端的事件。
//!
//! # 事件是可丢的，历史不是
//!
//! 每一条 [`Event`] 都可能因为客户端落后而丢失。事实来源是会话条目树，客户端在收到
//! [`Event::Resync`] 后用 [`crate::wire::Request::HistoryFetch`] 按游标补齐。
//!
//! 因此**绝不**把只在事件里出现过、条目树里查不到的状态设计成客户端的必需品。
//!
//! # 慢消费者不断流
//!
//! 上游两代都错在这里：opencode v1 用 `Queue.unbounded`，一个卡住的客户端就能把 daemon
//! 撑爆（`packages/opencode/src/server/routes/instance/httpapi/handlers/event.ts:30-32`）；
//! v2 换成 `Queue.dropping(256)` 却在 offer 被拒时 `Queue.fail` **打挂整条订阅流**
//! （`packages/core/src/event.ts:152-164`）——把"掉了几帧 UI 增量"升级成了"连接断开"。
//!
//! 本协议要求：有界缓冲 + 溢出转 [`Event::Resync`]。流不断，客户端补拉。
//!
//! # 每条事件都带 session
//!
//! turn 属于 session 而不属于连接，一条连接可以同时订阅多个 session，
//! 因此归属必须显式写在事件里。jcode 把 turn 绑在连接上（一条连接同时只有一个
//! `processing_message_id`，`crates/jcode-app-core/src/server/client_lifecycle.rs:461`），
//! 于是流式增量可以不带 id——本协议不能照抄那个省略。

use serde::{Deserialize, Serialize};

use crate::wire::types::{
    ApprovalId, CallId, EntryId, Message, PendingApproval, PendingStdin, SessionId, SessionSummary,
    StdinId, ToolProgress, Usage,
};

/// 运行时 → 客户端的推送。
///
/// 带 `#[serde(other)]` 兜底：来自更新对端的未知事件**必须静默跳过**，没人在等它的回音。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// 一次 turn 开始。
    TurnStart {
        /// 会话 id。
        session: SessionId,
        /// 触发本次 turn 的用户消息条目 id。
        user_entry: EntryId,
    },

    /// 助手消息开始。`entry` 是**开流前预分配**的条目 id，随后的每一条增量与
    /// [`Event::MessageEnd`] 都带同一个 id——中途接入或刚补拉完的客户端因此能立刻把增量
    /// 归属到正确的消息，不必等消息结束。
    MessageStart {
        /// 会话 id。
        session: SessionId,
        /// 预分配的消息条目 id。
        entry: EntryId,
    },

    /// 助手文本增量。
    TextDelta {
        /// 会话 id。
        session: SessionId,
        /// 消息条目 id。
        entry: EntryId,
        /// 内容块下标。
        index: u32,
        /// 新增文本。
        delta: String,
    },

    /// 思考增量。
    ThinkingDelta {
        /// 会话 id。
        session: SessionId,
        /// 消息条目 id。
        entry: EntryId,
        /// 内容块下标。
        index: u32,
        /// 新增文本。
        delta: String,
    },

    /// 工具调用参数的**原始 partial JSON** 增量。
    ///
    /// 绝不换成"已解析参数"：解析受节流窗口影响会滞后于流，`bash` 的内联环境变量赋值
    /// 可能要到 JSON 对象闭合前一刻才可见
    /// （oh-my-pi `packages/coding-agent/src/modes/controllers/tool-args-reveal.ts:10-14`）。
    ToolCallDelta {
        /// 会话 id。
        session: SessionId,
        /// 消息条目 id。
        entry: EntryId,
        /// 内容块下标。
        index: u32,
        /// 调用 id；首帧尚未给出时为空串。
        call_id: CallId,
        /// 新增 JSON 片段。
        delta: String,
    },

    /// 助手消息结束，携带完整消息。
    MessageEnd {
        /// 会话 id。
        session: SessionId,
        /// 消息条目 id。
        entry: EntryId,
        /// 完整消息。
        message: Box<Message>,
        /// 本次请求的用量。
        #[serde(default, skip_serializing_if = "Usage::is_empty")]
        usage: Usage,
    },

    /// 一个工具开始执行。
    ToolStart {
        /// 会话 id。
        session: SessionId,
        /// 调用 id。
        call_id: CallId,
        /// 工具名。
        name: String,
    },

    /// 工具执行中的增量输出。
    ToolProgress {
        /// 会话 id。
        session: SessionId,
        /// 调用 id。
        call_id: CallId,
        /// 增量。
        progress: ToolProgress,
    },

    /// 一个工具执行结束。
    ToolEnd {
        /// 会话 id。
        session: SessionId,
        /// 调用 id。
        call_id: CallId,
        /// 结果条目 id。
        entry: EntryId,
        /// 是否失败。
        is_error: bool,
    },

    /// 需要用户审批。
    ///
    /// 客户端**不能**只靠这条事件维持审批 UI：断连期间发生的询问不会重放，重连后必须用
    /// [`crate::wire::Request::PendingList`] 重拉。
    ApprovalRequested {
        /// 会话 id。
        session: SessionId,
        /// 待审批项。
        pending: PendingApproval,
    },

    /// 审批已结算。**任何**一条待审批消失都必须有这条事件，客户端靠它移除 UI。
    ///
    /// 包括连锁结算：`always` 会放行同作用域的其余 pending，`reject` 连坐同会话全部 pending，
    /// 每一条都要单独推一次（opencode `packages/opencode/src/permission/index.ts:129-166`）。
    ApprovalResolved {
        /// 会话 id。
        session: SessionId,
        /// 审批请求 id。
        request_id: ApprovalId,
        /// 是否放行。
        approved: bool,
    },

    /// 工具正在等待一行 stdin 输入。
    StdinRequested {
        /// 会话 id。
        session: SessionId,
        /// 待输入项。
        pending: PendingStdin,
    },

    /// stdin 询问已结算（用户回答、或 turn 取消导致作废）。
    StdinResolved {
        /// 会话 id。
        session: SessionId,
        /// stdin 请求 id。
        request_id: StdinId,
        /// 是否拿到了输入；`false` 表示被取消。
        submitted: bool,
    },

    /// 上下文被压缩。
    Compacted {
        /// 会话 id。
        session: SessionId,
        /// 压缩条目 id。
        entry: EntryId,
    },

    /// 会话元数据变了（标题、模型、head）。
    ///
    /// 多客户端共享会话时，另一端改了模型或回退了 head，本端必须跟着更新，
    /// 否则两个客户端会显示互相矛盾的状态。
    SessionUpdated {
        /// 会话 id。
        session: SessionId,
        /// 最新摘要。
        summary: SessionSummary,
        /// 当前 head。
        head: EntryId,
    },

    /// 一次 turn 结束。
    TurnEnd {
        /// 会话 id。
        session: SessionId,
    },

    /// 本次 turn 以错误告终。
    Failed {
        /// 会话 id。
        session: SessionId,
        /// 面向用户的错误文本。
        message: String,
    },

    /// 订阅者落后，中间丢了 `dropped` 条事件。
    ///
    /// **流没有断。** 收到它的客户端应当用 [`crate::wire::Request::HistoryFetch`] 按自己的
    /// 游标补拉，然后继续消费。
    Resync {
        /// 会话 id。
        session: SessionId,
        /// 被丢弃的事件条数。
        dropped: u64,
    },

    /// 对端比本端新。**静默跳过**，不得报错、不得断连。
    #[serde(other)]
    Unknown,
}

#[cfg(test)]
mod tests {
    use super::Event;
    use crate::wire::types::{CallId, EntryId, Message, SessionId, StopReason, Usage};

    #[test]
    fn unknown_event_is_absorbed_not_fatal() {
        let from_newer_peer = r#"{"type":"quantum_delta","session":"ses_1","spin":"up"}"#;
        assert_eq!(
            serde_json::from_str::<Event>(from_newer_peer).expect("未知事件必须静默跳过"),
            Event::Unknown
        );
    }

    #[test]
    fn deltas_carry_session_and_entry() {
        let event = Event::ToolCallDelta {
            session: SessionId::from("ses_1"),
            entry: EntryId::from("ent_2"),
            index: 0,
            call_id: CallId::from(""),
            delta: r#"{"cm"#.to_owned(),
        };
        let json = serde_json::to_value(&event).expect("事件必须可序列化");
        assert_eq!(json["type"], "tool_call_delta");
        assert_eq!(json["session"], "ses_1");
        assert_eq!(json["call_id"], "");
        assert_eq!(
            serde_json::from_value::<Event>(json).expect("事件必须可回读"),
            event
        );
    }

    #[test]
    fn message_end_omits_zero_usage() {
        let event = Event::MessageEnd {
            session: SessionId::from("ses_1"),
            entry: EntryId::from("ent_2"),
            message: Box::new(Message::Assistant {
                content: Vec::new(),
                model: None,
                usage: Usage::default(),
                stop_reason: StopReason::Stop,
            }),
            usage: Usage::default(),
        };
        let json = serde_json::to_value(&event).expect("事件必须可序列化");
        assert!(json.get("usage").is_none(), "全 0 用量不该上线");
    }

    #[test]
    fn resync_reports_drop_count() {
        let event: Event =
            serde_json::from_str(r#"{"type":"resync","session":"ses_1","dropped":12}"#)
                .expect("resync 必须可回读");
        assert!(matches!(event, Event::Resync { dropped: 12, .. }));
    }
}
