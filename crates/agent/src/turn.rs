//! turn 循环：驱动提供商流、执行工具、维护历史。
//!
//! # 只有一条循环
//!
//! headless 与 TUI 共用这一个函数，事件流是唯一的输出面。jcode 有两条几乎重复的 turn 循环
//! （`crates/jcode-app-core/src/agent/turn_streaming_mpsc.rs` 1695 行 vs
//! `turn_loops.rs` 1199 行），它们已经漂移——`MAX_EMPTY_POST_TOOL_CONTINUATION_ATTEMPTS`
//! 只存在于后者。本仓不复制这个债。
//!
//! # 流式累积器是一个结构体，不是二十个局部变量
//!
//! jcode 在流循环里维护 11 个 per-stream 可变局部变量，provider 中途重放时要**逐个手工清空**
//! （`turn_streaming_mpsc.rs:718-777`），漏一个就是重放后内容重复。本仓把它们收进
//! `StreamAccumulator`，重放就是一次 `reset()`。
//!
//! # 助手条目 id 在开流前就分配
//!
//! `MessageStart`、每一条增量、最终落盘、`MessageEnd` 共用同一个 id。若等落盘再分配，
//! 客户端要到流结束才知道刚才那串文本属于谁——中途接入或刚 `Resync` 恢复的客户端会彻底对不上。
//!
//! # 没有"最多 N 轮工具"的上限
//!
//! 三仓都没有：唯一的界是模型自己不再发工具调用（jcode 完全没有，opencode 的 `maxSteps`
//! 默认 `Infinity`，oh-my-pi 同）。加一个上限是**新契约**，不在本次范围内。真正的界来自
//! 取消信号与上下文预算。

use std::path::PathBuf;
use std::sync::Arc;

use crate::approval::{
    ApprovalGate, ApprovalMode, Policy, UserPolicies, denial_message, resolve_approval,
};
use crate::cancel::{CancelRegistry, TurnRegistration};
use crate::context::{
    CompactionPlan, ContextBudget, effective_context_tokens, estimate_context, plan_compaction,
    plan_forced_compaction, reported_context_tokens,
};
use crate::error::AgentError;
use crate::event::{AgentEvent, EventSink};
use crate::id::EntryId;
use crate::interrupt::InterruptSignal;
use crate::session::entry::{CompactionReason, EntryKind};
use crate::session::message::{
    StoredAssistantContent, StoredMessage, StoredStopReason, StoredToolResultContent, StoredUsage,
};
use crate::session::store::SessionStore;
use crate::tool::registry::ToolRegistry;
use crate::tool::schedule::{PreparedCall, execute_batch};
use crate::tool::{ToolContext, ToolOutput};
use futures_util::StreamExt as _;
use serde_json::Value;
use zcode_ai::{
    CompletionRequest, EventStream, Provider, StopReason, StreamEvent, Thinking, ToolChoice, Usage,
};

/// 压缩用的系统提示。
///
/// 存静态 `.md` 并 `include_str!`，不在代码里拼字符串——见 `rule://zcode-architecture`：
/// 运行时 `fs::read_to_string` 会让 prompt 随发布环境漂移，且逃过编译期检查。
const COMPACTION_PROMPT: &str = include_str!("prompts/compaction.md");

/// 审批提示体里参数部分的字节上限。
///
/// 取自 oh-my-pi 的 `DEFAULT_PROMPT_TRUNCATE_CHARS = 2000`
/// （`packages/coding-agent/src/tools/approval.ts:40`）。**上游未给出这个数的依据**；
/// 它的作用是让审批弹窗在一屏内可读。本仓按字节而非字符计——`&str` 本身就是 UTF-8。
const APPROVAL_ARGS_MAX_BYTES: usize = 2000;

/// 一次 turn 的配置。
#[derive(Debug, Clone)]
pub struct TurnConfig {
    /// system / developer 提示，按顺序拼接。
    pub system: Vec<String>,
    /// 工具的工作目录。
    pub cwd: PathBuf,
    /// 审批模式。
    pub approval_mode: ApprovalMode,
    /// 用户的逐工具策略覆盖。
    pub user_policies: UserPolicies,
    /// 思考配置。
    pub thinking: Thinking,
    /// 上下文超限后允许压缩重试的次数。
    ///
    /// 沿用 jcode 的 `MAX_CONTEXT_LIMIT_RETRIES = 5`
    /// （`crates/jcode-app-core/src/agent/turn_loops.rs:7`）。**上游未给出这个数的依据**，
    /// 本仓沿用并待实测修正；它的作用只是防止"压缩了但没降下来"变成死循环。
    pub max_context_retries: u32,
}

impl Default for TurnConfig {
    fn default() -> Self {
        Self {
            system: Vec::new(),
            cwd: PathBuf::from("."),
            approval_mode: ApprovalMode::default(),
            user_policies: UserPolicies::new(),
            thinking: Thinking::default(),
            max_context_retries: 5,
        }
    }
}

/// 一个内容块在流式过程中的累积状态。
#[derive(Debug, Clone)]
enum PendingBlock {
    Text(String),
    Thinking {
        text: String,
        signature: Option<String>,
    },
    RedactedThinking(String),
    ToolCall {
        id: String,
        name: String,
        arguments: String,
    },
}

/// 一次提供商流的累积器。
///
/// 存在的理由见模块文档：把 per-stream 的可变状态收成一个可整体 `reset()` 的结构体，
/// 而不是散落的十几个局部变量。
#[derive(Debug, Default)]
struct StreamAccumulator {
    /// 按内容块下标累积，最终按下标升序组装。用 `Vec<Option<_>>` 而不是 `HashMap`：
    /// 下标是稠密的小整数，顺序组装也就不用再排序。
    blocks: Vec<Option<PendingBlock>>,
    model: Option<String>,
    usage: Usage,
    stop_reason: StopReason,
}

