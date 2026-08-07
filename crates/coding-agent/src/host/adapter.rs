//! 领域类型 ↔ wire 类型的互转。
//!
//! # 两侧都不得绕过 `zcode-protocol`
//!
//! `zcode_agent::AgentEvent` 是运行时的可丢 UI 增量流，`zcode_protocol::wire::Event` 是线上
//! 协议——两者形状故意不同（见 `crates/protocol/src/wire/types.rs` 模块文档），互转只在这里
//! 发生一次。别的模块不许自己写第二份字段搬运，那是"新增一套并行约定"。
//!
//! # 穷尽性哨兵
//!
//! 本文件里凡是对领域枚举做 `match`，一律逐个变体列出、不写 `_ =>` 兜底：
//! `zcode-agent` 新增一个 `AgentEvent` 变体时，这里必须编译失败，而不是静默丢事件。
//!
//! # `ApprovalRequested` 为什么要多传一个 `ApprovalGate`
//!
//! [`zcode_agent::AgentEvent::ApprovalRequested`] 只带 `request_id` / `call_id` / `prompt`
//! 三个字段——它是广播流上的轻量通知，`tool_name` / `scope` 这两个 wire 侧
//! [`zcode_protocol::wire::PendingApproval`] 必填的字段并不在其中。真正的全量信息在
//! [`zcode_agent::ApprovalGate::pending`] 里：`ApprovalGate::ask` 在同一个临界区里先入队
//! 再发事件（`crates/agent/src/approval.rs` 的文档），所以事件到达时该请求几乎总能在
//! `pending()` 里查到。极端竞态下（查到之前就被结算）查不到，此时**不伪造**
//! `tool_name`/`scope`，而是跳过这条事件——客户端马上会收到对应的
//! `Event::ApprovalResolved`，状态不会错乱，只是这一条通知被丢弃。

use zcode_agent::event::ToolProgress as DomainToolProgress;
use zcode_agent::{
    AgentEvent, ApprovalGate, ApprovalMode as DomainApprovalMode,
    ApprovalReply as DomainApprovalReply, CompactionReason as DomainCompactionReason,
    DisplayRole as DomainDisplayRole, EntryId as DomainEntryId, EntryKind as DomainEntryKind,
    PendingApproval as DomainPendingApproval, PendingStdin as DomainPendingStdin, SessionEntry,
    SessionTree, StoredAssistantContent, StoredImage, StoredMessage, StoredStopReason,
    StoredToolResultContent, StoredUsage, StoredUserContent,
};
use zcode_protocol::wire::{
    ApprovalMode as WireApprovalMode, ApprovalReply as WireApprovalReply, AssistantContent, CallId,
    CompactionReason as WireCompactionReason, DisplayRole as WireDisplayRole, Entry, EntryId,
    EntryKind as WireEntryKind, Event, Image, Message, PendingApproval, PendingStdin, SessionId,
    SessionSummary, StopReason, ToolProgress, ToolResultContent, Usage, UserContent,
};

/// 域 [`zcode_agent::EntryId`] → wire [`EntryId`]。
pub(crate) fn entry_id(id: &DomainEntryId) -> EntryId {
    EntryId::from(id.as_str())
}

/// 域调用 id（裸 `String`）→ wire [`CallId`]。
pub(crate) fn call_id(id: &str) -> CallId {
    CallId::from(id)
}

/// 内容块下标：域侧 `usize` → wire `u32`。
///
/// 单条消息的内容块数不可能逼近 `u32::MAX`（那是 40 亿块），溢出只可能是上游给错了值；
/// 按铁律不允许 `as`，也不允许在库代码里 `unwrap`，所以钳到 `u32::MAX` 并记录一条
/// `warn!`——好过让整条事件流因为一次不可能发生的溢出而失败。
pub(crate) fn block_index(index: usize) -> u32 {
    u32::try_from(index).unwrap_or_else(|_| {
        tracing::warn!(index, "内容块下标超出 u32 范围，已钳到上限");
        u32::MAX
    })
}

/// 内联图片：域 [`StoredImage`] → wire [`Image`]。
fn image_to_wire(image: &StoredImage) -> Image {
    Image {
        media_type: image.media_type.clone(),
        data: image.data.clone(),
    }
}

/// 用户内容块：域 [`StoredUserContent`] → wire [`UserContent`]。
fn user_content_to_wire(block: &StoredUserContent) -> UserContent {
    match block {
        StoredUserContent::Text { text } => UserContent::Text { text: text.clone() },
        StoredUserContent::Image { image } => UserContent::Image {
            image: image_to_wire(image),
        },
    }
}

