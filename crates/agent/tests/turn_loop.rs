//! turn 循环的端到端验证：脚本化提供商 + 真实工具 + 真实会话文件。
//!
//! 单元测试只覆盖了累积器、审批、存储各自的语义。这里跑的是**把它们接起来之后**的契约：
//! 一次 turn 结束时磁盘上的历史是否合法、事件序列是否可用、取消路径是否仍然维持
//! `tool_use` / `tool_result` 的配对。这三条错了，单元测试全绿也没有意义。

use async_trait::async_trait;
use futures_util::StreamExt as _;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use zcode_agent::approval::{ApprovalDecision, ApprovalMode, Tier};
use zcode_agent::error::ToolError;
use zcode_agent::event::AgentEvent;
use zcode_agent::session::entry::EntryKind;
use zcode_agent::session::message::{
    StoredAssistantContent, StoredMessage, StoredStopReason, StoredUsage,
};
use zcode_agent::session::store::SessionStore;
use zcode_agent::tool::registry::ToolRegistry;
use zcode_agent::tool::{Concurrency, Tool, ToolContext, ToolOutput};
use zcode_agent::turn::{AgentRuntime, TurnConfig};
use zcode_ai::{
    AiError, CompletionRequest, EventStream, Provider, ProviderId, StopReason, StreamEvent,
    ToolCall, Usage,
};

/// 按剧本逐次返回预设事件序列的提供商。
///
/// 两种"挂住"模式，对应 turn 循环里两个不同的可挂起点：
///
/// - `stall`：事件吐完之后流**永不结束**——只在两帧之间查取消位的实现会挂死在这里。
/// - `hang_on_open`：`stream()` 本身**永不返回**——请求还没拿到响应头就进了网络黑洞，
///   连 `EventStream` 都拿不到，逐帧消费处的取消并等根本轮不到。
#[derive(Debug)]
struct ScriptedProvider {
    script: Vec<Vec<StreamEvent>>,
    stall: bool,
    hang_on_open: bool,
    calls: AtomicUsize,
    /// 每次请求的 system 提示。用来确认测试真的走到了它想测的那条请求上——
    /// 否则"压缩请求挂起"的用例在压缩根本没触发时也会通过，等于什么都没测。
    systems: Mutex<Vec<Vec<String>>>,
}

impl ScriptedProvider {
    fn new(script: Vec<Vec<StreamEvent>>) -> Self {
        Self {
            script,
            stall: false,
            hang_on_open: false,
            calls: AtomicUsize::new(0),
            systems: Mutex::new(Vec::new()),
        }
    }

    fn stalling(events: Vec<StreamEvent>) -> Self {
        Self {
            stall: true,
            ..Self::new(vec![events])
        }
    }

    fn hanging_on_open() -> Self {
        Self {
            hang_on_open: true,
            ..Self::new(Vec::new())
        }
    }

    /// 至今为止每次请求的 system 提示。
    fn systems(&self) -> Vec<Vec<String>> {
        self.systems
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

#[async_trait]
impl Provider for ScriptedProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Anthropic
    }

    async fn stream(&self, request: &CompletionRequest) -> Result<EventStream, AiError> {
        self.systems
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(request.system.clone());
        if self.hang_on_open {
            // 永不返回：连 `EventStream` 都拿不到。
            std::future::pending::<()>().await;
        }
        let index = self.calls.fetch_add(1, Ordering::SeqCst);
        let events = self.script.get(index).cloned().unwrap_or_else(|| {
            vec![StreamEvent::Done {
                stop_reason: StopReason::Stop,
                usage: Usage::default(),
            }]
        });
        let scripted =
            futures_util::stream::iter(events.into_iter().map(Ok::<StreamEvent, AiError>));
        if self.stall {
            Ok(Box::pin(scripted.chain(futures_util::stream::pending())))
        } else {
            Ok(Box::pin(scripted))
        }
    }
}