impl StreamAccumulator {
    /// 清空所有 per-stream 状态。提供商中途重放时调用。
    fn reset(&mut self) {
        self.blocks.clear();
        self.model = None;
        self.usage = Usage::default();
        self.stop_reason = StopReason::default();
    }

    /// 取（必要时扩展）下标对应的槽位。
    fn slot(&mut self, index: usize) -> Option<&mut Option<PendingBlock>> {
        if self.blocks.len() <= index {
            self.blocks.resize(index.saturating_add(1), None);
        }
        self.blocks.get_mut(index)
    }

    fn put(&mut self, index: usize, block: PendingBlock) {
        if let Some(slot) = self.slot(index) {
            *slot = Some(block);
        }
    }

    /// 并进一条流式事件，返回该推给订阅者的 UI 增量（若有）。
    ///
    /// **这是唯一一份并进逻辑**，测试也调它——把它复制一份"不发事件的版本"就是在制造
    /// 两条会各自漂移的路径。
    #[allow(
        clippy::too_many_lines,
        reason = "对线协议枚举的单个穷举 match；拆开会让穷举性分散到多处，新增变体时不再有编译期保护"
    )]
    fn apply(&mut self, entry: &EntryId, event: StreamEvent) -> Option<AgentEvent> {
        match event {
            StreamEvent::Start { model, .. } => {
                // 流中途再次 `Start` 意味着提供商重放了请求。整体清空，
                // 否则重放前后的内容会拼在一起变成重复输出。
                if !self.blocks.is_empty() {
                    self.reset();
                }
                self.model = model;
                None
            }
            StreamEvent::TextStart { index } => {
                self.put(index, PendingBlock::Text(String::new()));
                None
            }
            StreamEvent::TextDelta { index, delta } => {
                if let Some(Some(PendingBlock::Text(text))) = self.slot(index) {
                    text.push_str(&delta);
                }
                Some(AgentEvent::TextDelta {
                    entry: entry.clone(),
                    index,
                    delta,
                })
            }
            StreamEvent::TextEnd { index, text } => {
                self.put(index, PendingBlock::Text(text));
                None
            }
            StreamEvent::ThinkingStart { index } => {
                self.put(
                    index,
                    PendingBlock::Thinking {
                        text: String::new(),
                        signature: None,
                    },
                );
                None
            }
            StreamEvent::ThinkingDelta { index, delta } => {
                if let Some(Some(PendingBlock::Thinking { text, .. })) = self.slot(index) {
                    text.push_str(&delta);
                }
                Some(AgentEvent::ThinkingDelta {
                    entry: entry.clone(),
                    index,
                    delta,
                })
            }
            StreamEvent::ThinkingEnd { index, content } => {
                self.put(
                    index,
                    PendingBlock::Thinking {
                        text: content.text,
                        signature: content.signature,
                    },
                );
                None
            }
            StreamEvent::RedactedThinking { index, data } => {
                self.put(index, PendingBlock::RedactedThinking(data));
                None
            }
            StreamEvent::ToolCallStart { index, id, name } => {
                self.put(
                    index,
                    PendingBlock::ToolCall {
                        id,
                        name,
                        arguments: String::new(),
                    },
                );
                None
            }
            StreamEvent::ToolCallDelta { index, delta } => {
                let mut call_id = String::new();
                if let Some(Some(PendingBlock::ToolCall { id, arguments, .. })) = self.slot(index) {
                    arguments.push_str(&delta);
                    call_id.clone_from(id);
                }
                // 推的是**原始 partial JSON**，不是已解析参数：解析受节流窗口影响会滞后于流。
                Some(AgentEvent::ToolCallDelta {
                    entry: entry.clone(),
                    index,
                    call_id,
                    delta,
                })
            }
            StreamEvent::ToolCallEnd { index, tool_call } => {
                self.put(
                    index,
                    PendingBlock::ToolCall {
                        id: tool_call.id,
                        name: tool_call.name,
                        arguments: tool_call.arguments,
                    },
                );
                None
            }
            StreamEvent::Done { stop_reason, usage } => {
                self.stop_reason = stop_reason;
                self.usage = usage;
                None
            }
        }
    }

    /// 组装成一条落盘态助手消息。
    fn finish(&mut self) -> StoredMessage {
        let content = self
            .blocks
            .drain(..)
            .flatten()
            .map(|block| match block {
                PendingBlock::Text(text) => StoredAssistantContent::Text { text },
                PendingBlock::Thinking { text, signature } => {
                    StoredAssistantContent::Thinking { text, signature }
                }
                PendingBlock::RedactedThinking(data) => {
                    StoredAssistantContent::RedactedThinking { data }
                }
                PendingBlock::ToolCall {
                    id,
                    name,
                    arguments,
                } => StoredAssistantContent::ToolCall {
                    id,
                    name,
                    arguments,
                },
            })
            .collect();
        StoredMessage::Assistant {
            content,
            model: self.model.clone(),
            usage: StoredUsage::from(self.usage),
            stop_reason: StoredStopReason::from(self.stop_reason),
        }
    }
}

/// turn 期间持有的守卫。
///
/// **软取消信号的复位在这里，硬取消信号的复位在 [`TurnRegistration`] 的 `Drop` 里**——
/// 后者是注册表统一保证的，让每个信号只有一处复位点，避免两处各清一半。
#[derive(Debug)]
struct TurnGuard {
    steering: InterruptSignal,
    /// 注册进取消注册表；drop 时注销并复位硬取消信号。
    _registration: TurnRegistration,
}

impl Drop for TurnGuard {
    fn drop(&mut self) {
        self.steering.reset();
    }
}

/// 从助手消息里摘出的一次工具调用。
#[derive(Debug, Clone, PartialEq, Eq)]
struct ToolCallRequest {
    id: String,
    name: String,
    arguments: String,
}