/// 助手内容块：域 [`StoredAssistantContent`] → wire [`AssistantContent`]。
fn assistant_content_to_wire(block: &StoredAssistantContent) -> AssistantContent {
    match block {
        StoredAssistantContent::Text { text } => AssistantContent::Text { text: text.clone() },
        StoredAssistantContent::Thinking { text, signature } => AssistantContent::Thinking {
            text: text.clone(),
            signature: signature.clone(),
        },
        StoredAssistantContent::RedactedThinking { data } => {
            AssistantContent::RedactedThinking { data: data.clone() }
        }
        StoredAssistantContent::ToolCall {
            id,
            name,
            arguments,
        } => AssistantContent::ToolCall {
            id: call_id(id),
            name: name.clone(),
            arguments: arguments.clone(),
        },
    }
}

/// 工具结果内容块：域 [`StoredToolResultContent`] → wire [`ToolResultContent`]。
fn tool_result_content_to_wire(block: &StoredToolResultContent) -> ToolResultContent {
    match block {
        StoredToolResultContent::Text { text } => ToolResultContent::Text { text: text.clone() },
        StoredToolResultContent::Image { image } => ToolResultContent::Image {
            image: image_to_wire(image),
        },
    }
}

/// token 用量：域 [`StoredUsage`] → wire [`Usage`]。两者字段同形，逐个搬运。
pub(crate) fn usage_to_wire(usage: StoredUsage) -> Usage {
    Usage {
        input: usage.input,
        output: usage.output,
        cache_read: usage.cache_read,
        cache_write: usage.cache_write,
        reasoning: usage.reasoning,
    }
}

/// 停止原因：域 [`StoredStopReason`] → wire [`StopReason`]。
fn stop_reason_to_wire(reason: StoredStopReason) -> StopReason {
    match reason {
        StoredStopReason::Stop => StopReason::Stop,
        StoredStopReason::Length => StopReason::Length,
        StoredStopReason::ToolUse => StopReason::ToolUse,
        StoredStopReason::Error => StopReason::Error,
        StoredStopReason::Aborted => StopReason::Aborted,
    }
}

/// UI 展示角色：域 [`DomainDisplayRole`] → wire [`WireDisplayRole`]。
fn display_role_to_wire(role: DomainDisplayRole) -> WireDisplayRole {
    match role {
        DomainDisplayRole::System => WireDisplayRole::System,
        DomainDisplayRole::BackgroundTask => WireDisplayRole::BackgroundTask,
    }
}

/// 一条落盘态消息：域 [`StoredMessage`] → wire [`Message`]。
pub(crate) fn message_to_wire(message: &StoredMessage) -> Message {
    match message {
        StoredMessage::User {
            content,
            display_role,
        } => Message::User {
            content: content.iter().map(user_content_to_wire).collect(),
            display_role: display_role.map(display_role_to_wire),
        },
        StoredMessage::Assistant {
            content,
            model,
            usage,
            stop_reason,
        } => Message::Assistant {
            content: content.iter().map(assistant_content_to_wire).collect(),
            model: model.clone(),
            usage: usage_to_wire(*usage),
            stop_reason: stop_reason_to_wire(*stop_reason),
        },
        StoredMessage::ToolResult {
            tool_call_id,
            tool_name,
            content,
            is_error,
        } => Message::ToolResult {
            tool_call_id: call_id(tool_call_id),
            tool_name: tool_name.clone(),
            content: content.iter().map(tool_result_content_to_wire).collect(),
            is_error: *is_error,
        },
    }
}

/// 压缩触发原因：域 [`DomainCompactionReason`] → wire [`WireCompactionReason`]。
fn compaction_reason_to_wire(reason: DomainCompactionReason) -> WireCompactionReason {
    match reason {
        DomainCompactionReason::Threshold => WireCompactionReason::Threshold,
        DomainCompactionReason::Overflow => WireCompactionReason::Overflow,
        DomainCompactionReason::Manual => WireCompactionReason::Manual,
    }
}

/// 一条条目内容：域 [`DomainEntryKind`] → wire [`WireEntryKind`]。
fn entry_kind_to_wire(kind: &DomainEntryKind) -> WireEntryKind {
    match kind {
        DomainEntryKind::SessionInit { cwd, model } => WireEntryKind::SessionInit {
            cwd: cwd.clone(),
            model: model.clone(),
        },
        DomainEntryKind::Message { message } => WireEntryKind::Message {
            message: message_to_wire(message),
        },
        DomainEntryKind::ModelChange { model } => WireEntryKind::ModelChange {
            model: model.clone(),
        },
        DomainEntryKind::TitleChange { title } => WireEntryKind::TitleChange {
            title: title.clone(),
        },
        DomainEntryKind::Compaction {
            summary,
            first_kept,
            reason,
        } => WireEntryKind::Compaction {
            summary: summary.clone(),
            first_kept: first_kept.as_ref().map(entry_id),
            reason: compaction_reason_to_wire(*reason),
        },
    }
}

