//! 工具执行途中向用户要一行 stdin：oneshot-by-request-id 回环。
//!
//! # 为什么状态挂在 session 上而不是连接上
//!
//! jcode 把 stdin 的 oneshot 存在**每连接**的 `HashMap` 里
//! （`crates/jcode-app-core/src/server/client_lifecycle.rs:630-666`），连接一断整张表随栈帧
//! drop，工具侧 `response_rx.await` 收 `Err` 退出，**而子进程还卡在读 stdin**。本仓把待回答
//! 状态挂在 session 上：断连不作废、换客户端能接手、重连能重拉
//! （[`StdinGate::pending`] 存在的唯一理由）。
//!
//! # `request_id` 必须逐次唯一
//!
//! 一条命令可能连问 sudo 密码、y/N、再密码，单 pending 槽位会让回答串到别的提示上
//! （jcode `tool/bash.rs:803-812` 用计数器后缀解决同一问题）；本仓每次 [`StdinGate::ask`]
//! 都生成一个新 `request_id`，天然不会撞，也不需要额外的后缀计数器。
//!
//! # `is_password` 的理由
//!
//! 客户端据此关回显与本地历史——这不是展示层的可选修饰，泄露到 shell 历史或终端回显
//! 是这个字段要防的具体失败模式。
//!
//! 形状抄同目录 [`crate::approval::ApprovalGate`]：`Mutex<GateState>` + `oneshot` +
//! [`EventSink`]，含它处理锁中毒的方式（理由见该模块 `ApprovalGate::with_state`
//! 的文档，此处不重复）。审批回环还要处理 `always` 连锁与 `reject` 连坐，stdin 没有这两种
//! 语义——一次问只对应一次答，所以本模块比 `ApprovalGate` 薄得多。

use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

use crate::event::{AgentEvent, EventSink};

/// 一条待回答的 stdin 请求的公开信息。重连时靠它重建 UI。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingStdin {
    /// 请求 id。
    pub request_id: String,
    /// 触发请求的工具调用 id。
    pub call_id: String,
    /// 展示给用户的提示体。
    pub prompt: String,
    /// 是否应作为密码处理：客户端应关闭回显与本地历史。
    pub is_password: bool,
}

#[derive(Debug)]
struct PendingSlot {
    info: PendingStdin,
    responder: oneshot::Sender<String>,
}

#[derive(Debug, Default)]
struct GateState {
    pending: Vec<PendingSlot>,
}

/// stdin 询问回环：工具执行途中要用户输入一行文本时的 oneshot-by-request-id 等待。
#[derive(Debug)]
pub struct StdinGate {
    state: Mutex<GateState>,
    events: EventSink,
}

impl StdinGate {
    /// 建立一个回环，结算事件推给 `events`。
    #[must_use]
    pub fn new(events: EventSink) -> Self {
        Self {
            state: Mutex::new(GateState::default()),
            events,
        }
    }

    /// 当前所有待回答请求。**重连的客户端必须调它**，否则界面会漏掉询问而看起来卡死。
    #[must_use]
    pub fn pending(&self) -> Vec<PendingStdin> {
        self.with_state(|state| state.pending.iter().map(|slot| slot.info.clone()).collect())
    }

    /// 发起一次询问，等待一行答复。
    ///
    /// 返回 `None` 表示这次问不到答案了：要么被 [`StdinGate::cancel_all`] 结算为取消，
    /// 要么 `StdinGate` 本身连同其内部状态被销毁（responder 随之 drop）。两种情形对
    /// 调用方而言处理方式相同——中止当前工具调用，不是继续等。
    ///
    /// 入队与发 [`AgentEvent::StdinRequested`] 必须在同一个临界区：分离开会让
    /// [`StdinGate::reply`] 在事件广播之前就已经能命中这个 `request_id`（客户端还没收到
    /// 询问就先被答复），或者反过来在 pending 真正入队前的一次 `reply` 因查无此 id 而落空。
    pub async fn ask(&self, call_id: &str, prompt: String, is_password: bool) -> Option<String> {
        let request_id = crate::id::EntryId::generate().as_str().to_owned();
        let (responder, waiter) = oneshot::channel();
        let info = PendingStdin {
            request_id: request_id.clone(),
            call_id: call_id.to_owned(),
            prompt: prompt.clone(),
            is_password,
        };
        self.with_state(|state| {
            state.pending.push(PendingSlot { info, responder });
            self.events.emit(AgentEvent::StdinRequested {
                request_id,
                call_id: call_id.to_owned(),
                prompt,
                is_password,
            });
        });
        waiter.await.ok()
    }