/// 一次调用最终写进历史的结果。
#[derive(Debug, Clone)]
struct ToolResultPayload {
    content: Vec<StoredToolResultContent>,
    is_error: bool,
}

impl ToolResultPayload {
    /// 失败 / 跳过：一条文本，喂回模型让它自己决定怎么改道。
    fn failed(text: impl Into<String>) -> Self {
        Self {
            content: vec![StoredToolResultContent::Text { text: text.into() }],
            is_error: true,
        }
    }
}

impl From<ToolOutput> for ToolResultPayload {
    fn from(output: ToolOutput) -> Self {
        // **原样保留内容块**：图片块不能在这里被压成文本，否则支持图片输入的模型
        // 永远看不到工具产出的截图。
        Self {
            content: output.content,
            is_error: false,
        }
    }
}

/// Agent 运行时：一个会话一个实例。
#[derive(Debug)]
pub struct AgentRuntime {
    provider: Arc<dyn Provider>,
    registry: Arc<ToolRegistry>,
    store: SessionStore,
    events: EventSink,
    approvals: Arc<ApprovalGate>,
    cancels: Arc<CancelRegistry>,
    config: TurnConfig,
    cancel: InterruptSignal,
    steering: InterruptSignal,
    /// 上一次请求提供商回报的上下文占用。压缩决策取它与本地估算的较大者。
    last_reported: Option<u64>,
}

impl AgentRuntime {
    /// 组装一个运行时。
    ///
    /// `cancels` 是**跨会话共享**的：取消请求只带 session id 过来，必须能从一张表里找到
    /// 该会话所有在飞 turn 与后台作业的信号。同一个进程里的所有运行时共用一张表。
    #[must_use]
    pub fn new(
        provider: Arc<dyn Provider>,
        registry: Arc<ToolRegistry>,
        store: SessionStore,
        cancels: Arc<CancelRegistry>,
        config: TurnConfig,
    ) -> Self {
        let events = EventSink::new();
        let approvals = Arc::new(ApprovalGate::new(events.clone()));
        Self {
            provider,
            registry,
            store,
            events,
            approvals,
            cancels,
            config,
            cancel: InterruptSignal::new(),
            steering: InterruptSignal::new(),
            last_reported: None,
        }
    }

    /// 取消注册表。取消请求经它按 session id 找到本 turn 的信号。
    #[must_use]
    pub fn cancels(&self) -> &Arc<CancelRegistry> {
        &self.cancels
    }

    /// 事件广播端：客户端从这里订阅。
    #[must_use]
    pub fn events(&self) -> &EventSink {
        &self.events
    }

    /// 审批回环：客户端答复与重连补拉都走它。
    #[must_use]
    pub fn approvals(&self) -> &Arc<ApprovalGate> {
        &self.approvals
    }

    /// 硬取消信号。
    #[must_use]
    pub fn cancel_signal(&self) -> &InterruptSignal {
        &self.cancel
    }

    /// 软取消（插话）信号。
    #[must_use]
    pub fn steering_signal(&self) -> &InterruptSignal {
        &self.steering
    }

    /// 会话存储。
    #[must_use]
    pub fn store(&self) -> &SessionStore {
        &self.store
    }

    /// 跑一次 turn：写入用户消息，然后循环"请求模型 → 执行工具"直到模型不再调用工具。
    pub async fn run_turn(&mut self, user_text: impl Into<String>) -> Result<(), AgentError> {
        let user_entry = self
            .store
            .append(EntryKind::Message {
                message: StoredMessage::user(user_text),
            })
            .await?;
        self.events.emit(AgentEvent::TurnStart { user_entry });

        let _guard = TurnGuard {
            steering: self.steering.clone(),
            _registration: self
                .cancels
                .register_turn(self.store.tree().session_id(), self.cancel.clone()),
        };
        let result = self.drive().await;
        // 无论成败都要把待审批清空：调用方正在 `await` 那个 oneshot，
        // 不清就是让工具执行永久挂着。
        self.approvals.cancel_all();
        match &result {
            Ok(()) => self.events.emit(AgentEvent::TurnEnd),
            Err(error) => self.events.emit(AgentEvent::Failed {
                message: error.to_string(),
            }),
        }
        result
    }

    /// 可变借出会话存储。
    ///
    /// 宿主层要在 turn 之外改会话：`/rewind` 与 `/branch` 改 head、切模型与改标题各追加
    /// 一条条目。这些都不经过 turn 循环，因此只读的 [`AgentRuntime::store`] 不够。
    ///
    /// **不要用它绕开 turn 循环追加消息**：turn 循环维护着"每个 `ToolCall` 都有配对
    /// `ToolResult`"这条提供商硬约束（见 [`crate::session::message`] 的不变量一节），
    /// 从外部插入消息会破坏它，后续每一次请求都 400。
    pub fn store_mut(&mut self) -> &mut SessionStore {
        &mut self.store
    }

    /// 可变借出 turn 配置。
    ///
    /// 会话生命周期内可以切换审批模式、思考档位与逐工具策略。改动对**下一次**
    /// [`AgentRuntime::run_turn`] 生效；正在跑的 turn 已经把配置读进栈上了，不受影响。
    pub fn config_mut(&mut self) -> &mut TurnConfig {
        &mut self.config
    }

    /// 立刻压缩一次上下文，不管是否达到阈值。
    ///
    /// 对应用户显式请求（wire 侧的 `Request::Compact`）。找不到安全切点时是 no-op——
    /// 宁可不压也不能切出孤儿 `tool_use`，理由见 [`AgentRuntime::compact_with`] 的实现注释。
    pub async fn compact(&mut self) -> Result<(), AgentError> {
        self.compact_with(CompactionReason::Manual).await
    }