/// 把参数原样回显的工具；`executions` 记录它**真的**被执行了几次。
///
/// 取消路径的核心断言靠它：结果里写着"已取消"不等于工具没跑，必须直接数执行次数。
#[derive(Debug)]
struct EchoTool {
    executions: Arc<AtomicUsize>,
    tier: Tier,
}

impl EchoTool {
    fn new(tier: Tier) -> (Arc<Self>, Arc<AtomicUsize>) {
        let executions = Arc::new(AtomicUsize::new(0));
        let tool = Arc::new(Self {
            executions: Arc::clone(&executions),
            tier,
        });
        (tool, executions)
    }
}

#[async_trait]
impl Tool for EchoTool {
    fn name(&self) -> &'static str {
        "echo"
    }

    fn description(&self) -> &'static str {
        "回显参数"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": { "text": { "type": "string" } },
            "required": ["text"],
        })
    }

    fn approval(&self, _args: &Value) -> ApprovalDecision {
        ApprovalDecision::tier(self.tier)
    }

    fn concurrency(&self, _args: &Value) -> Concurrency {
        Concurrency::Shared
    }

    async fn execute(&self, args: Value, _ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        self.executions.fetch_add(1, Ordering::SeqCst);
        let text = args
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        Ok(ToolOutput::text(format!("echo: {text}")))
    }
}

fn tool_call_turn(call_id: &str, arguments: &str) -> Vec<StreamEvent> {
    vec![
        StreamEvent::Start {
            response_id: None,
            model: Some("test-model".to_owned()),
        },
        StreamEvent::TextStart { index: 0 },
        StreamEvent::TextDelta {
            index: 0,
            delta: "先调工具".to_owned(),
        },
        StreamEvent::TextEnd {
            index: 0,
            text: "先调工具".to_owned(),
        },
        StreamEvent::ToolCallStart {
            index: 1,
            id: call_id.to_owned(),
            name: "echo".to_owned(),
        },
        StreamEvent::ToolCallDelta {
            index: 1,
            delta: arguments.to_owned(),
        },
        StreamEvent::ToolCallEnd {
            index: 1,
            tool_call: ToolCall {
                id: call_id.to_owned(),
                name: "echo".to_owned(),
                arguments: arguments.to_owned(),
            },
        },
        StreamEvent::Done {
            stop_reason: StopReason::ToolUse,
            usage: Usage {
                input: 100,
                output: 20,
                ..Usage::default()
            },
        },
    ]
}

fn final_answer_turn(text: &str) -> Vec<StreamEvent> {
    vec![
        StreamEvent::Start {
            response_id: None,
            model: Some("test-model".to_owned()),
        },
        StreamEvent::TextEnd {
            index: 0,
            text: text.to_owned(),
        },
        StreamEvent::Done {
            stop_reason: StopReason::Stop,
            usage: Usage::default(),
        },
    ]
}

/// 一次不带终止事件的工具调用轮：流吐完这些之后**永远停在这里**。
fn tool_call_turn_without_done(call_id: &str, arguments: &str) -> Vec<StreamEvent> {
    let mut events = tool_call_turn(call_id, arguments);
    events.pop();
    events
}

/// 组装一个跑在临时目录上的运行时。
///
/// 返回 `Result` 而不是在这里 `expect`：clippy 的 `expect_used` 只在 `#[test]` 函数体内
/// 放行，辅助函数里必须传播出去，由调用方在测试体内断言。
async fn build_with(
    dir: &std::path::Path,
    provider: Arc<ScriptedProvider>,
    approval_mode: ApprovalMode,
    tier: Tier,
    seed: Option<SeedHistory>,
) -> Result<(AgentRuntime, PathBuf, Arc<AtomicUsize>), Box<dyn std::error::Error>> {
    let mut store =
        SessionStore::create(dir, "/workspace".to_owned(), "test-model".to_owned()).await?;
    if let Some(seed) = seed {
        seed.apply(&mut store).await?;
    }
    let path = store.path().to_path_buf();
    let mut registry = ToolRegistry::new();
    let (tool, executions) = EchoTool::new(tier);
    registry.register(tool)?;
    let runtime = AgentRuntime::new(
        provider,
        Arc::new(registry),
        store,
        TurnConfig {
            approval_mode,
            ..TurnConfig::default()
        },
    );
    Ok((runtime, path, executions))
}