/// 一个会话树节点：域 [`SessionEntry`] → wire [`Entry`]。
pub(crate) fn entry_to_wire(entry: &SessionEntry) -> Entry {
    Entry {
        id: entry_id(&entry.id),
        parent_id: entry.parent_id.as_ref().map(entry_id),
        timestamp_ms: entry.timestamp_ms,
        kind: entry_kind_to_wire(&entry.kind),
    }
}

/// 会话摘要：从 [`SessionTree`] 现读现算，不缓存。
///
/// `created_ms` 取根到 head 路径的第一条（恒为根）时间戳，`updated_ms` 取最后一条
/// （恒为 head）；路径不可能为空（[`SessionTree::branch`] 至少含根条目），因此
/// 兜底值只在防御性编程意义上存在。
pub(crate) fn session_summary(tree: &SessionTree) -> SessionSummary {
    let path = tree.branch();
    let created_ms = path.first().map_or(0, |entry| entry.timestamp_ms);
    let updated_ms = path.last().map_or(created_ms, |entry| entry.timestamp_ms);
    SessionSummary {
        id: SessionId::from(tree.session_id().as_str()),
        title: tree.title().map(str::to_owned),
        cwd: tree.cwd().to_owned(),
        model: tree.model().to_owned(),
        created_ms,
        updated_ms,
    }
}

/// 一条待审批请求：域 [`DomainPendingApproval`] → wire [`PendingApproval`]。
pub(crate) fn pending_approval_to_wire(pending: &DomainPendingApproval) -> PendingApproval {
    PendingApproval {
        request_id: pending.request_id.as_str().into(),
        call_id: call_id(&pending.call_id),
        tool_name: pending.tool_name.clone(),
        scope: pending.scope.clone(),
        prompt: pending.prompt.clone(),
    }
}

/// 一条待 stdin 请求：域 [`DomainPendingStdin`] → wire [`PendingStdin`]。
pub(crate) fn pending_stdin_to_wire(pending: &DomainPendingStdin) -> PendingStdin {
    PendingStdin {
        request_id: pending.request_id.as_str().into(),
        call_id: call_id(&pending.call_id),
        prompt: pending.prompt.clone(),
        is_password: pending.is_password,
    }
}

/// 工具执行增量：域 [`DomainToolProgress`] → wire [`ToolProgress`]。
fn tool_progress_to_wire(progress: DomainToolProgress) -> ToolProgress {
    match progress {
        DomainToolProgress::Chunk { text } => ToolProgress::Chunk { text },
        DomainToolProgress::Status { text } => ToolProgress::Status { text },
    }
}