    /// turn 主循环。
    async fn drive(&mut self) -> Result<(), AgentError> {
        let mut context_retries = 0_u32;
        loop {
            if self.cancel.is_set() {
                return Ok(());
            }
            self.compact_if_needed().await?;

            let entry = EntryId::generate();
            let Some(message) = (match self.stream_once(&entry).await {
                Ok(message) => message,
                Err(error) => {
                    // 上下文超限不是终局：压缩后重试同一轮。
                    let overflow = match &error {
                        AgentError::Ai(ai) => ai.is_context_overflow(),
                        _ => false,
                    };
                    if !overflow {
                        return Err(error);
                    }
                    context_retries = context_retries.saturating_add(1);
                    if context_retries > self.config.max_context_retries {
                        return Err(AgentError::ContextExhausted {
                            attempts: context_retries,
                        });
                    }
                    self.compact_with(CompactionReason::Overflow).await?;
                    continue;
                }
            }) else {
                // 建流前就被取消：这一轮**没有消息**。既不落盘也不发 `MessageEnd`——
                // 发过 `MessageStart` 才有 `MessageEnd`，空助手消息会污染历史。
                return Ok(());
            };

            let usage = assistant_usage(&message);
            let calls = collect_tool_calls(&message);
            self.store
                .append_with_id(
                    entry.clone(),
                    EntryKind::Message {
                        message: message.clone(),
                    },
                )
                .await?;
            self.last_reported = Some(reported_context_tokens(self.store.tree().model(), usage));
            self.events.emit(AgentEvent::MessageEnd {
                entry: entry.clone(),
                message: Box::new(message),
                usage,
            });

            if calls.is_empty() {
                return Ok(());
            }
            if self.cancel.is_set() {
                // 取消之后**绝不执行**这批工具，但每一个 `tool_use` 仍然必须有配对的
                // `tool_result`——否则这段历史再也发不出去，后续每次请求都 400。
                self.write_results(
                    &calls,
                    calls
                        .iter()
                        .map(|_| ToolResultPayload::failed("已取消：本次调用未执行。"))
                        .collect(),
                )
                .await?;
                return Ok(());
            }
            let results = self.run_tools(&entry, &calls).await;
            self.write_results(&calls, results).await?;
        }
    }

    /// 发一次请求并消费整条流。
    ///
    /// `entry` 是**开流前**分配好的条目 id：`MessageStart`、每一条增量、最终落盘与
    /// `MessageEnd` 共用它。
    ///
    /// 返回 `None` 表示"这一轮没有消息"——在发出 `MessageStart` 之前就被取消了。
    /// 此时调用方**不得**落盘、也不得发 `MessageEnd`：那会造出一条谁也没见过开头的
    /// 空助手消息，同时破坏 [`AgentEvent::MessageStart`] 与 `MessageEnd` 的配对契约。
    async fn stream_once(&mut self, entry: &EntryId) -> Result<Option<StoredMessage>, AgentError> {
        let records = self.store.tree().context();
        let messages = records
            .iter()
            .map(|record| record.message.to_provider())
            .collect();

        let mut request = CompletionRequest::new(self.store.tree().model(), messages);
        request.system.clone_from(&self.config.system);
        request.tools = self.registry.definitions();
        request.tool_choice = ToolChoice::Auto;
        request.thinking = self.config.thinking;
        request.session_id = Some(self.store.tree().session_id().as_str().to_owned());

        let mut accumulator = StreamAccumulator::default();
        // **建流本身也会挂**：请求还没拿到响应头之前 `stream()` 不返回，
        // 网络黑洞下这一步可以永久停在这里。
        let Some(mut stream) = self.open_stream(&request).await? else {
            return Ok(None);
        };
        self.events.emit(AgentEvent::MessageStart {
            entry: entry.clone(),
        });

        let sink = self.events.clone();
        let cancelled = drain_stream(&mut stream, &self.cancel, |event| {
            if let Some(ui) = accumulator.apply(entry, event) {
                sink.emit(ui);
            }
        })
        .await?;
        // 丢弃剩余的流即中止底层请求，不必等提供商把话说完。
        drop(stream);

        if cancelled || self.cancel.is_set() {
            accumulator.stop_reason = StopReason::Aborted;
        }
        Ok(Some(accumulator.finish()))
    }

    /// 建立一条提供商流；取消先到达时返回 `None`。
    async fn open_stream(
        &self,
        request: &CompletionRequest,
    ) -> Result<Option<EventStream>, AgentError> {
        tokio::select! {
            biased;
            () = self.cancel.notified() => Ok(None),
            stream = self.provider.stream(request) => Ok(Some(stream?)),
        }
    }

