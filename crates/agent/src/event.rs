//! Agent 运行时向订阅者推送的事件流。
//!
//! # 慢消费者：不断流，推 [`AgentEvent::Resync`]
//!
//! 这是 `plans/runtime-boundary/implementation.md:52-54` 已定的契约，也是对 opencode 两代
//! 都做错的直接修正：v1 用 `Queue.unbounded` 让一个卡住的客户端把 daemon 撑爆
//! （`packages/opencode/src/server/routes/instance/httpapi/handlers/event.ts:31`），
//! v2 改成 `Queue.dropping(256)` 但**溢出即打挂整条流**（`packages/core/src/event.ts:152-166`）。
//! 两者都不可接受：前者是内存故障，后者把"掉了几帧 UI 增量"升级成"连接断开"。
//!
//! 本仓用有界 `broadcast`，订阅侧把 `RecvError::Lagged(n)` 转成一条
//! [`AgentEvent::Resync`]：流不断，客户端按游标补拉自己缺的那段。
//!
//! # 持久化绝不走这条通道
//!
//! broadcast 会丢事件，所以**只有可丢的东西**才走这里（UI 增量、进度）。落盘由运行时内联
//! 完成，是单一权威写入方。抄源 oh-my-pi 的拓扑（`packages/coding-agent/src/session/agent-session.ts:1522`：
//! 内核事件流只有一个订阅者，由它负责持久化再向外扇出），避免"UI 掉帧"与"历史丢失"共用
//! 同一个失败模式。

use tokio::sync::broadcast;

use crate::id::EntryId;
use crate::session::message::{StoredMessage, StoredUsage};

/// 事件通道容量。
///
/// 取值前提：单个 turn 的事件量由 token 速率决定（约 10²/s），256 足够覆盖一次
/// 网络抖动或一帧渲染的落后；再大只是把"客户端已经跟不上"这个事实推迟暴露。
/// 溢出不是故障——它会被翻译成 [`AgentEvent::Resync`]，客户端补拉即可。
pub const EVENT_CHANNEL_CAPACITY: usize = 256;

/// 工具执行过程中的增量输出。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolProgress {
    /// 一段新的输出文本。
    Chunk {
        /// 新增文本。
        text: String,
    },
    /// 工具自报的一行状态（例如"已扫描 120 个文件"）。
    Status {
        /// 状态文本。
        text: String,
    },
}

/// 运行时事件。
///
/// 这是**可丢**的 UI 增量流，不是事实来源。任何需要精确重建的状态都要从会话存储读。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentEvent {
    /// 一次 turn 开始。
    TurnStart {
        /// 触发本次 turn 的用户消息 id。
        user_entry: EntryId,
    },
    /// 助手消息开始。
    ///
    /// `entry` 是**开流前预分配**的条目 id，随后的每一条增量与最终的
    /// [`AgentEvent::MessageEnd`] 都带同一个 id。预分配而不是等落盘再分配，是为了让中途接入
    /// 或刚从 [`AgentEvent::Resync`] 恢复的客户端也能立刻把增量归属到正确的消息——
    /// 若增量不带 id，客户端要等到 `MessageEnd` 才知道刚才那串文本属于谁。
    MessageStart {
        /// 预分配的消息 id。
        entry: EntryId,
    },
    /// 助手文本增量。
    TextDelta {
        /// 消息 id，与本条消息的 `MessageStart` 相同。
        entry: EntryId,
        /// 内容块下标。
        index: usize,
        /// 新增文本。
        delta: String,
    },
    /// 思考增量。
    ThinkingDelta {
        /// 消息 id，与本条消息的 `MessageStart` 相同。
        entry: EntryId,
        /// 内容块下标。
        index: usize,
        /// 新增文本。
        delta: String,
    },
    /// 工具调用参数的原始 JSON 增量。
    ///
    /// **必须是原始 partial JSON**，不能换成"已解析参数"：解析受节流窗口影响会滞后于流，
    /// bash 的内联环境变量赋值可能要到 JSON 对象闭合前一刻才可见
    /// （oh-my-pi `packages/coding-agent/src/modes/controllers/tool-args-reveal.ts:10-14`）。
    ToolCallDelta {
        /// 消息 id，与本条消息的 `MessageStart` 相同。
        entry: EntryId,
        /// 内容块下标。
        index: usize,
        /// 调用 id；首帧未给出时为空串。
        call_id: String,
        /// 新增 JSON 片段。
        delta: String,
    },
    /// 助手消息结束，携带完整消息。
    MessageEnd {
        /// 消息 id。
        entry: EntryId,
        /// 完整消息。
        message: Box<StoredMessage>,
        /// 本次请求的用量。
        usage: StoredUsage,
    },
    /// 一个工具开始执行。
    ToolStart {
        /// 调用 id。
        call_id: String,
        /// 工具名。
        name: String,
    },
    /// 工具执行中的增量输出。
    ToolProgress {
        /// 调用 id。
        call_id: String,
        /// 增量。
        progress: ToolProgress,
    },
    /// 一个工具执行结束。
    ToolEnd {
        /// 调用 id。
        call_id: String,
        /// 结果消息 id。
        entry: EntryId,
        /// 是否失败。
        is_error: bool,
    },
    /// 需要用户审批。
    ApprovalRequested {
        /// 审批请求 id。
        request_id: String,
        /// 调用 id。
        call_id: String,
        /// 展示给用户的提示体。
        prompt: String,
    },
    /// 审批已结算——**任何**一条待审批消失都必须有这条事件，客户端靠它移除 UI。
    ApprovalResolved {
        /// 审批请求 id。
        request_id: String,
        /// 是否放行。
        approved: bool,
    },
    /// 上下文被压缩。
    Compacted {
        /// 压缩条目 id。
        entry: EntryId,
    },
    /// 需要用户输入一行 stdin。
    StdinRequested {
        /// 请求 id。
        request_id: String,
        /// 触发请求的工具调用 id。
        call_id: String,
        /// 展示给用户的提示体。
        prompt: String,
        /// 是否应作为密码处理：客户端应关闭回显与本地历史。
        is_password: bool,
    },
    /// stdin 请求已结算——**任何**一条待回答请求消失都必须有这条事件，客户端靠它移除 UI。
    StdinResolved {
        /// 请求 id。
        request_id: String,
        /// 是否提交了答案（`false` = 取消 / 未提交）。
        submitted: bool,
    },
    /// 一次 turn 结束。
    TurnEnd,
    /// 本次 turn 以错误告终。
    Failed {
        /// 面向用户的错误文本。
        message: String,
    },
    /// 订阅者落后，中间丢了 `dropped` 条事件。
    ///
    /// **流没有断**。收到它的客户端应按自己的游标从会话存储补拉，然后继续消费。
    Resync {
        /// 被丢弃的事件条数。
        dropped: u64,
    },
}

