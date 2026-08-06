//! 客户端发往运行时的请求，以及运行时对它们的回应。
//!
//! # 每条请求恰好一帧回应
//!
//! [`Request`] **没有** `#[serde(other)]` 兜底变体，这是刻意的：认不出来的请求必须回一帧
//! [`crate::ProtocolError`]（[`crate::ErrorCode::UnsupportedRequest`]），静默跳过会让等
//! `reply_to` 的调用方永久挂着。解不出 payload 时用 [`FrameProbe`] 分类失败原因——
//! 信封先按 [`crate::wire::RawEnvelope`] 解，帧序号在 payload 解析失败后仍然拿得到。
//!
//! # 只有一个方向有请求
//!
//! 运行时**从不**向客户端发请求。工具执行途中需要用户输入（审批、stdin）时，运行时推一条
//! `Event::*Requested`，客户端用 [`Request::ApprovalRespond`] / [`Request::StdinRespond`]
//! 回环，pending 列表随时可由 [`Request::PendingList`] 重拉。
//!
//! 这是对两个上游缺陷的同时修正：
//!
//! - opencode 的权限 pending 只在内存、重连不重拉，SSE 在 `permission.asked` 之后断开则
//!   服务端工具永久挂着而 UI 无显示（`packages/opencode/src/permission/index.ts:98-107`，
//!   重连路径 `packages/tui/src/context/sync.tsx:451-532` 没有 `permission.list`）。
//! - jcode 的 stdin 回环把 oneshot 存在**每连接**的 map 里
//!   （`crates/jcode-app-core/src/server/client_lifecycle.rs:630-666`），连接一断整张表随
//!   栈帧 drop，工具侧 `response_rx.await` 收 `Err` 退出，而子进程还卡在读 stdin。
//!
//! 两者的共同根因是"待回答状态挂在连接上"。本协议把它挂在 session 上，因此断连、换客户端、
//! 多客户端同时看都不会丢。
//!
//! # 游标就是条目 id
//!
//! 条目 id 字典序即时间序（生成规则见 `zcode_agent::id`），所以 `since` 用条目 id 就够了，
//! 不需要另造一套序号。[`Request::Subscribe`] 与 [`Request::HistoryFetch`] 共用这一个游标
//! 语义：返回**追加顺序上**排在 `since` 之后的条目。
//!
//! 这比 jcode 的 `client_has_local_history: bool` 强：那个布尔在 server 侧只当 takeover
//! 授权凭证用，并不裁剪 History 载荷，"轻量"实际发生在客户端丢弃
//! （`crates/jcode-app-core/src/server/client_session.rs:1264-1265,1343-1356`）。

use serde::{Deserialize, Serialize};

use crate::wire::types::{
    ApprovalId, ApprovalMode, ApprovalReply, ClientId, Entry, EntryId, PendingApproval,
    PendingStdin, SessionId, SessionSummary, StdinId, UserContent,
};