    /// 校验、审批、执行一批工具调用。返回与 `calls` 一一对应、顺序一致的结果。
    async fn run_tools(
        &self,
        entry: &EntryId,
        calls: &[ToolCallRequest],
    ) -> Vec<ToolResultPayload> {
        let mut prepared = Vec::new();
        let mut results: Vec<Option<ToolResultPayload>> = vec![None; calls.len()];

        for (index, call) in calls.iter().enumerate() {
            match self.prepare(call).await {
                Ok(ready) => prepared.push((index, ready)),
                Err(message) => {
                    if let Some(slot) = results.get_mut(index) {
                        *slot = Some(ToolResultPayload::failed(message));
                    }
                }
            }
        }

        // 定稿阶段（尤其是等审批）期间可能被取消。此时**一个工具都不能跑**——
        // 副作用一旦发生就收不回来。但每个调用仍然要有配对结果。
        if self.cancel.is_set() {
            for (index, _) in prepared {
                if let Some(slot) = results.get_mut(index) {
                    *slot = Some(ToolResultPayload::failed("已取消：本次调用未执行。"));
                }
            }
            return fill_missing(results);
        }

        // `PreparedCall` 按值搬进调度器：`args` 是 `Value`，克隆一份纯属浪费。
        let (indices, ready): (Vec<usize>, Vec<PreparedCall>) = prepared.into_iter().unzip();
        for call in &ready {
            self.events.emit(AgentEvent::ToolStart {
                call_id: call.call_id.clone(),
                name: call.tool_name.clone(),
            });
        }

        let events = self.events.clone();
        let session_id = self.store.tree().session_id().clone();
        let cwd = self.config.cwd.clone();
        let cancel = self.cancel.clone();
        let steering = self.steering.clone();
        let owner = entry.clone();
        let outcomes = execute_batch(ready, move |call| {
            let (progress, mut receiver) = tokio::sync::mpsc::unbounded_channel();
            let forwarding = events.clone();
            let call_id = call.call_id.clone();
            tokio::spawn(async move {
                while let Some(update) = receiver.recv().await {
                    forwarding.emit(AgentEvent::ToolProgress {
                        call_id: call_id.clone(),
                        progress: update,
                    });
                }
            });
            ToolContext {
                session_id: session_id.clone(),
                entry_id: owner.clone(),
                call_id: call.call_id.clone(),
                cwd: cwd.clone(),
                cancel: cancel.clone(),
                // 不可被插话打断的工具只看硬取消信号；可打断的两个都看。
                steering: if call.tool.interruptible(&call.args) {
                    steering.clone()
                } else {
                    InterruptSignal::new()
                },
                progress,
            }
        })
        .await;

        for (position, outcome) in outcomes.into_iter().enumerate() {
            let Some(index) = indices.get(position) else {
                continue;
            };
            let payload = match outcome.outcome {
                Ok(output) => ToolResultPayload::from(output),
                Err(error) => ToolResultPayload::failed(error.to_string()),
            };
            if let Some(slot) = results.get_mut(*index) {
                *slot = Some(payload);
            }
        }

        fill_missing(results)
    }

    /// 把一批结果按调用顺序写进历史并广播。
    async fn write_results(
        &mut self,
        calls: &[ToolCallRequest],
        results: Vec<ToolResultPayload>,
    ) -> Result<(), AgentError> {
        for (call, payload) in calls.iter().zip(results) {
            let entry = self
                .store
                .append(EntryKind::Message {
                    message: StoredMessage::ToolResult {
                        tool_call_id: call.id.clone(),
                        tool_name: call.name.clone(),
                        content: payload.content,
                        is_error: payload.is_error,
                    },
                })
                .await?;
            self.events.emit(AgentEvent::ToolEnd {
                call_id: call.id.clone(),
                entry,
                is_error: payload.is_error,
            });
        }
        Ok(())
    }

    /// 单个调用的定稿：解析参数 → schema 校验 → 审批。任一步失败都返回喂回模型的文本。
    async fn prepare(&self, call: &ToolCallRequest) -> Result<PreparedCall, String> {
        let Some(tool) = self.registry.get(&call.name) else {
            return Err(self.registry.unknown_tool_message(&call.name));
        };
        let args: Value = if call.arguments.trim().is_empty() {
            Value::Object(serde_json::Map::new())
        } else {
            serde_json::from_str(&call.arguments)
                .map_err(|error| format!("参数不是合法 JSON：{error}"))?
        };
        self.registry.validate(&call.name, &args)?;

        let decision = tool.approval(&args);
        let resolved = resolve_approval(
            &call.name,
            &decision,
            self.config.approval_mode,
            &self.config.user_policies,
        );
        match resolved.policy {
            Policy::Deny => return Err(denial_message(&call.name, &resolved)),
            Policy::Prompt => {
                let scope = decision.scope_for(&call.name);
                let prompt = approval_prompt(&call.name, &args, resolved.reason.as_deref());
                // 与取消信号并等：用户可能在审批弹窗还挂着的时候按下中断，
                // 此时没有任何答复会到来，只等 `ask()` 就是永久挂起。
                // 遗留的 pending 槽位由 `run_turn` 收尾时的 `cancel_all()` 结算并广播。
                let granted = tokio::select! {
                    biased;
                    () = self.cancel.notified() => None,
                    granted = self.approvals.ask(&call.id, &call.name, &scope, prompt) => {
                        Some(granted)
                    }
                };
                match granted {
                    None => {
                        return Err("已取消：等待审批期间被中断，本次调用未执行。".to_owned());
                    }
                    Some(false) => {
                        return Err(format!("用户拒绝了工具 `{}` 的这次调用。", call.name));
                    }
                    Some(true) => {}
                }
            }
            Policy::Allow => {}
        }

        let concurrency = tool.concurrency(&args);
        Ok(PreparedCall {
            call_id: call.id.clone(),
            tool_name: call.name.clone(),
            tool: Arc::clone(tool),
            args,
            concurrency,
        })
    }

    /// 当前上下文的占用估算与压缩计划。
    ///
    /// `reason` 决定走哪条计划：自动触发（`Threshold` / `Overflow`）要过阈值门，
    /// 用户显式请求（`Manual`）不过——阈值门加在手动路径上会让"立刻压缩"在占用不高时
    /// 静默 no-op，见 [`plan_forced_compaction`] 的文档。
    fn plan_for(
        &self,
        reason: CompactionReason,
    ) -> (CompactionPlan, Vec<crate::session::message::MessageRecord>) {
        let records = self.store.tree().context();
        let plan = match reason {
            CompactionReason::Manual => plan_forced_compaction(&records),
            CompactionReason::Threshold | CompactionReason::Overflow => {
                let messages: Vec<StoredMessage> = records
                    .iter()
                    .map(|record| record.message.clone())
                    .collect();
                let budget = ContextBudget::for_model(self.store.tree().model());
                let occupied =
                    effective_context_tokens(estimate_context(&messages), self.last_reported);
                plan_compaction(&records, budget, occupied)
            }
        };
        (plan, records)
    }