/// 事件广播端。
#[derive(Debug, Clone)]
pub struct EventSink {
    sender: broadcast::Sender<AgentEvent>,
}

impl Default for EventSink {
    fn default() -> Self {
        Self::new()
    }
}

impl EventSink {
    /// 建立一个新的广播端。
    #[must_use]
    pub fn new() -> Self {
        let (sender, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        Self { sender }
    }

    /// 推送一条事件。没有订阅者时静默丢弃——运行时不因为没人看而停下。
    pub fn emit(&self, event: AgentEvent) {
        let _ = self.sender.send(event);
    }

    /// 新增一个订阅者。
    #[must_use]
    pub fn subscribe(&self) -> EventStream {
        EventStream {
            receiver: self.sender.subscribe(),
        }
    }
}

/// 事件订阅端。
#[derive(Debug)]
pub struct EventStream {
    receiver: broadcast::Receiver<AgentEvent>,
}

impl EventStream {
    /// 取下一条事件；广播端关闭时返回 `None`。
    ///
    /// 落后时**不返回错误、不结束流**，而是给出一条 [`AgentEvent::Resync`]。
    pub async fn recv(&mut self) -> Option<AgentEvent> {
        match self.receiver.recv().await {
            Ok(event) => Some(event),
            Err(broadcast::error::RecvError::Lagged(dropped)) => {
                Some(AgentEvent::Resync { dropped })
            }
            Err(broadcast::error::RecvError::Closed) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn events_reach_every_subscriber() {
        let sink = EventSink::new();
        let mut first = sink.subscribe();
        let mut second = sink.subscribe();
        sink.emit(AgentEvent::TurnEnd);
        assert_eq!(first.recv().await, Some(AgentEvent::TurnEnd));
        assert_eq!(second.recv().await, Some(AgentEvent::TurnEnd));
    }

    #[tokio::test]
    async fn a_slow_subscriber_gets_resync_instead_of_a_broken_stream() {
        let sink = EventSink::new();
        let mut slow = sink.subscribe();
        for _ in 0..(EVENT_CHANNEL_CAPACITY + 10) {
            sink.emit(AgentEvent::TurnEnd);
        }
        let Some(AgentEvent::Resync { dropped }) = slow.recv().await else {
            panic!("落后的订阅者必须先收到 Resync");
        };
        assert_eq!(dropped, 10);
        // 关键不变量：流没有断，后续事件照常送达。
        assert_eq!(slow.recv().await, Some(AgentEvent::TurnEnd));
    }

    #[tokio::test]
    async fn dropping_the_sink_ends_the_stream() {
        let sink = EventSink::new();
        let mut stream = sink.subscribe();
        drop(sink);
        assert_eq!(stream.recv().await, None);
    }

    #[test]
    fn emitting_without_subscribers_is_not_an_error() {
        let sink = EventSink::new();
        sink.emit(AgentEvent::TurnEnd);
    }
}
