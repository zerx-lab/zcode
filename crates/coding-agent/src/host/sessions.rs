//! 会话表：`SessionId -> SessionHandle`。
//!
//! # 为什么是 actor，不是 `tokio::sync::Mutex<AgentRuntime>`
//!
//! `AgentRuntime::run_turn` 要 `&mut self`，且一跑可能是几秒到几分钟（多轮工具调用）。
//! 不管用 actor 任务串行拥有它，还是用 `Mutex<AgentRuntime>` 让调用方轮流 `.lock().await`，
//! **可读性上的限制是同一个**：`AgentRuntime` 没有暴露任何"只读不冲突"的访问面，
//! `store()`/`store_mut()`/`config_mut()`/`compact()` 全部要求独占访问，所以无论选哪种
//! 方案，`HistoryFetch`/`Subscribe` 这类读请求在一次 `run_turn` 跑完之前都要排队——
//! 这是 `AgentRuntime` 当前 API 形状带来的固有代价，不是本模块能绕开的。
//!
//! 两种方案真正的区别在别处：**谁的生命周期与连接绑定**。
//!
//! - `Mutex<AgentRuntime>` 要求每个调用点自己记得 `tokio::spawn` 一个detached 任务去跑
//!   `run_turn`，否则默认写法会把 `.lock().await.run_turn(..).await` 直接摆在处理某条
//!   连接请求的 future 里——连接一断，这个 future 被 drop，turn 也跟着死。"turn 属于
//!   session 而不属于连接"这条不变量因此要靠**调用纪律**维持，写代码的人必须每次都记得
//!   detach。
//! - actor 任务在会话创建 / 首次加载时**只 spawn 一次**，此后所有交互都是"往 channel 里
//!   丢一条命令"。没有任何调用路径能不小心把 `run_turn` 摆进一个跟连接同生共死的
//!   future——channel 的发送端与接收端在结构上就是分离的两个任务，不变量由类型/模块
//!   结构保证，不依赖人记得。
//!
//! 因此选 actor。

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use tokio::sync::{Mutex as AsyncMutex, mpsc, oneshot};
use zcode_agent::{
    AgentError, AgentEvent, AgentRuntime, ApprovalGate, ApprovalMode, CancelRegistry, EntryId,
    EntryKind, SessionId, SessionStore, StdinGate, StoreError, StoredMessage, TurnConfig,
};
use zcode_protocol::wire;

use crate::host::adapter;
use crate::host::{HostDeps, HostError};

/// 一个会话级控制命令的排队上限。
///
/// 控制命令（切模型、改标题、压缩……）远比事件流稀疏；有界只是为了防止一个卡死的
/// actor 让调用方无限堆积待发送命令，取值不需要与吞吐挂钩，比照 tokio 惯用的小容量
/// 控制通道。
const SESSION_COMMAND_CAPACITY: usize = 32;

/// 宿主层合成事件（`Event::SessionUpdated`）广播通道的容量。
///
/// 这类事件只在 `SetHead`/`SetModel`/`SetTitle` 成功后各发一条，频率比流式增量低两个
/// 数量级；取一个与 [`SESSION_COMMAND_CAPACITY`] 同量级的小容量即可，容量耗尽时订阅端
/// 会在下一次 `recv()` 收到 `RecvError::Lagged` 并自行转译成 `Event::Resync`，不影响
/// 正确性，只是让客户端多补一次 `HistoryFetch`。
const SESSION_META_CHANNEL_CAPACITY: usize = 32;