    /// 需要时压缩上下文。
    async fn compact_if_needed(&mut self) -> Result<(), AgentError> {
        let (plan, _) = self.plan_for(CompactionReason::Threshold);
        if matches!(plan, CompactionPlan::None) {
            return Ok(());
        }
        self.compact_with(CompactionReason::Threshold).await
    }

    /// 执行一次压缩：请模型给出摘要，写入一条 [`EntryKind::Compaction`]。
    async fn compact_with(&mut self, reason: CompactionReason) -> Result<(), AgentError> {
        let (plan, records) = self.plan_for(reason);
        let CompactionPlan::Compact { first_kept, .. } = plan else {
            // 找不到安全切点：宁可不压也不能切出孤儿 `tool_use`——那会让后续每次请求都 400。
            // 手动路径走到这里说明历史确实还不够压（保留段之外一条消息都没有）。
            tracing::warn!(?reason, "找不到安全的压缩切点，保持原样");
            return Ok(());
        };

        let head: Vec<_> = records
            .iter()
            .take_while(|record| Some(&record.id) != first_kept.as_ref())
            .map(|record| record.message.to_provider())
            .collect();
        if head.is_empty() {
            return Ok(());
        }

        let mut request = CompletionRequest::new(self.store.tree().model(), head);
        request.system = vec![COMPACTION_PROMPT.to_owned()];
        request.tool_choice = ToolChoice::None;

        // 建流与消费都必须能被取消打断——两处都可能永久挂起，理由与 `stream_once` 相同。
        let Some(mut stream) = self.open_stream(&request).await? else {
            return Ok(());
        };
        let mut summary = String::new();
        let cancelled = drain_stream(&mut stream, &self.cancel, |event| {
            if let StreamEvent::TextDelta { delta, .. } = event {
                summary.push_str(&delta);
            }
        })
        .await?;
        drop(stream);
        if cancelled {
            // 半截摘要落盘等于永久损坏这段历史——它会替代掉被摘要的原文。
            tracing::warn!("压缩过程中被取消，不写入残缺摘要");
            return Ok(());
        }
        if summary.trim().is_empty() {
            tracing::warn!("压缩请求没有产出摘要，保持原样");
            return Ok(());
        }

        let entry = self
            .store
            .append(EntryKind::Compaction {
                summary,
                first_kept,
                reason,
            })
            .await?;
        // 压缩改变了 prompt 前缀，提供商上一轮回报的占用不再成立。
        self.last_reported = None;
        self.events.emit(AgentEvent::Compacted { entry });
        Ok(())
    }
}

/// 消费一条提供商流，直到流结束或取消到达；返回是否是被取消打断的。
///
/// **只有这一份实现**。取消并等最初只写在主流程上，压缩那条流忘了同样处理——
/// 同构漏洞第二次出现。共用一个 helper，让"提供商停摆时取消仍然生效"这条不变量
/// 不依赖每个调用点各自记得。
async fn drain_stream(
    stream: &mut EventStream,
    cancel: &InterruptSignal,
    mut on_event: impl FnMut(StreamEvent),
) -> Result<bool, AgentError> {
    loop {
        let next = tokio::select! {
            biased;
            () = cancel.notified() => return Ok(true),
            event = stream.next() => event,
        };
        let Some(event) = next else {
            return Ok(false);
        };
        on_event(event?);
    }
}

/// 把没有结果的槽位补成占位错误。
///
/// 调度层理应为每个调用回一条结果，少一条也必须补上：一个没有配对 `tool_result` 的
/// `tool_use` 会让这段历史再也发不出去，后续每一次请求都 400。
fn fill_missing(results: Vec<Option<ToolResultPayload>>) -> Vec<ToolResultPayload> {
    results
        .into_iter()
        .map(|slot| slot.unwrap_or_else(|| ToolResultPayload::failed("工具未产出结果。")))
        .collect()
}

fn assistant_usage(message: &StoredMessage) -> StoredUsage {
    match message {
        StoredMessage::Assistant { usage, .. } => *usage,
        StoredMessage::User { .. } | StoredMessage::ToolResult { .. } => StoredUsage::default(),
    }
}

fn collect_tool_calls(message: &StoredMessage) -> Vec<ToolCallRequest> {
    let StoredMessage::Assistant { content, .. } = message else {
        return Vec::new();
    };
    content
        .iter()
        .filter_map(|block| match block {
            StoredAssistantContent::ToolCall {
                id,
                name,
                arguments,
            } => Some(ToolCallRequest {
                id: id.clone(),
                name: name.clone(),
                arguments: arguments.clone(),
            }),
            StoredAssistantContent::Text { .. }
            | StoredAssistantContent::Thinking { .. }
            | StoredAssistantContent::RedactedThinking { .. } => None,
        })
        .collect()
}

/// 询问用户时展示的文案。
///
/// **这是给人看的 UI 文本，不是发给模型的 prompt**，因此不适用
/// `rule://zcode-architecture` 的"prompt 一律存静态 `.md`"。那条约束针对的是模型输入：
/// 它漂移会悄悄改变模型行为且逃过编译期检查。审批文案不进任何请求体，改错了用户当场就看得见。
/// 真正发给模型的那条（压缩摘要）存在 `prompts/compaction.md`，走 `include_str!`。
fn approval_prompt(tool_name: &str, args: &Value, reason: Option<&str>) -> String {
    let mut prompt = format!("允许执行工具 `{tool_name}`？");
    if let Some(reason) = reason {
        prompt.push_str("\n理由：");
        prompt.push_str(reason);
    }
    prompt.push_str("\n参数：");
    // 参数可能很长（整个文件内容都可能在里面），统一走 `zcode-text` 的封顶，
    // 不在这里另写一份截断。
    prompt.push_str(&zcode_text::truncate::enforce_inline_byte_cap(
        &args.to_string(),
        APPROVAL_ARGS_MAX_BYTES,
    ));
    prompt
}