    /// 答复一次询问。返回该 `request_id` 是否命中一条待回答请求。
    ///
    /// 重复回答同一个 id 返回 `false` 而不是 panic：第二次到达时槽位已经不在
    /// （第一次调用或 [`StdinGate::cancel_all`] 已经把它移除），按"迟到的重复提交"处理，
    /// 不能让一次客户端重试打垮整个会话。
    pub fn reply(&self, request_id: &str, text: String) -> bool {
        self.with_state(|state| {
            let Some(index) = state
                .pending
                .iter()
                .position(|slot| slot.info.request_id == request_id)
            else {
                return false;
            };
            let slot = state.pending.remove(index);
            // responder 是否还有人在等（`ask` 的 future 可能已被取消丢弃）不影响这里的
            // 结算逻辑：send 失败只说明没人收，不代表这次答复无效。
            let _ = slot.responder.send(text);
            self.events.emit(AgentEvent::StdinResolved {
                request_id: slot.info.request_id,
                submitted: true,
            });
            true
        })
    }

    /// 把所有待回答请求结算为"未提交"。turn 取消或运行时收尾时调用，避免调用方永久挂着。
    pub fn cancel_all(&self) {
        self.with_state(|state| {
            for slot in std::mem::take(&mut state.pending) {
                // 显式 drop：responder 一断，`ask` 里的 `waiter.await` 立刻收到 `Err`
                // 并翻译成 `None`——这是 `ask` 文档里承诺的"取消"路径的唯一触发点。
                drop(slot.responder);
                self.events.emit(AgentEvent::StdinResolved {
                    request_id: slot.info.request_id,
                    submitted: false,
                });
            }
        });
    }