/// 运行时事件 → wire 推送。
///
/// 返回 `None` 仅在 `ApprovalRequested` 撞上极端竞态（见模块文档）时出现——调用方应当
/// 静默跳过，不回任何错误帧：这条事件本就是可丢的 UI 增量，不是事实来源。
// 这是一个对 17 个 `AgentEvent` 变体的**穷尽** match，没有 `_` 兜底——那正是"领域侧加变体、
// adapter 立刻编译失败"这条护栏（模块文档"漂移风险由穷尽 match 兜住"）。拆成几个子函数
// 就必须给每个子函数补 fallthrough 分支，护栏当场失效。行数是这条护栏的代价，不是坏味道。
#[expect(clippy::too_many_lines, reason = "穷尽 match 不可拆，见上方注释")]
pub(crate) fn agent_event_to_wire(
    session: &SessionId,
    event: AgentEvent,
    approvals: &ApprovalGate,
) -> Option<Event> {
    let event = match event {
        AgentEvent::TurnStart { user_entry } => Event::TurnStart {
            session: session.clone(),
            user_entry: entry_id(&user_entry),
        },
        AgentEvent::MessageStart { entry } => Event::MessageStart {
            session: session.clone(),
            entry: entry_id(&entry),
        },
        AgentEvent::TextDelta {
            entry,
            index,
            delta,
        } => Event::TextDelta {
            session: session.clone(),
            entry: entry_id(&entry),
            index: block_index(index),
            delta,
        },
        AgentEvent::ThinkingDelta {
            entry,
            index,
            delta,
        } => Event::ThinkingDelta {
            session: session.clone(),
            entry: entry_id(&entry),
            index: block_index(index),
            delta,
        },
        AgentEvent::ToolCallDelta {
            entry,
            index,
            call_id: cid,
            delta,
        } => Event::ToolCallDelta {
            session: session.clone(),
            entry: entry_id(&entry),
            index: block_index(index),
            call_id: call_id(&cid),
            delta,
        },
        AgentEvent::MessageEnd {
            entry,
            message,
            usage,
        } => Event::MessageEnd {
            session: session.clone(),
            entry: entry_id(&entry),
            message: Box::new(message_to_wire(&message)),
            usage: usage_to_wire(usage),
        },
        AgentEvent::ToolStart { call_id: cid, name } => Event::ToolStart {
            session: session.clone(),
            call_id: call_id(&cid),
            name,
        },
        AgentEvent::ToolProgress {
            call_id: cid,
            progress,
        } => Event::ToolProgress {
            session: session.clone(),
            call_id: call_id(&cid),
            progress: tool_progress_to_wire(progress),
        },
        AgentEvent::ToolEnd {
            call_id: cid,
            entry,
            is_error,
        } => Event::ToolEnd {
            session: session.clone(),
            call_id: call_id(&cid),
            entry: entry_id(&entry),
            is_error,
        },
        AgentEvent::ApprovalRequested { request_id, .. } => {
            let pending = approvals
                .pending()
                .into_iter()
                .find(|candidate| candidate.request_id == request_id)?;
            Event::ApprovalRequested {
                session: session.clone(),
                pending: pending_approval_to_wire(&pending),
            }
        }
        AgentEvent::ApprovalResolved {
            request_id,
            approved,
        } => Event::ApprovalResolved {
            session: session.clone(),
            request_id: request_id.as_str().into(),
            approved,
        },
        AgentEvent::Compacted { entry } => Event::Compacted {
            session: session.clone(),
            entry: entry_id(&entry),
        },
        AgentEvent::StdinRequested {
            request_id,
            call_id: cid,
            prompt,
            is_password,
        } => Event::StdinRequested {
            session: session.clone(),
            pending: PendingStdin {
                request_id: request_id.as_str().into(),
                call_id: call_id(&cid),
                prompt,
                is_password,
            },
        },
        AgentEvent::StdinResolved {
            request_id,
            submitted,
        } => Event::StdinResolved {
            session: session.clone(),
            request_id: request_id.as_str().into(),
            submitted,
        },
        AgentEvent::TurnEnd => Event::TurnEnd {
            session: session.clone(),
        },
        AgentEvent::Failed { message } => Event::Failed {
            session: session.clone(),
            message,
        },
        AgentEvent::Resync { dropped } => Event::Resync {
            session: session.clone(),
            dropped,
        },
    };
    Some(event)
}

/// wire 审批答复 → 域答复。
///
/// `Unknown`（对端比本端新）按 [`DomainApprovalReply::Reject`] 结算——认不出来的授权
/// 一律不放行，理由见 `zcode_protocol::wire::types::ApprovalReply` 的文档。
pub(crate) fn approval_reply_from_wire(reply: WireApprovalReply) -> DomainApprovalReply {
    match reply {
        WireApprovalReply::Once => DomainApprovalReply::Once,
        WireApprovalReply::Always => DomainApprovalReply::Always,
        WireApprovalReply::Reject | WireApprovalReply::Unknown => DomainApprovalReply::Reject,
    }
}

/// wire 审批模式 → 域模式。
///
/// `Unknown` 与 `AlwaysAsk` 结算成同一个值是**有意的**：认不出来的模式按最保守处理，
/// 与 `Tier::Unknown` 按 `Exec` 渲染同一个 fail-safe 方向。合并成一条 `match` 分支
/// 只是形式，语义上它们是两件事——新增模式时不要照抄这条合并，先想清楚保守方向在哪。
pub(crate) fn approval_mode_from_wire(mode: WireApprovalMode) -> DomainApprovalMode {
    match mode {
        WireApprovalMode::AlwaysAsk | WireApprovalMode::Unknown => DomainApprovalMode::AlwaysAsk,
        WireApprovalMode::Write => DomainApprovalMode::Write,
        WireApprovalMode::Yolo => DomainApprovalMode::Yolo,
    }
}

/// 把 `Request::Prompt` 的内容块规约成 `AgentRuntime::run_turn` 能接受的纯文本。
///
/// `run_turn(user_text: impl Into<String>)` 只接受一个字符串——`zcode-agent` 当前不支持
/// 结构化多块用户输入（无法从外部构造带图片的 `StoredMessage::User` 并喂给 turn 循环）。
/// 这是已知限制：多个文本块用空行拼接，图片块目前会被丢弃并记一条 `warn!`。
pub(crate) fn prompt_text_from_wire(content: &[UserContent]) -> String {
    let mut text = String::new();
    for block in content {
        match block {
            UserContent::Text { text: part } => {
                if !text.is_empty() {
                    text.push_str("\n\n");
                }
                text.push_str(part);
            }
            UserContent::Image { .. } => {
                tracing::warn!(
                    "Request::Prompt 携带图片内容块，但 AgentRuntime::run_turn 只接受纯文本，已丢弃"
                );
            }
        }
    }
    text
}