/// 客户端 → 运行时的请求。
///
/// 每条请求都必须收到恰好一帧回应：[`Reply`] 或 [`crate::ProtocolError`]，
/// 且外层信封的 `reply_to` 指向本请求的 `id`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    /// 存活探测。**不做**任何副作用，健康认证靠握手完成。
    Ping,

    /// 列出会话。`cwd` 非空时只列该工作目录下的会话。
    ///
    /// **绝不附带条目树**：会话选择器打开一次就传输全部历史是不可接受的。
    SessionList {
        /// 按工作目录过滤。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
    },

    /// 新建会话。
    SessionCreate {
        /// 工作目录。
        cwd: String,
        /// 初始模型 id。
        model: String,
    },

    /// 订阅一个会话的事件流，并取回自 `since` 之后的条目。
    ///
    /// 重复订阅同一 session 是幂等的：重连后直接再发一次即可，运行时不会因此重建 turn。
    Subscribe {
        /// 会话 id。
        session: SessionId,
        /// 客户端实例 id：**同一个客户端**重连时必须原样带回。
        ///
        /// 接管仲裁的三元判据之一（`plans/runtime-boundary/implementation.md:83-84`，
        /// 抄源 jcode `server/client_session.rs:1264-1265,1417-1418,1485-1490`）：
        /// 实例 id 相同说明这是同一个客户端重连，不是第二个客户端来抢会话。
        client: ClientId,
        /// 本端是否已有该会话的本地历史。
        ///
        /// 三元判据之二。它**不是** `since.is_some()` 的同义词：客户端可以持有本地历史
        /// 却仍要求全量重拉（本地副本可疑时），那时它依然是"回来的老客户端"而不是新客户端。
        #[serde(default)]
        has_local_history: bool,
        /// 显式请求接管：已有别的客户端占着这个会话时，是否把它挤下去。
        ///
        /// 三元判据之三。默认 `false`——**默认不抢**，运行时应回错误而不是静默踢人。
        #[serde(default)]
        takeover: bool,
        /// 客户端已有的最后一条条目 id；`None` 表示要全量历史。
        ///
        /// 这是**载荷游标**，与上面三个仲裁字段职责不同：条目 id 字典序即时间序，
        /// 运行时只回追加顺序上排在它之后的条目。jcode 那个 `client_has_local_history`
        /// 在 server 侧只当授权凭证用、并不裁剪 History 载荷
        /// （`server/client_session.rs:1343-1356`），"轻量"实际发生在客户端丢弃——
        /// 本协议把两件事拆成两个字段，各自做各自的事。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        since: Option<EntryId>,
    },

    /// 退订。**不影响** turn：turn 属于 session，不属于连接。
    Unsubscribe {
        /// 会话 id。
        session: SessionId,
    },

    /// 按游标补拉条目。收到 `Event::Resync` 后走这条路补齐丢失的增量。
    HistoryFetch {
        /// 会话 id。
        session: SessionId,
        /// 已有的最后一条条目 id；`None` 表示全量。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        since: Option<EntryId>,
    },

    /// 投入一轮用户输入并开始 turn。
    Prompt {
        /// 会话 id。
        session: SessionId,
        /// 用户输入的内容块（文本 + 内联图片）。
        content: Vec<UserContent>,
    },

    /// 取消当前 turn。
    ///
    /// **运行时必须在写出任何回应字节之前先把取消打到中断信号上。** 共享出站 writer 在
    /// 大帧回放或客户端反压时可能被占很久，先写 Ack 再分发会让取消排在出站积压后面，
    /// 症状是"连按 Esc 没反应"（jcode
    /// `crates/jcode-app-core/src/server/client_lifecycle.rs:949-1018` 的注释与实现）。
    Cancel {
        /// 会话 id。
        session: SessionId,
    },

    /// 立刻压缩上下文。
    Compact {
        /// 会话 id。
        session: SessionId,
    },

    /// 把 head 指到另一条已存在的条目：`/rewind` 与 `/branch` 都是这一个操作。
    SetHead {
        /// 会话 id。
        session: SessionId,
        /// 新的 head 条目 id。
        entry: EntryId,
    },

    /// 切换模型。
    SetModel {
        /// 会话 id。
        session: SessionId,
        /// 新的模型 id。
        model: String,
    },

    /// 修改标题。
    SetTitle {
        /// 会话 id。
        session: SessionId,
        /// 新标题。
        title: String,
    },

    /// 切换审批模式。
    SetApprovalMode {
        /// 会话 id。
        session: SessionId,
        /// 新模式。
        mode: ApprovalMode,
    },

    /// 回答一条审批询问。
    ///
    /// 重复回答同一个 `request_id` 静默成功（`Reply::Ok`）：多客户端同时看同一个会话时，
    /// 两个人几乎同时点"允许"是正常操作，不是错误。
    ApprovalRespond {
        /// 审批请求 id。
        request_id: ApprovalId,
        /// 答复。
        reply: ApprovalReply,
    },

    /// 回答一条 stdin 询问。
    StdinRespond {
        /// stdin 请求 id。
        request_id: StdinId,
        /// 用户输入的一行文本（不含换行）。
        text: String,
    },

    /// 重拉所有待回答项。**重连后必调**——这是"工具永久挂着而 UI 无显示"的唯一解药。
    PendingList {
        /// 会话 id。
        session: SessionId,
    },
}

/// 运行时对一条 [`Request`] 的回应。
///
/// 带 `#[serde(other)]` 兜底：请求方知道自己发的是什么，回应解不出来时按"不支持"结算，
/// 也好过让等待者永久挂着。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Reply {
    /// 操作成功，无返回值。
    Ok,
    /// [`Request::Ping`] 的回应。
    Pong,
    /// 会话列表。
    Sessions {
        /// 摘要列表，按最后更新时间倒序。
        sessions: Vec<SessionSummary>,
    },
    /// [`Request::SessionCreate`] 的回应。
    SessionCreated {
        /// 新会话的摘要。
        summary: SessionSummary,
        /// 根条目。
        root: Entry,
    },
    /// [`Request::Subscribe`] 的回应：一次拿齐重建 UI 所需的全部状态。
    Subscribed {
        /// 会话摘要。
        summary: SessionSummary,
        /// 当前 head。
        head: EntryId,
        /// `since` 之后的条目，按追加顺序。
        entries: Vec<Entry>,
        /// 待回答项。**必须在订阅回应里一并返回**，否则重连的客户端看不到正在等它的审批。
        pending: Pending,
        /// 该会话当前是否有 turn 在跑。
        turn_active: bool,
    },
    /// 会话已被另一个客户端占用，且本次 [`Request::Subscribe`] 没有请求接管。
    ///
    /// **默认不抢**：静默踢掉另一端会让两个客户端互相把对方挤下去，形成抢占循环。
    /// 客户端拿到它之后应当询问用户，再带 `takeover: true` 重发。
    SessionBusy {
        /// 当前占用者的客户端实例 id。
        holder: ClientId,
    },
    /// [`Request::HistoryFetch`] 的回应。
    History {
        /// `since` 之后的条目，按追加顺序。
        entries: Vec<Entry>,
    },
    /// [`Request::Prompt`] 的回应。
    TurnStarted {
        /// 刚写入的用户消息条目 id。
        user_entry: EntryId,
    },
    /// [`Request::PendingList`] 的回应。
    Pending {
        /// 待回答项。
        pending: Pending,
    },
    /// 对端比本端新。按"该请求不被支持"结算，不要重试。
    #[serde(other)]
    Unknown,
}