    /// 在锁内跑一段不含 `.await` 的临界区。
    ///
    /// 用 `std::sync::Mutex` 而不是 tokio 的：临界区里没有 `.await`，换成异步锁只会多一次
    /// 调度。锁毒化时取回内部值继续——与 [`crate::approval::ApprovalGate::with_state`] 同一
    /// 理由：stdin 等待状态没有"半改坏"的中间态，因一个 panic 就让整个会话再也问不了
    /// stdin 是更糟的失败模式。
    fn with_state<T>(&self, apply: impl FnOnce(&mut GateState) -> T) -> T {
        let mut guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        apply(&mut guard)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use super::*;

    #[tokio::test]
    async fn ask_and_reply_round_trip() {
        let gate = StdinGate::new(EventSink::new());
        let asking = async { gate.ask("call_1", "密码？".to_owned(), true).await };
        let replying = async {
            wait_for_pending(&gate, 1).await;
            assert!(gate.reply(&gate.pending()[0].request_id, "hunter2".to_owned()));
        };
        let (answer, ()) = tokio::join!(asking, replying);
        assert_eq!(answer.as_deref(), Some("hunter2"));
    }

    #[tokio::test]
    async fn replying_twice_to_the_same_request_returns_false_not_panic() {
        let gate = StdinGate::new(EventSink::new());
        let asking = async { gate.ask("call_1", "y/N?".to_owned(), false).await };
        let replying = async {
            wait_for_pending(&gate, 1).await;
            let request_id = gate.pending()[0].request_id.clone();
            assert!(gate.reply(&request_id, "y".to_owned()));
            assert!(
                !gate.reply(&request_id, "y".to_owned()),
                "重复回答必须返回 false"
            );
        };
        let (answer, ()) = tokio::join!(asking, replying);
        assert_eq!(answer.as_deref(), Some("y"));
    }

    #[tokio::test]
    async fn replying_to_an_unknown_request_returns_false() {
        let gate = StdinGate::new(EventSink::new());
        assert!(!gate.reply("ent_nope", "irrelevant".to_owned()));
    }

    #[tokio::test]
    async fn cancel_all_unblocks_every_waiter_with_none() {
        let gate = StdinGate::new(EventSink::new());
        let asking = async { gate.ask("call_1", "a".to_owned(), false).await };
        let cancelling = async {
            wait_for_pending(&gate, 1).await;
            gate.cancel_all();
        };
        let (answer, ()) = tokio::join!(asking, cancelling);
        assert_eq!(
            answer, None,
            "cancel_all 后等待方必须拿到 None 而不是永久挂死"
        );
    }

    #[tokio::test]
    async fn pending_becomes_empty_once_every_request_is_settled() {
        let gate = StdinGate::new(EventSink::new());
        let asking = async { gate.ask("call_1", "a".to_owned(), false).await };
        let replying = async {
            wait_for_pending(&gate, 1).await;
            assert_eq!(gate.pending().len(), 1);
            let request_id = gate.pending()[0].request_id.clone();
            gate.reply(&request_id, "ok".to_owned());
        };
        let (_, ()) = tokio::join!(asking, replying);
        assert!(gate.pending().is_empty(), "结算后 pending 必须清空");
    }

    #[tokio::test]
    async fn every_settlement_broadcasts_stdin_resolved() {
        let events = EventSink::new();
        let mut stream = events.subscribe();
        let gate = StdinGate::new(events);
        let asking = async { gate.ask("call_1", "a".to_owned(), false).await };
        let replying = async {
            wait_for_pending(&gate, 1).await;
            let request_id = gate.pending()[0].request_id.clone();
            gate.reply(&request_id, "ok".to_owned());
        };
        let (_, ()) = tokio::join!(asking, replying);

        let mut saw_requested = false;
        let mut saw_resolved = false;
        while let Ok(Some(event)) =
            tokio::time::timeout(Duration::from_millis(200), stream.recv()).await
        {
            match event {
                AgentEvent::StdinRequested { .. } => saw_requested = true,
                AgentEvent::StdinResolved { submitted, .. } => {
                    saw_resolved = true;
                    assert!(submitted);
                    break;
                }
                _ => {}
            }
        }
        assert!(saw_requested && saw_resolved);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_asks_each_get_their_own_answer() {
        // 并发两条询问不能串答：各自的 request_id 必须只结算各自那条。
        let gate = Arc::new(StdinGate::new(EventSink::new()));

        let gate_a = Arc::clone(&gate);
        let asking_a =
            tokio::spawn(async move { gate_a.ask("call_1", "sudo 密码？".to_owned(), true).await });
        let gate_b = Arc::clone(&gate);
        let asking_b =
            tokio::spawn(async move { gate_b.ask("call_2", "确认删除？".to_owned(), false).await });

        wait_for_pending(&gate, 2).await;
        let pending = gate.pending();
        let slot_a = pending
            .iter()
            .find(|slot| slot.call_id == "call_1")
            .expect("call_1 应该在 pending 里");
        let slot_b = pending
            .iter()
            .find(|slot| slot.call_id == "call_2")
            .expect("call_2 应该在 pending 里");
        assert!(gate.reply(&slot_b.request_id, "no".to_owned()));
        assert!(gate.reply(&slot_a.request_id, "hunter2".to_owned()));

        let answer_a = asking_a.await.expect("task 未 panic");
        let answer_b = asking_b.await.expect("task 未 panic");
        assert_eq!(answer_a.as_deref(), Some("hunter2"));
        assert_eq!(answer_b.as_deref(), Some("no"));
    }

    /// 轮询等待队列里出现至少 `count` 条待回答请求。
    /// 直接读会有竞态：`ask` 入队与轮询方读取不在同一个执行步内。
    async fn wait_for_pending(gate: &StdinGate, count: usize) -> Vec<PendingStdin> {
        loop {
            let pending = gate.pending();
            if pending.len() >= count {
                return pending;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }
}