/// 预先铺进会话的历史：`messages` 条交替的用户 / 助手消息，每条 `bytes` 字节。
///
/// 压缩需要**足够多的条目**才可能有安全切点：`plan_compaction` 在记录数不足
/// `RECENT_TURNS_TO_KEEP` 时一律返回 `None`（宁可不压）。只把单条消息撑大是不够的。
#[derive(Debug, Clone, Copy)]
struct SeedHistory {
    messages: usize,
    bytes: usize,
}

impl SeedHistory {
    async fn apply(self, store: &mut SessionStore) -> Result<(), Box<dyn std::error::Error>> {
        for index in 0..self.messages {
            let filler = "y".repeat(self.bytes);
            let message = if index % 2 == 0 {
                StoredMessage::user(filler)
            } else {
                StoredMessage::Assistant {
                    content: vec![StoredAssistantContent::Text { text: filler }],
                    model: Some("test-model".to_owned()),
                    usage: StoredUsage::default(),
                    stop_reason: StoredStopReason::Stop,
                }
            };
            store.append(EntryKind::Message { message }).await?;
        }
        Ok(())
    }
}

/// 默认组装：yolo 模式 + 只读档位，工具直接放行。
async fn build(
    dir: &std::path::Path,
    script: Vec<Vec<StreamEvent>>,
) -> Result<(AgentRuntime, PathBuf), Box<dyn std::error::Error>> {
    let (runtime, path, _) = build_with(
        dir,
        Arc::new(ScriptedProvider::new(script)),
        ApprovalMode::Yolo,
        Tier::Read,
        None,
    )
    .await?;
    Ok((runtime, path))
}

/// 组装一个"吐完这些事件就永远停住"的提供商，并把它的句柄交回给调用方。
async fn build_stalling(
    dir: &std::path::Path,
    events: Vec<StreamEvent>,
    seed: Option<SeedHistory>,
) -> Result<(AgentRuntime, Arc<AtomicUsize>, Arc<ScriptedProvider>), Box<dyn std::error::Error>> {
    let provider = Arc::new(ScriptedProvider::stalling(events));
    let (runtime, _, executions) = build_with(
        dir,
        Arc::clone(&provider),
        ApprovalMode::Yolo,
        Tier::Read,
        seed,
    )
    .await?;
    Ok((runtime, executions, provider))
}

/// 组装一个必然触发审批的运行时：`always-ask` 模式 + `Exec` 档位。
async fn build_asking(
    dir: &std::path::Path,
    script: Vec<Vec<StreamEvent>>,
) -> Result<(AgentRuntime, Arc<AtomicUsize>), Box<dyn std::error::Error>> {
    let (runtime, _, executions) = build_with(
        dir,
        Arc::new(ScriptedProvider::new(script)),
        ApprovalMode::AlwaysAsk,
        Tier::Exec,
        None,
    )
    .await?;
    Ok((runtime, executions))
}

/// 组装一个"建流就永远不返回"的提供商，并把它的句柄交回给调用方。
async fn build_hanging_on_open(
    dir: &std::path::Path,
) -> Result<(AgentRuntime, Arc<ScriptedProvider>), Box<dyn std::error::Error>> {
    let provider = Arc::new(ScriptedProvider::hanging_on_open());
    let (runtime, _, _) = build_with(
        dir,
        Arc::clone(&provider),
        ApprovalMode::Yolo,
        Tier::Read,
        None,
    )
    .await?;
    Ok((runtime, provider))
}