#[cfg(test)]
mod tests {
    use zcode_ai::{ThinkingContent, ToolCall};

    use super::*;

    /// 跑一遍累积器，返回组装结果与沿途产生的 UI 事件。
    fn accumulate(events: Vec<StreamEvent>) -> (StoredMessage, Vec<AgentEvent>, EntryId) {
        let entry = EntryId::generate();
        let mut accumulator = StreamAccumulator::default();
        let mut emitted = Vec::new();
        for event in events {
            if let Some(ui) = accumulator.apply(&entry, event) {
                emitted.push(ui);
            }
        }
        (accumulator.finish(), emitted, entry)
    }

    fn done() -> StreamEvent {
        StreamEvent::Done {
            stop_reason: StopReason::Stop,
            usage: Usage::default(),
        }
    }

    #[test]
    fn blocks_are_assembled_in_index_order() {
        let (message, _, _) = accumulate(vec![
            StreamEvent::ThinkingStart { index: 0 },
            StreamEvent::ThinkingDelta {
                index: 0,
                delta: "想".to_owned(),
            },
            StreamEvent::ThinkingEnd {
                index: 0,
                content: ThinkingContent {
                    text: "想清楚了".to_owned(),
                    signature: Some("sig".to_owned()),
                },
            },
            StreamEvent::TextStart { index: 1 },
            StreamEvent::TextDelta {
                index: 1,
                delta: "答".to_owned(),
            },
            StreamEvent::TextEnd {
                index: 1,
                text: "答案".to_owned(),
            },
            done(),
        ]);
        let StoredMessage::Assistant { content, .. } = message else {
            panic!("必须组装成助手消息");
        };
        assert_eq!(
            content,
            vec![
                StoredAssistantContent::Thinking {
                    text: "想清楚了".to_owned(),
                    signature: Some("sig".to_owned()),
                },
                StoredAssistantContent::Text {
                    text: "答案".to_owned(),
                },
            ]
        );
    }

    #[test]
    fn every_delta_carries_the_preallocated_entry_id() {
        // 客户端靠这个 id 把增量归属到 MessageStart / MessageEnd 之间的那条消息。
        let (_, emitted, entry) = accumulate(vec![
            StreamEvent::TextStart { index: 0 },
            StreamEvent::TextDelta {
                index: 0,
                delta: "a".to_owned(),
            },
            StreamEvent::ThinkingStart { index: 1 },
            StreamEvent::ThinkingDelta {
                index: 1,
                delta: "b".to_owned(),
            },
            StreamEvent::ToolCallStart {
                index: 2,
                id: "call_1".to_owned(),
                name: "read".to_owned(),
            },
            StreamEvent::ToolCallDelta {
                index: 2,
                delta: "{}".to_owned(),
            },
            done(),
        ]);
        assert_eq!(emitted.len(), 3, "三条增量都要推出去：{emitted:?}");
        for event in &emitted {
            let seen = match event {
                AgentEvent::TextDelta { entry, .. }
                | AgentEvent::ThinkingDelta { entry, .. }
                | AgentEvent::ToolCallDelta { entry, .. } => entry,
                other => panic!("不该出现的事件：{other:?}"),
            };
            assert_eq!(seen, &entry, "增量必须带预分配的条目 id");
            assert!(!seen.as_str().is_empty());
        }
    }

    #[test]
    fn a_mid_stream_restart_discards_everything_accumulated() {
        // 提供商重放请求时若不整体清空，重放前后的文本会拼在一起变成重复输出。
        let (message, _, _) = accumulate(vec![
            StreamEvent::TextStart { index: 0 },
            StreamEvent::TextDelta {
                index: 0,
                delta: "第一次".to_owned(),
            },
            StreamEvent::Start {
                response_id: None,
                model: Some("m".to_owned()),
            },
            StreamEvent::TextStart { index: 0 },
            StreamEvent::TextEnd {
                index: 0,
                text: "第二次".to_owned(),
            },
            done(),
        ]);
        let StoredMessage::Assistant { content, model, .. } = message else {
            panic!("必须组装成助手消息");
        };
        assert_eq!(
            content,
            vec![StoredAssistantContent::Text {
                text: "第二次".to_owned()
            }]
        );
        assert_eq!(model.as_deref(), Some("m"));
    }

    #[test]
    fn tool_call_arguments_survive_streaming_in_fragments() {
        let (message, _, _) = accumulate(vec![
            StreamEvent::ToolCallStart {
                index: 0,
                id: "call_1".to_owned(),
                name: "read".to_owned(),
            },
            StreamEvent::ToolCallDelta {
                index: 0,
                delta: r#"{"path":"#.to_owned(),
            },
            StreamEvent::ToolCallDelta {
                index: 0,
                delta: r#""a.rs"}"#.to_owned(),
            },
            StreamEvent::ToolCallEnd {
                index: 0,
                tool_call: ToolCall {
                    id: "call_1".to_owned(),
                    name: "read".to_owned(),
                    arguments: r#"{"path":"a.rs"}"#.to_owned(),
                },
            },
            done(),
        ]);
        assert_eq!(
            collect_tool_calls(&message),
            vec![ToolCallRequest {
                id: "call_1".to_owned(),
                name: "read".to_owned(),
                arguments: r#"{"path":"a.rs"}"#.to_owned(),
            }]
        );
    }

    #[test]
    fn sparse_block_indices_do_not_produce_holes() {
        // 提供商可能跳号（某个块被过滤掉）。组装结果里不能出现空洞占位。
        let (message, _, _) = accumulate(vec![
            StreamEvent::TextEnd {
                index: 3,
                text: "只有这一块".to_owned(),
            },
            done(),
        ]);
        let StoredMessage::Assistant { content, .. } = message else {
            panic!("必须组装成助手消息");
        };
        assert_eq!(content.len(), 1);
    }