/// 一个会话上所有等待用户回答的项。
///
/// 审批与 stdin 是同一个机制的两种载荷，因此一次取回：重连的客户端只需要一次往返
/// 就能把询问类 UI 完整重建。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Pending {
    /// 待审批。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub approvals: Vec<PendingApproval>,
    /// 待输入。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stdin: Vec<PendingStdin>,
}

impl Pending {
    /// 是否没有任何待回答项。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.approvals.is_empty() && self.stdin.is_empty()
    }
}

/// payload 解不出来时用来分类失败原因的探针。
///
/// 用法：信封按 [`crate::wire::RawEnvelope`] 解出 `id` 与原始 payload；payload 解成
/// [`crate::wire::ClientFrame`] 失败时再用本探针看一眼——`kind == "request"` 说明对端发的
/// 是一条本端不认识的请求，回 [`crate::ErrorCode::UnsupportedRequest`] 并带上 `name`；
/// 否则回 [`crate::ErrorCode::MalformedFrame`]。
///
/// 探针本身**必须**能吸收任何 JSON 对象：它是错误路径上的最后一道防线，自己再解析失败
/// 就只能退回 `MalformedFrame`。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct FrameProbe {
    /// 外层帧类别（`hello` / `request` / `error` / …）。
    #[serde(default)]
    pub kind: Option<String>,
    /// 内层请求名（[`Request`] 的 `type` 标签）。
    #[serde(default, rename = "type")]
    pub name: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::{FrameProbe, Pending, Reply, Request};
    use crate::wire::types::{ApprovalId, ApprovalReply, ClientId, EntryId, SessionId};

    #[test]
    fn subscribe_omits_cursor_and_defaults_arbitration_flags() {
        let request = Request::Subscribe {
            session: SessionId::from("ses_1"),
            client: ClientId::from("cli_1"),
            has_local_history: false,
            takeover: false,
            since: None,
        };
        let json = serde_json::to_value(&request).expect("请求必须可序列化");
        assert_eq!(json["type"], "subscribe");
        assert!(json.get("since").is_none(), "空游标不该占字节");
        assert_eq!(
            serde_json::from_value::<Request>(json).expect("请求必须可回读"),
            request
        );

        // 老客户端不发仲裁字段时必须解成"不抢、无本地历史"——默认不抢是安全侧。
        let lean = serde_json::json!({"type": "subscribe", "session": "ses_1", "client": "cli_1"});
        assert_eq!(
            serde_json::from_value::<Request>(lean).expect("缺省仲裁字段必须能吸收"),
            request
        );
    }

    #[test]
    fn unknown_request_fails_to_parse_so_it_can_be_answered() {
        // 未知请求**必须**解析失败：只有失败才能触发 UnsupportedRequest 回应。
        // 若这里能解成某个兜底变体，等 reply_to 的对端就会永久挂着。
        let err = serde_json::from_str::<Request>(r#"{"type":"teleport","target":"mars"}"#)
            .expect_err("未知请求不得被静默吸收");
        assert!(err.to_string().contains("teleport"), "错误里要带上请求名");
    }

    #[test]
    fn probe_classifies_unknown_request() {
        let probe: FrameProbe =
            serde_json::from_str(r#"{"kind":"request","type":"teleport","target":"mars"}"#)
                .expect("探针必须吸收任意对象");
        assert_eq!(probe.kind.as_deref(), Some("request"));
        assert_eq!(probe.name.as_deref(), Some("teleport"));

        let bare: FrameProbe = serde_json::from_str("{}").expect("探针必须吸收空对象");
        assert!(bare.kind.is_none() && bare.name.is_none());
    }

    #[test]
    fn unknown_reply_degrades_instead_of_hanging() {
        let reply: Reply = serde_json::from_str(r#"{"type":"quantum_ok"}"#)
            .expect("未知回应必须降级，不能让等待者挂着");
        assert_eq!(reply, Reply::Unknown);
    }

    #[test]
    fn empty_pending_serializes_to_empty_object() {
        let json = serde_json::to_value(Pending::default()).expect("必须可序列化");
        assert_eq!(json, serde_json::json!({}));
        assert!(Pending::default().is_empty());
    }

    #[test]
    fn approval_respond_round_trips() {
        let request = Request::ApprovalRespond {
            request_id: ApprovalId::from("apr_1"),
            reply: ApprovalReply::Always,
        };
        let text = serde_json::to_string(&request).expect("必须可序列化");
        assert_eq!(
            serde_json::from_str::<Request>(&text).expect("必须可回读"),
            request
        );

        let head = Request::SetHead {
            session: SessionId::from("ses_1"),
            entry: EntryId::from("ent_9"),
        };
        let text = serde_json::to_string(&head).expect("必须可序列化");
        assert_eq!(
            serde_json::from_str::<Request>(&text).expect("必须可回读"),
            head
        );
    }
}