/// 发给会话 actor 的命令。每个变体自带一个 oneshot 回程通道。
enum SessionCommand {
    /// 投入一轮用户输入。`started` 在观测到 [`AgentEvent::TurnStart`]
    /// （或 turn 在那之前就失败）时被结算一次，不等整个 turn 跑完。
    Prompt {
        text: String,
        started: oneshot::Sender<Result<EntryId, AgentError>>,
    },
    /// 把 head 指到另一条已存在的条目。
    SetHead {
        entry: EntryId,
        reply: oneshot::Sender<bool>,
    },
    /// 追加一条 `ModelChange`。
    SetModel {
        model: String,
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
    /// 追加一条 `TitleChange`。
    SetTitle {
        title: String,
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
    /// 切换审批模式。对**下一次** `run_turn` 生效。
    SetApprovalMode {
        mode: ApprovalMode,
        reply: oneshot::Sender<()>,
    },
    /// 立刻压缩一次。历史不够时是 no-op（仍然 `Ok(())`）——调用方要判定"是否真的压了"
    /// 应该看有没有收到 `Event::Compacted`，不要靠这个回应的 `Ok`/`Err`。
    Compact {
        reply: oneshot::Sender<Result<(), AgentError>>,
    },
    /// 取一份可直接编码上线的快照：摘要 + head + `since` 之后的条目。
    Snapshot {
        since: Option<EntryId>,
        reply: oneshot::Sender<SessionSnapshot>,
    },
}

/// [`SessionCommand::Snapshot`] 的回应，字段已经是 wire 形状。
pub(crate) struct SessionSnapshot {
    /// 会话摘要。
    pub(crate) summary: wire::SessionSummary,
    /// 当前 head。
    pub(crate) head: wire::EntryId,
    /// `since` 之后（按追加顺序）的条目。
    pub(crate) entries: Vec<wire::Entry>,
}

/// 会话是否被另一个客户端占用的仲裁结果。
///
/// **只是接口预留**：本模块只实现"默认不抢、`takeover` 显式抢"这一条最基本策略
/// （逻辑持有者字段，进程内有效）。跨进程/跨 daemon 重启的持有权持久化不在本模块
/// 职责内，由 `HostDaemon`（`host::daemon`）按需在这个方法之上扩展。
pub(crate) enum Claim {
    /// 仲裁通过，调用方可以继续走 Subscribed 流程。
    Granted,
    /// 被另一个客户端占用，且本次请求没有要求接管。
    Busy {
        /// 当前占用者的客户端实例 id。
        holder: wire::ClientId,
    },
}

/// 一个存活会话的句柄。
///
/// 除 `commands`（进 actor）之外的字段都是从构造 actor 之前的 [`AgentRuntime`]
/// 克隆出来的：[`zcode_agent::EventSink`] 可以随意 `subscribe()`，
/// [`ApprovalGate`]/[`StdinGate`] 内部自带锁，三者都不需要经过 actor 的命令队列，
/// 因此审批/输入回环与 pending 重拉在 actor 正忙着跑一个长 turn 时依然是即时的。
pub(crate) struct SessionHandle {
    /// 运行时事件广播端：客户端从这里订阅 [`zcode_agent::AgentEvent`]。
    pub(crate) events: zcode_agent::EventSink,
    /// 审批询问回环。
    pub(crate) approvals: Arc<ApprovalGate>,
    /// stdin 询问回环。
    pub(crate) stdin: Arc<StdinGate>,
    /// 宿主层合成事件的广播端。
    ///
    /// 目前只用来发 `Event::SessionUpdated`：`AgentEvent` 没有对应变体——标题/模型/
    /// head 变更是宿主层直接改会话元数据，不是运行时领域事件，走不了
    /// `zcode_agent::EventSink` 那条线。慢消费者语义与它一致：满了不阻塞发送方，
    /// 订阅端收到 `RecvError::Lagged` 时自己转译成 `Event::Resync`（客户端读路径
    /// 只认 `Event::Resync`，不关心丢的是哪种事件），流不断。
    meta: tokio::sync::broadcast::Sender<wire::Event>,
    commands: mpsc::Sender<SessionCommand>,
    /// 当前逻辑持有者。见 [`Claim`] 的文档。
    holder: Mutex<Option<wire::ClientId>>,
}

impl SessionHandle {
    /// 订阅宿主层合成事件（目前只有 `SessionUpdated`）。
    pub(crate) fn subscribe_meta(&self) -> tokio::sync::broadcast::Receiver<wire::Event> {
        self.meta.subscribe()
    }

    /// 发一条宿主层合成事件。没有订阅者时静默丢弃，与 `EventSink::emit` 同一个哲学。
    pub(crate) fn publish_meta(&self, event: wire::Event) {
        let _ = self.meta.send(event);
    }

    /// 仲裁一次 `Subscribe`。
    pub(crate) fn claim(&self, client: &wire::ClientId, takeover: bool) -> Claim {
        let mut holder = self.holder.lock().unwrap_or_else(PoisonError::into_inner);
        match holder.as_ref() {
            Some(current) if current != client && !takeover => Claim::Busy {
                holder: current.clone(),
            },
            _ => {
                *holder = Some(client.clone());
                Claim::Granted
            }
        }
    }

    /// 投入一轮用户输入，返回刚写入的用户消息条目 id。
    pub(crate) async fn prompt(&self, text: String) -> Result<EntryId, HostError> {
        let (started, wait) = oneshot::channel();
        self.commands
            .send(SessionCommand::Prompt { text, started })
            .await
            .map_err(|_| HostError::ActorGone)?;
        wait.await
            .map_err(|_| HostError::ActorGone)?
            .map_err(HostError::from)
    }

    /// 把 head 指到 `entry`；条目不存在时返回 `false`。
    pub(crate) async fn set_head(&self, entry: EntryId) -> Result<bool, HostError> {
        self.call(|reply| SessionCommand::SetHead { entry, reply })
            .await
    }

    /// 切模型：追加一条 `ModelChange`。
    pub(crate) async fn set_model(&self, model: String) -> Result<(), HostError> {
        let result = self
            .call(|reply| SessionCommand::SetModel { model, reply })
            .await?;
        result.map_err(HostError::from)
    }

    /// 改标题：追加一条 `TitleChange`。
    pub(crate) async fn set_title(&self, title: String) -> Result<(), HostError> {
        let result = self
            .call(|reply| SessionCommand::SetTitle { title, reply })
            .await?;
        result.map_err(HostError::from)
    }

    /// 切审批模式，对下一次 turn 生效。
    pub(crate) async fn set_approval_mode(&self, mode: ApprovalMode) -> Result<(), HostError> {
        self.call(|reply| SessionCommand::SetApprovalMode { mode, reply })
            .await
    }

    /// 立刻压缩一次。是否真的压了看 `Event::Compacted`，不要看这个调用是否 `Ok`。
    pub(crate) async fn compact(&self) -> Result<(), HostError> {
        let result = self.call(|reply| SessionCommand::Compact { reply }).await?;
        result.map_err(HostError::from)
    }

    /// 取一份快照：摘要 + head + `since` 之后（按追加顺序）的条目。
    pub(crate) async fn snapshot(
        &self,
        since: Option<EntryId>,
    ) -> Result<SessionSnapshot, HostError> {
        self.call(|reply| SessionCommand::Snapshot { since, reply })
            .await
    }

    /// 发一条命令并等它的回应。命令与回应类型通过 `build` 的返回值绑定，调用点不必
    /// 重复写 `send`/`await`/错误映射。
    async fn call<T>(
        &self,
        build: impl FnOnce(oneshot::Sender<T>) -> SessionCommand,
    ) -> Result<T, HostError> {
        let (reply, wait) = oneshot::channel();
        self.commands
            .send(build(reply))
            .await
            .map_err(|_| HostError::ActorGone)?;
        wait.await.map_err(|_| HostError::ActorGone)
    }
}

/// 会话 actor 主循环：串行处理命令，唯一持有 `AgentRuntime` 的可变引用。
async fn run_actor(mut runtime: AgentRuntime, mut commands: mpsc::Receiver<SessionCommand>) {
    while let Some(command) = commands.recv().await {
        match command {
            SessionCommand::Prompt { text, started } => {
                handle_prompt(&mut runtime, text, started).await;
            }
            SessionCommand::SetHead { entry, reply } => {
                let moved = runtime.store_mut().tree_mut().set_head(&entry);
                let _ = reply.send(moved);
            }
            SessionCommand::SetModel { model, reply } => {
                let result = runtime
                    .store_mut()
                    .append(EntryKind::ModelChange { model })
                    .await
                    .map(|_entry| ());
                let _ = reply.send(result);
            }
            SessionCommand::SetTitle { title, reply } => {
                let result = runtime
                    .store_mut()
                    .append(EntryKind::TitleChange { title })
                    .await
                    .map(|_entry| ());
                let _ = reply.send(result);
            }
            SessionCommand::SetApprovalMode { mode, reply } => {
                runtime.config_mut().approval_mode = mode;
                let _ = reply.send(());
            }
            SessionCommand::Compact { reply } => {
                let result = runtime.compact().await;
                let _ = reply.send(result);
            }
            SessionCommand::Snapshot { since, reply } => {
                let tree = runtime.store().tree();
                let summary = adapter::session_summary(tree);
                let head = adapter::entry_id(tree.head());
                let entries = tree
                    .branch()
                    .into_iter()
                    .filter(|entry| since.as_ref().is_none_or(|cursor| entry.id > *cursor))
                    .map(adapter::entry_to_wire)
                    .collect();
                let _ = reply.send(SessionSnapshot {
                    summary,
                    head,
                    entries,
                });
            }
        }
    }
}

/// 处理一条 `Prompt` 命令：抢在整个 turn 跑完之前，尽早把 `user_entry` 报给调用方。
///
/// `run_turn` 内部在写入用户消息后**立刻**（`drive()` 之前）广播
/// [`AgentEvent::TurnStart`]。本函数在调用 `run_turn` 之前先订阅一份事件流，与
/// `run_turn` 本身的 future 一起 `select!`：先看到 `TurnStart` 就立刻结算 `started`，
/// 不必等后续可能长达数分钟的工具调用循环；`run_turn` 若在写入用户消息那一步就失败
/// （从未发出 `TurnStart`），改由 `run_turn` 的返回值结算 `started`。
async fn handle_prompt(
    runtime: &mut AgentRuntime,
    text: String,
    started: oneshot::Sender<Result<EntryId, AgentError>>,
) {
    let mut events = runtime.events().subscribe();
    let mut started = Some(started);
    let run = runtime.run_turn(text);
    tokio::pin!(run);
    let outcome = loop {
        tokio::select! {
            biased;
            event = events.recv() => {
                if let Some(AgentEvent::TurnStart { user_entry }) = event
                    && let Some(sender) = started.take()
                {
                    let _ = sender.send(Ok(user_entry));
                }
            }
            outcome = &mut run => break outcome,
        }
    };
    match (started.take(), outcome) {
        (Some(sender), Err(error)) => {
            // 从未观测到 TurnStart：写入用户消息那一步本身就失败了。
            let _ = sender.send(Err(error));
        }
        (Some(sender), Ok(())) => {
            // 不可达：Ok 只可能发生在 TurnStart 之后，此时 `started` 应该已被消费。
            // 仍然保守处理，不 unwrap、不 panic。
            tracing::warn!("run_turn 成功结束但从未观测到 TurnStart，属于不可达分支");
            let _ = sender.send(Err(AgentError::Cancelled));
        }
        (None, Ok(())) => {}
        (None, Err(error)) => {
            tracing::warn!(%error, "turn 以错误结束");
        }
    }
}

/// 首条 user 消息是否还需要注入 `<system-reminder>` 环境上下文。
///
/// 幂等判据：根到 head 路径上**只要出现过一条消息**（不论是不是之前注入的提醒本身），
/// 就不再重写。理由与 jcode `crates/jcode-base/src/session.rs:897-921` 的二态规则一致：
/// 已经开始的对话不该被后来的进程 cwd 悄悄覆盖。
fn needs_session_context(tree: &zcode_agent::SessionTree) -> bool {
    tree.branch().iter().all(|entry| entry.message().is_none())
}

/// 按幂等规则注入环境上下文；已经注入过（或已有真实对话）时是 no-op。
async fn inject_session_context(store: &mut SessionStore, context: &str) -> Result<(), StoreError> {
    if !needs_session_context(store.tree()) {
        return Ok(());
    }
    store
        .append(EntryKind::Message {
            message: StoredMessage::system_reminder(context),
        })
        .await
        .map(|_entry| ())
}

/// 装配一个新的 [`AgentRuntime`]。`store` 必须已经完成过环境上下文注入。
fn build_runtime(
    deps: &HostDeps,
    cancels: &Arc<CancelRegistry>,
    store: SessionStore,
) -> AgentRuntime {
    let turn_config = TurnConfig {
        system: deps.prompts.system.clone(),
        cwd: std::path::PathBuf::from(store.tree().cwd()),
        approval_mode: deps.config.approval.mode,
        user_policies: deps.config.approval.policies.clone(),
        thinking: deps.model.thinking,
        ..TurnConfig::default()
    };
    AgentRuntime::new(
        Arc::clone(&deps.provider),
        Arc::clone(&deps.registry),
        store,
        Arc::clone(cancels),
        turn_config,
    )
}

/// 把一个 `AgentRuntime` 交给一个新起的 actor 任务，返回可共享的句柄。
fn spawn_handle(runtime: AgentRuntime) -> Arc<SessionHandle> {
    let events = runtime.events().clone();
    let approvals = Arc::clone(runtime.approvals());
    let stdin = Arc::new(StdinGate::new(events.clone()));
    let (meta, _meta_rx) = tokio::sync::broadcast::channel(SESSION_META_CHANNEL_CAPACITY);
    let (commands, rx) = mpsc::channel(SESSION_COMMAND_CAPACITY);
    tokio::spawn(run_actor(runtime, rx));
    Arc::new(SessionHandle {
        events,
        approvals,
        stdin,
        meta,
        commands,
        holder: Mutex::new(None),
    })
}

/// [`Host`](crate::host::Host) 持有的会话表：`SessionId -> SessionHandle`。
pub(crate) struct SessionTable {
    slots: AsyncMutex<HashMap<SessionId, Arc<SessionHandle>>>,
}

impl SessionTable {
    /// 造一张空表。
    pub(crate) fn new() -> Self {
        Self {
            slots: AsyncMutex::new(HashMap::new()),
        }
    }

    /// 新建一个会话：落盘 + 注入环境上下文 + 起 actor + 登记。
    pub(crate) async fn create(
        &self,
        deps: &HostDeps,
        cancels: &Arc<CancelRegistry>,
        cwd: String,
        model: String,
    ) -> Result<(Arc<SessionHandle>, wire::SessionSummary, wire::Entry), HostError> {
        let mut store = SessionStore::create(&deps.sessions_dir, cwd, model).await?;
        inject_session_context(&mut store, &deps.prompts.session_context).await?;

        let session_id = store.tree().session_id().clone();
        let root = store
            .tree()
            .branch()
            .first()
            .copied()
            .cloned()
            .ok_or_else(|| HostError::UnknownSession(session_id.to_string()))?;
        let summary = adapter::session_summary(store.tree());

        let runtime = build_runtime(deps, cancels, store);
        let handle = spawn_handle(runtime);

        let mut slots = self.slots.lock().await;
        slots.insert(session_id, Arc::clone(&handle));
        drop(slots);

        Ok((handle, summary, adapter::entry_to_wire(&root)))
    }

    /// 取一个已在表内的会话句柄；不在表内时尝试从磁盘懒加载。
    pub(crate) async fn get_or_load(
        &self,
        deps: &HostDeps,
        cancels: &Arc<CancelRegistry>,
        session_id: &SessionId,
    ) -> Result<Arc<SessionHandle>, HostError> {
        let mut slots = self.slots.lock().await;
        if let Some(handle) = slots.get(session_id) {
            return Ok(Arc::clone(handle));
        }

        let path = deps.sessions_dir.join(format!("{session_id}.jsonl"));
        let mut store = SessionStore::open(&path)
            .await
            .map_err(|error| match error {
                StoreError::Io { source, .. } if source.kind() == std::io::ErrorKind::NotFound => {
                    HostError::UnknownSession(session_id.to_string())
                }
                other => HostError::Store(other),
            })?;
        inject_session_context(&mut store, &deps.prompts.session_context).await?;

        let runtime = build_runtime(deps, cancels, store);
        let handle = spawn_handle(runtime);
        slots.insert(session_id.clone(), Arc::clone(&handle));
        Ok(handle)
    }

    /// 遍历所有存活会话，把一次审批答复路由到拥有该 `request_id` 的那个。
    ///
    /// `Request::ApprovalRespond` 不带 session 字段（wire 协议本身如此设计），只能靠
    /// `request_id` 全局唯一这件事逐个尝试。会话数量对单人工具而言是个位数，线性扫描
    /// 的代价可以忽略；量级变化后再考虑维护一张 `request_id -> session` 的反向索引。
    pub(crate) async fn reply_approval(
        &self,
        request_id: &str,
        reply: zcode_agent::ApprovalReply,
    ) -> bool {
        let slots = self.slots.lock().await;
        slots
            .values()
            .any(|handle| handle.approvals.reply(request_id, reply))
    }

    /// 同 [`SessionTable::reply_approval`]，路由一次 stdin 答复。
    pub(crate) async fn reply_stdin(&self, request_id: &str, text: String) -> bool {
        let slots = self.slots.lock().await;
        for handle in slots.values() {
            if handle.stdin.reply(request_id, text.clone()) {
                return true;
            }
        }
        false
    }
}

/// 列出 `sessions_dir` 下的全部会话摘要，按最后更新时间倒序；`cwd_filter` 非空时只保留
/// 匹配的行。
///
/// 直接扫磁盘而不是查存活会话表：会话文件是历史的唯一事实来源
/// （`SessionStore::append` 每次都立即 `flush`），选择器列表不需要哪怕一条历史，
/// 打开每个文件重放一遍即可拿到准确的标题/模型/时间戳，不必唤醒对应的 actor。
pub(crate) async fn list_summaries(
    sessions_dir: &Path,
    cwd_filter: Option<&str>,
) -> Result<Vec<wire::SessionSummary>, HostError> {
    let mut out = Vec::new();
    let mut entries = match tokio::fs::read_dir(sessions_dir).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(error) => return Err(HostError::Io(error)),
    };

    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if path.extension().and_then(std::ffi::OsStr::to_str) != Some("jsonl") {
            continue;
        }
        let store = match SessionStore::open(&path).await {
            Ok(store) => store,
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "跳过无法打开的会话文件");
                continue;
            }
        };
        let summary = adapter::session_summary(store.tree());
        if cwd_filter.is_some_and(|filter| filter != summary.cwd) {
            continue;
        }
        out.push(summary);
    }

    // 倒序：最近更新的排在最前。`sort_by_key` 配不上反向比较（键要借 `&a`），
    // 用 `sort_unstable_by_key` + `Reverse` 表达同一意图且不需要稳定性。
    out.sort_unstable_by_key(|summary| std::cmp::Reverse(summary.updated_ms));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use futures_util::stream;
    use zcode_agent::tool::registry::ToolRegistry;
    use zcode_ai::{
        AiError, CompletionRequest, Provider, ProviderId, StopReason as AiStopReason, StreamEvent,
        Usage,
    };

    use super::*;

    /// 一句话就说完的假 provider：一次 `TextDelta` + `Done`，永不调用工具。
    #[derive(Debug)]
    struct EchoProvider {
        calls: AtomicUsize,
    }

    impl EchoProvider {
        fn new() -> Self {
            Self {
                calls: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait]
    impl Provider for EchoProvider {
        fn id(&self) -> ProviderId {
            ProviderId::Anthropic
        }

        async fn stream(
            &self,
            _request: &CompletionRequest,
        ) -> Result<zcode_ai::EventStream, AiError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let events = vec![
                Ok(StreamEvent::Start {
                    response_id: None,
                    model: None,
                }),
                Ok(StreamEvent::TextStart { index: 0 }),
                Ok(StreamEvent::TextDelta {
                    index: 0,
                    delta: "好的".to_owned(),
                }),
                Ok(StreamEvent::TextEnd {
                    index: 0,
                    text: "好的".to_owned(),
                }),
                Ok(StreamEvent::Done {
                    stop_reason: AiStopReason::Stop,
                    usage: Usage::default(),
                }),
            ];
            Ok(Box::pin(stream::iter(events)))
        }
    }

    /// 造一个装了假 provider 与空注册表的最小 [`AgentRuntime`]。
    fn test_runtime(store: SessionStore) -> AgentRuntime {
        AgentRuntime::new(
            Arc::new(EchoProvider::new()),
            Arc::new(ToolRegistry::new()),
            store,
            Arc::new(CancelRegistry::new()),
            TurnConfig::default(),
        )
    }

    async fn test_store() -> SessionStore {
        let dir = tempfile::tempdir().expect("tempdir 创建失败");
        // 让 `SessionStore` 独占这个临时目录的生命周期：测试结束前不删。
        let store =
            SessionStore::create(dir.path(), "/workspace".to_owned(), "test-model".to_owned())
                .await
                .expect("创建会话文件失败");
        std::mem::forget(dir);
        store
    }

    #[tokio::test]
    async fn prompt_reports_user_entry_before_turn_fully_completes() {
        let handle = spawn_handle(test_runtime(test_store().await));
        let mut events = handle.events.subscribe();

        let entry = handle
            .prompt("你好".to_owned())
            .await
            .expect("prompt 应该成功");
        assert!(entry.as_str().starts_with("ent_"));

        // 事件流上应当能追到同一个 id 的 TurnStart，随后是 TurnEnd。
        let mut saw_turn_start = false;
        loop {
            match events.recv().await.expect("事件流不该提前关闭") {
                AgentEvent::TurnStart { user_entry } => {
                    assert_eq!(user_entry, entry);
                    saw_turn_start = true;
                }
                AgentEvent::TurnEnd => break,
                _ => {}
            }
        }
        assert!(saw_turn_start);
    }

    #[tokio::test]
    async fn turn_survives_after_every_subscriber_is_dropped() {
        // 这条测试证明"turn 属于 session 而不属于连接"：订阅者（模拟客户端连接）在
        // turn 跑起来之后立刻整体丢弃，session 句柄本身（模拟会话表持有的引用）继续
        // 存活，turn 依然完整跑完并落盘。
        let handle = spawn_handle(test_runtime(test_store().await));
        {
            // 模拟一条客户端连接：订阅、拿到 user_entry，然后连同订阅一起 drop。
            let events = handle.events.subscribe();
            handle
                .prompt("有人在吗".to_owned())
                .await
                .expect("prompt 应该成功");
            drop(events);
        } // "连接" 在这里断开，但 `handle` 仍然活着——它属于会话表，不属于连接。

        // 用一个全新的订阅等 TurnEnd：如果 turn 真的跟着上面那个订阅者一起死了，
        // 这里永远等不到 TurnEnd，测试会超时失败。
        let mut fresh = handle.events.subscribe();
        let ended = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if matches!(fresh.recv().await, Some(AgentEvent::TurnEnd) | None) {
                    return;
                }
            }
        })
        .await;
        assert!(ended.is_ok(), "turn 应该在连接断开后继续跑完");
    }

    #[tokio::test]
    async fn set_head_set_model_set_title_and_approval_mode_round_trip() {
        let handle = spawn_handle(test_runtime(test_store().await));

        let snapshot = handle.snapshot(None).await.expect("snapshot 失败");
        let root = snapshot.head.clone();

        handle
            .set_model("new-model".to_owned())
            .await
            .expect("set_model 失败");
        handle
            .set_title("标题".to_owned())
            .await
            .expect("set_title 失败");
        handle
            .set_approval_mode(ApprovalMode::Write)
            .await
            .expect("set_approval_mode 失败");

        let after = handle.snapshot(None).await.expect("snapshot 失败");
        assert_eq!(after.summary.model, "new-model");
        assert_eq!(after.summary.title.as_deref(), Some("标题"));
        assert!(after.entries.len() >= 2);

        // set_head 指回根：head 应该变化。
        let moved = handle
            .set_head(EntryId::from(root.to_string()))
            .await
            .expect("set_head 失败");
        assert!(moved);
        let back = handle.snapshot(None).await.expect("snapshot 失败");
        assert_eq!(back.head, root);
    }

    #[tokio::test]
    async fn compact_without_enough_history_is_a_documented_no_op() {
        let handle = spawn_handle(test_runtime(test_store().await));
        // 历史只有根条目，没有任何消息可摘要：compact 必须是 `Ok(())` 的 no-op，
        // 不能假装压缩发生。
        handle.compact().await.expect("compact 不应该报错");
        let snapshot = handle.snapshot(None).await.expect("snapshot 失败");
        assert!(
            snapshot
                .entries
                .iter()
                .all(|entry| !matches!(entry.kind, wire::EntryKind::Compaction { .. }))
        );
    }

    #[tokio::test]
    async fn claim_default_denies_and_takeover_overrides() {
        let handle = spawn_handle(test_runtime(test_store().await));
        let alice = wire::ClientId::from("alice");
        let bob = wire::ClientId::from("bob");

        assert!(matches!(handle.claim(&alice, false), Claim::Granted));
        assert!(matches!(
            handle.claim(&bob, false),
            Claim::Busy { holder } if holder == alice
        ));
        assert!(matches!(handle.claim(&bob, true), Claim::Granted));
        // bob 接管之后，alice 不带 takeover 重连应该被拒。
        assert!(matches!(
            handle.claim(&alice, false),
            Claim::Busy { holder } if holder == bob
        ));
    }

    #[tokio::test]
    async fn list_summaries_filters_by_cwd_and_sorts_recent_first() {
        let dir = tempfile::tempdir().expect("tempdir 创建失败");
        let mut older = SessionStore::create(dir.path(), "/a".to_owned(), "m".to_owned())
            .await
            .expect("创建失败");
        older
            .append(EntryKind::TitleChange {
                title: "older".to_owned(),
            })
            .await
            .expect("追加失败");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let mut newer = SessionStore::create(dir.path(), "/b".to_owned(), "m".to_owned())
            .await
            .expect("创建失败");
        newer
            .append(EntryKind::TitleChange {
                title: "newer".to_owned(),
            })
            .await
            .expect("追加失败");

        let all = list_summaries(dir.path(), None).await.expect("扫描失败");
        assert_eq!(all.len(), 2);
        assert_eq!(all.first().map(|s| s.cwd.as_str()), Some("/b"));

        let filtered = list_summaries(dir.path(), Some("/a"))
            .await
            .expect("扫描失败");
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered.first().map(|s| s.cwd.as_str()), Some("/a"));
    }
}