    #[test]
    fn tool_images_are_not_flattened_into_text() {
        // 支持图片输入的模型必须能看到工具产出的截图。
        let output = ToolOutput::image(crate::session::message::StoredImage {
            media_type: "image/png".to_owned(),
            data: "AAAA".to_owned(),
        });
        let payload = ToolResultPayload::from(output);
        assert!(!payload.is_error);
        assert!(matches!(
            payload.content.first(),
            Some(StoredToolResultContent::Image { .. })
        ));
    }

    #[test]
    fn turn_guard_clears_both_signals_between_turns() {
        // 硬取消由注册表守卫复位、软取消由 TurnGuard 复位；任一处漏了都会让下一个 turn 秒退。
        let registry = Arc::new(CancelRegistry::new());
        let session = crate::id::SessionId::from("ses_guard".to_owned());
        let cancel = InterruptSignal::new();
        let steering = InterruptSignal::new();
        {
            let _guard = TurnGuard {
                steering: steering.clone(),
                _registration: registry.register_turn(&session, cancel.clone()),
            };
            cancel.fire();
            steering.fire();
        }
        assert!(!cancel.is_set());
        assert!(!steering.is_set());
        assert!(!registry.is_turn_active(&session));
    }

    #[test]
    fn approval_prompt_caps_oversized_arguments() {
        let args = serde_json::json!({ "blob": "x".repeat(50_000) });
        let prompt = approval_prompt("write", &args, Some("会覆盖已有文件"));
        assert!(prompt.contains("会覆盖已有文件"));
        assert!(
            prompt.len() < APPROVAL_ARGS_MAX_BYTES + 256,
            "审批提示体不得被巨大的参数撑爆：{} 字节",
            prompt.len()
        );
    }

    #[test]
    fn non_assistant_messages_have_no_tool_calls() {
        assert!(collect_tool_calls(&StoredMessage::user("hi")).is_empty());
    }

    /// 只产出一段固定摘要的假 provider。
    ///
    /// trait 注入而不是全局替换：`rule://rust-testing` 禁止进程级 mock，
    /// 而且并行跑测时全局替换必然互相串。
    #[derive(Debug)]
    struct SummaryProvider;

    #[async_trait::async_trait]
    impl Provider for SummaryProvider {
        fn id(&self) -> zcode_ai::ProviderId {
            zcode_ai::ProviderId::Anthropic
        }

        async fn stream(
            &self,
            _request: &CompletionRequest,
        ) -> Result<EventStream, zcode_ai::AiError> {
            let events = vec![
                Ok(StreamEvent::TextStart { index: 0 }),
                Ok(StreamEvent::TextDelta {
                    index: 0,
                    delta: "前情摘要".to_owned(),
                }),
                Ok(StreamEvent::TextEnd {
                    index: 0,
                    text: "前情摘要".to_owned(),
                }),
                Ok(done()),
            ];
            Ok(Box::pin(futures_util::stream::iter(events)))
        }
    }

    async fn runtime_with_history(dir: &std::path::Path, messages: usize) -> AgentRuntime {
        let mut store = SessionStore::create(dir, "/tmp/ws".to_owned(), "test-model".to_owned())
            .await
            .expect("建会话");
        for i in 0..messages {
            store
                .append(EntryKind::Message {
                    message: StoredMessage::user(format!("q{i}")),
                })
                .await
                .expect("写入消息");
        }
        AgentRuntime::new(
            Arc::new(SummaryProvider),
            Arc::new(ToolRegistry::new()),
            store,
            Arc::new(CancelRegistry::new()),
            TurnConfig::default(),
        )
    }

    /// 用户显式压缩必须**真的**压，哪怕占用远低于阈值。
    ///
    /// 回归防线：`compact()` 若复用带阈值门的 `plan_compaction`，`Request::Compact`
    /// 就会回一个成功却什么都没做——会话里既没有 `Compaction` 条目，也没有
    /// `Compacted` 事件，而调用方无从分辨。
    #[tokio::test]
    async fn manual_compaction_writes_an_entry_below_threshold() {
        let dir = tempfile::tempdir().expect("临时目录");
        // 12 条 > RECENT_TURNS_TO_KEEP(10)，因此存在可摘要的前缀；
        // 12 条短消息的 token 估算远低于任何模型的压缩阈值。
        let mut runtime = runtime_with_history(dir.path(), 12).await;
        let mut events = runtime.events().subscribe();

        runtime.compact().await.expect("手动压缩不该失败");

        let branch = runtime.store().tree().branch();
        let last = branch.last().expect("路径非空");
        let EntryKind::Compaction {
            summary, reason, ..
        } = &last.kind
        else {
            panic!("手动压缩必须落一条 Compaction 条目，实得：{:?}", last.kind);
        };
        assert_eq!(summary, "前情摘要");
        assert_eq!(*reason, CompactionReason::Manual);

        let event = events.recv().await.expect("必须广播 Compacted");
        assert!(
            matches!(event, AgentEvent::Compacted { ref entry } if entry == &last.id),
            "Compacted 的条目 id 要与落盘的那条一致：{event:?}"
        );
    }

    /// 绕过阈值不等于绕过安全切点：历史确实不够时保持原样，且**不**伪造事件。
    #[tokio::test]
    async fn manual_compaction_is_a_noop_when_there_is_nothing_to_summarize() {
        let dir = tempfile::tempdir().expect("临时目录");
        let mut runtime = runtime_with_history(dir.path(), 3).await;

        runtime.compact().await.expect("没东西可压不算失败");

        let branch = runtime.store().tree().branch();
        assert!(
            !branch
                .iter()
                .any(|entry| matches!(entry.kind, EntryKind::Compaction { .. })),
            "历史不够时不该落 Compaction 条目"
        );
    }
}