/// 等到 provider 至少收到 `count` 次请求为止。
///
/// **不要用 `sleep` 猜**：取消如果抢在 turn 进入 `stream()` 之前到达，`drive()` 会在循环
/// 顶部的取消检查处直接返回，于是"零消息、零事件"的断言照样成立——即使并等被删掉，
/// 测试也会通过。等到请求真的发出去，才谈得上"在挂起处被取消"。
async fn wait_for_requests(provider: &ScriptedProvider, count: usize) {
    while provider.systems().len() < count {
        tokio::task::yield_now().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_full_turn_persists_a_provider_legal_history() {
    let dir = tempfile::tempdir().expect("临时目录");
    let (mut runtime, path) = build(
        dir.path(),
        vec![
            tool_call_turn("call_1", r#"{"text":"你好"}"#),
            final_answer_turn("工具说：echo: 你好"),
        ],
    )
    .await
    .expect("组装运行时");

    runtime
        .run_turn("跑一下 echo")
        .await
        .expect("turn 应当成功");

    // 契约一：内存里的上下文是 用户 → 助手(带工具调用) → 工具结果 → 助手(收尾)。
    let context = runtime.store().tree().context();
    assert_eq!(context.len(), 4, "实际上下文：{context:#?}");
    assert!(matches!(
        context.first().map(|record| &record.message),
        Some(StoredMessage::User { .. })
    ));
    assert!(matches!(
        context.get(2).map(|record| &record.message),
        Some(StoredMessage::ToolResult {
            is_error: false,
            ..
        })
    ));

    // 契约二：每个 tool_use 都有配对的 tool_result。缺配对会让后续每次请求都 400。
    let issued: Vec<&str> = context
        .iter()
        .filter_map(|record| match &record.message {
            StoredMessage::Assistant { content, .. } => Some(content),
            _ => None,
        })
        .flatten()
        .filter_map(|block| match block {
            StoredAssistantContent::ToolCall { id, .. } => Some(id.as_str()),
            _ => None,
        })
        .collect();
    let answered: Vec<&str> = context
        .iter()
        .filter_map(|record| match &record.message {
            StoredMessage::ToolResult { tool_call_id, .. } => Some(tool_call_id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(issued, vec!["call_1"]);
    assert_eq!(answered, issued, "每个工具调用都必须有配对结果");

    // 契约三：工具真的跑了，产出进了历史。
    let Some(StoredMessage::ToolResult { content, .. }) =
        context.get(2).map(|record| &record.message)
    else {
        panic!("第三条必须是工具结果");
    };
    let rendered = format!("{content:?}");
    assert!(
        rendered.contains("echo: 你好"),
        "工具产出没进历史：{rendered}"
    );

    // 契约四：重新打开磁盘上的文件，得到同一段历史——事件流可丢，历史不可丢。
    let reopened = SessionStore::open(&path).await.expect("重新打开会话");
    assert_eq!(
        reopened.tree().context().len(),
        context.len(),
        "落盘历史与内存不一致"
    );
    assert_eq!(reopened.tree().model(), "test-model");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_event_stream_brackets_deltas_with_a_stable_entry_id() {
    let dir = tempfile::tempdir().expect("临时目录");
    let (mut runtime, _) = build(dir.path(), vec![final_answer_turn("答案")])
        .await
        .expect("组装运行时");
    let mut stream = runtime.events().subscribe();

    runtime.run_turn("问题").await.expect("turn 应当成功");
    drop(runtime);

    let mut seen = Vec::new();
    while let Some(event) = stream.recv().await {
        seen.push(event);
    }

    let start = seen.iter().find_map(|event| match event {
        AgentEvent::MessageStart { entry } => Some(entry.clone()),
        _ => None,
    });
    let end = seen.iter().find_map(|event| match event {
        AgentEvent::MessageEnd { entry, .. } => Some(entry.clone()),
        _ => None,
    });
    assert!(start.is_some(), "必须发 MessageStart：{seen:#?}");
    assert_eq!(start, end, "MessageStart 与 MessageEnd 必须是同一个条目 id");
    assert!(
        matches!(seen.first(), Some(AgentEvent::TurnStart { .. })),
        "第一条必须是 TurnStart"
    );
    assert!(
        matches!(seen.last(), Some(AgentEvent::TurnEnd)),
        "最后一条必须是 TurnEnd"
    );
}

/// 统计上下文里发出的工具调用数与工具结果数。
fn pairing(context: &[zcode_agent::session::message::MessageRecord]) -> (usize, usize) {
    let issued = context
        .iter()
        .filter_map(|record| match &record.message {
            StoredMessage::Assistant { content, .. } => Some(content),
            _ => None,
        })
        .flatten()
        .filter(|block| matches!(block, StoredAssistantContent::ToolCall { .. }))
        .count();
    let answered = context
        .iter()
        .filter(|record| matches!(record.message, StoredMessage::ToolResult { .. }))
        .count();
    (issued, answered)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelling_a_stalled_stream_ends_the_turn_and_pairs_every_tool_call() {
    // 提供商把工具调用吐完之后**再也不发任何东西**（既不 Done 也不断流）。
    // 只在两帧之间查取消位的实现会在这里永久挂起——超时即失败。
    let dir = tempfile::tempdir().expect("临时目录");
    let (mut runtime, executed, _) = build_stalling(
        dir.path(),
        tool_call_turn_without_done("call_1", r#"{"text":"不该执行"}"#),
        None,
    )
    .await
    .expect("组装运行时");

    let cancel = runtime.cancel_signal().clone();
    let mut watching = runtime.events().subscribe();
    let firing = tokio::spawn(async move {
        // 等到工具调用增量真的流出来，再取消——这样才落在"流已消费到一半、
        // 然后停摆"的场景上。用 `sleep` 猜时序会在慢机器上退化成"还没开流就取消"，
        // 那种情况下即使删掉并等，测试也会因为零消息而假绿。
        while let Some(event) = watching.recv().await {
            if matches!(event, AgentEvent::ToolCallDelta { .. }) {
                break;
            }
        }
        cancel.fire();
    });

    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        runtime.run_turn("取消我"),
    )
    .await
    .expect("停滞的流必须被取消打断，而不是把 turn 挂死")
    .expect("取消不是错误");
    firing.await.expect("取消任务不应 panic");

    let context = runtime.store().tree().context();
    let (issued, answered) = pairing(&context);
    assert_eq!(issued, 1, "助手消息里应当留下那次工具调用：{context:#?}");
    assert_eq!(answered, issued, "取消之后仍然不能留下孤儿 tool_use");
    assert_eq!(
        executed.load(Ordering::SeqCst),
        0,
        "取消之后一个工具都不该跑——副作用发生了就收不回来"
    );
    // 取消守卫必须把信号清干净，否则下一个 turn 会秒退。
    assert!(
        !runtime.cancel_signal().is_set(),
        "turn 结束时必须复位取消信号"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelling_while_an_approval_is_pending_ends_the_turn() {
    // 审批弹窗挂着时用户按中断：没有任何答复会到来，只等 `ask()` 就是永久挂起。
    let dir = tempfile::tempdir().expect("临时目录");
    let (mut runtime, executed) = build_asking(
        dir.path(),
        vec![tool_call_turn("call_1", r#"{"text":"要审批"}"#)],
    )
    .await
    .expect("组装运行时");

    let gate = Arc::clone(runtime.approvals());
    let cancel = runtime.cancel_signal().clone();
    let firing = tokio::spawn(async move {
        // 等到确实产生了一条待审批，再取消——这样测的是"等待中被打断"，
        // 而不是"还没走到审批就退出"。
        loop {
            if !gate.pending().is_empty() {
                break;
            }
            tokio::task::yield_now().await;
        }
        cancel.fire();
    });

    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        runtime.run_turn("需要审批"),
    )
    .await
    .expect("待审批期间取消必须能结束 turn")
    .expect("取消不是错误");
    firing.await.expect("取消任务不应 panic");

    let context = runtime.store().tree().context();
    let (issued, answered) = pairing(&context);
    assert_eq!(issued, 1);
    assert_eq!(answered, 1, "被取消的审批同样要落一条配对结果");
    assert_eq!(
        executed.load(Ordering::SeqCst),
        0,
        "审批未通过就取消，工具不得执行"
    );
    assert!(
        runtime.approvals().pending().is_empty(),
        "turn 收尾必须清空待审批，否则客户端 UI 留下幽灵条目"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelling_while_the_request_is_still_opening_ends_the_turn() {
    // 提供商的 `stream()` 永不返回：连 `EventStream` 都拿不到，
    // 只在"逐帧消费"处并等取消的实现会挂死在建流这一步。
    let dir = tempfile::tempdir().expect("临时目录");
    let (mut runtime, provider) = build_hanging_on_open(dir.path()).await.expect("组装运行时");
    let mut stream = runtime.events().subscribe();

    let cancel = runtime.cancel_signal().clone();
    let firing = tokio::spawn(async move {
        // 等到 `stream()` 真的被调用（它随后永不返回），再取消。
        wait_for_requests(&provider, 1).await;
        cancel.fire();
    });

    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        runtime.run_turn("建流就卡住"),
    )
    .await
    .expect("建流阶段的挂起同样必须能被取消打断")
    .expect("取消不是错误");
    firing.await.expect("取消任务不应 panic");

    let context = runtime.store().tree().context();
    let (issued, answered) = pairing(&context);
    assert_eq!(issued, 0);
    assert_eq!(answered, 0);
    // 没发出 `MessageStart` 就不该有助手消息——空壳消息会永久留在历史里。
    assert!(
        !context
            .iter()
            .any(|record| matches!(record.message, StoredMessage::Assistant { .. })),
        "建流前被取消不得落下空助手消息：{context:#?}"
    );

    drop(runtime);
    let mut seen = Vec::new();
    while let Some(event) = stream.recv().await {
        seen.push(event);
    }
    let starts = seen
        .iter()
        .filter(|event| matches!(event, AgentEvent::MessageStart { .. }))
        .count();
    let ends = seen
        .iter()
        .filter(|event| matches!(event, AgentEvent::MessageEnd { .. }))
        .count();
    assert_eq!(starts, 0, "没开流就不该发 MessageStart：{seen:#?}");
    assert_eq!(
        ends, 0,
        "MessageEnd 绝不能没有配对的 MessageStart：{seen:#?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelling_a_stalled_compaction_stream_ends_the_turn() {
    // 上下文一上来就超阈值，turn 的第一件事是发压缩请求。这里的流**能开**、
    // 但一个事件都不吐（`stalling(vec![])`）——因此测的是压缩那条流的**逐帧消费**处，
    // 而不是建流处。压缩流曾经是裸 `stream.next().await`，与主流程同构却漏改。
    let dir = tempfile::tempdir().expect("临时目录");
    // 12 条 × 60KB = 720KB ≈ 180k token + 18k 开销，远超 80% 阈值（160k）；
    // 条目数也超过 `RECENT_TURNS_TO_KEEP`（10），压缩才可能有安全切点。
    let seed = SeedHistory {
        messages: 12,
        bytes: 60_000,
    };
    let (mut runtime, executed, provider) = build_stalling(dir.path(), Vec::new(), Some(seed))
        .await
        .expect("组装运行时");

    let cancel = runtime.cancel_signal().clone();
    let watching = Arc::clone(&provider);
    let firing = tokio::spawn(async move {
        // 等压缩请求真的发出去，再取消。
        wait_for_requests(&watching, 1).await;
        cancel.fire();
    });

    // 预铺的历史已经超阈值，用户这句话本身很短——压缩在第一次请求模型之前就会发生。
    let huge = "继续".to_owned();
    tokio::time::timeout(std::time::Duration::from_secs(5), runtime.run_turn(huge))
        .await
        .expect("压缩流停滞时取消必须能结束 turn")
        .expect("取消不是错误");
    firing.await.expect("取消任务不应 panic");

    // 关键：确认这一轮**真的**走到了压缩请求上。否则这个用例在压缩没触发时
    // 也会因为主流程的取消而通过，等于什么都没测。
    let systems = provider.systems();
    let first = systems.first().and_then(|system| system.first()).cloned();
    assert!(
        first.is_some_and(|prompt| prompt.contains("compacting a coding session")),
        "第一条请求必须是压缩请求，实际 system：{systems:?}"
    );

    // 半截摘要绝不能落盘——它会替代掉被摘要的原文。
    let has_compaction = runtime
        .store()
        .tree()
        .branch()
        .iter()
        .any(|entry| matches!(entry.kind, EntryKind::Compaction { .. }));
    assert!(!has_compaction, "被取消的压缩不得写入残缺摘要");
    assert_eq!(executed.load(Ordering::SeqCst), 0);
    assert!(!runtime.cancel_signal().is_set());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unknown_tool_becomes_an_error_result_instead_of_killing_the_turn() {
    let dir = tempfile::tempdir().expect("临时目录");
    let mut script = tool_call_turn("call_1", "{}");
    for event in &mut script {
        if let StreamEvent::ToolCallStart { name, .. }
        | StreamEvent::ToolCallEnd {
            tool_call: ToolCall { name, .. },
            ..
        } = event
        {
            *name = "ecko".to_owned();
        }
    }
    let (mut runtime, _) = build(dir.path(), vec![script, final_answer_turn("改用别的办法")])
        .await
        .expect("组装运行时");

    runtime
        .run_turn("调一个不存在的工具")
        .await
        .expect("turn 不该因此失败");

    let context = runtime.store().tree().context();
    let Some(StoredMessage::ToolResult {
        is_error, content, ..
    }) = context.get(2).map(|record| &record.message)
    else {
        panic!("必须落一条工具结果，实际：{context:#?}");
    };
    assert!(*is_error);
    let rendered = format!("{content:?}");
    assert!(
        rendered.contains("echo"),
        "错误文本要给出可用工具名的建议：{rendered}"
    );
    // turn 继续跑到了第二轮。
    assert!(matches!(
        context.get(3).map(|record| &record.message),
        Some(StoredMessage::Assistant { .. })
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_arguments_are_fed_back_to_the_model_not_raised() {
    let dir = tempfile::tempdir().expect("临时目录");
    let (mut runtime, _) = build(
        dir.path(),
        vec![
            // `text` 是 required，这里故意不给。
            tool_call_turn("call_1", r#"{"wrong":1}"#),
            final_answer_turn("我改一下参数"),
        ],
    )
    .await
    .expect("组装运行时");

    runtime
        .run_turn("参数写错")
        .await
        .expect("参数错误不该中断 turn");

    let context = runtime.store().tree().context();
    let Some(StoredMessage::ToolResult {
        is_error, content, ..
    }) = context.get(2).map(|record| &record.message)
    else {
        panic!("必须落一条工具结果，实际：{context:#?}");
    };
    assert!(*is_error);
    let rendered = format!("{content:?}");
    assert!(
        rendered.contains("text"),
        "校验错误必须点名缺失字段：{rendered}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_title_change_is_visible_after_reopening() {
    // 标题走追加条目而不是定宽槽位——重开之后必须取到最后一条。
    let dir = tempfile::tempdir().expect("临时目录");
    let store = SessionStore::create(dir.path(), "/workspace".to_owned(), "test-model".to_owned())
        .await
        .expect("建立会话文件");
    let path = store.path().to_path_buf();
    let mut store = store;
    store
        .append(EntryKind::TitleChange {
            title: "第一版".to_owned(),
        })
        .await
        .expect("写标题");
    store
        .append(EntryKind::TitleChange {
            title: "第二版".to_owned(),
        })
        .await
        .expect("写标题");
    drop(store);

    let reopened = SessionStore::open(&path).await.expect("重新打开会话");
    assert_eq!(reopened.tree().title(), Some("第二版"));
}
