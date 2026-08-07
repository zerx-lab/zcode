//! 端到端冒烟：把 CLI 装配链整条串起来跑一次真实对话。
//!
//! # 它证明什么
//!
//! 各模块自己的单元测试只覆盖各自的契约；本模块覆盖**它们之间的缝**——
//! `config` → `tools::registry` → `prompt::build` → `host::connect::connect`
//! → `handle_client` → `ClientSession` → `Request::Prompt` → turn 循环
//! → **真实工具执行** → 会话 JSONL 落盘。缝上出问题（签名对得上但语义对不上、
//! 装配顺序错、工具拿不到正确的工作区根）在单模块测试里一律看不见。
//!
//! # 为什么不打真实提供商
//!
//! provider 是唯一被替身的东西：一次真实请求需要凭据、要花钱、结果不确定，
//! 没法作为回归门。除它之外的一切都是生产实现——真的注册八个工具、真的读磁盘上的文件、
//! 真的走 wire 协议往返、真的落 JSONL。
//!
//! # 为什么走 `connect()` 而不是自己拼 `stream_pair`
//!
//! `plans/runtime-boundary/README.md:195` 已裁决：headless 与 TUI 共用同一条执行路径，
//! daemon 不在时由 `connect()` 自托管。测试直接调 `connect()`（`daemon.enabled = false`）
//! 才是在测生产路径；自己拼一对流会绕开被测对象本身。

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use futures_util::FutureExt as _;
use zcode_agent::ApprovalMode;
use zcode_agent::session::entry::EntryKind;
use zcode_agent::session::message::{StoredAssistantContent, StoredMessage};
use zcode_agent::session::store::SessionStore;
use zcode_ai::{
    AiError, CompletionRequest, EventStream, Provider, ProviderId, StopReason, StreamEvent,
    Thinking, ToolCall, Usage,
};
use zcode_protocol::wire::types::{ClientId, SessionId, UserContent};
use zcode_protocol::{Reply, Request};

use crate::config::{
    ApprovalConfig, Config, DaemonConfig, ModelConfig, SessionConfig, ToolsConfig, UiConfig,
};
use crate::host::HostDeps;
use crate::host::connect::{self, ClientSession};
use crate::model::ResolvedModel;
use crate::workspace::Workspace;

/// 冒烟用的假提供商：第一轮吐一个 `read` 工具调用，第二轮把工具结果复述成最终答案。
///
/// 它不解析请求内容，只按"这是第几次调用"分支——本测试要证明的是**装配是否接通**，
/// 不是模型是否聪明。
#[derive(Debug)]
struct ScriptedProvider {
    calls: AtomicUsize,
    /// 第一轮要求读的文件（相对工作区根）。
    target: String,
}

#[async_trait]
impl Provider for ScriptedProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Anthropic
    }

    async fn stream(&self, request: &CompletionRequest) -> Result<EventStream, AiError> {
        let round = self.calls.fetch_add(1, Ordering::SeqCst);
        let events = if round == 0 {
            // 断言装配确实把工具定义下发给了提供商——注册表接线断了的话这里就是空的。
            assert!(
                request.tools.iter().any(|tool| tool.name == "read"),
                "第一轮请求必须带上 read 工具定义，实得：{:?}",
                request.tools.iter().map(|t| &t.name).collect::<Vec<_>>()
            );
            let call = ToolCall {
                id: "call_read_1".to_owned(),
                name: "read".to_owned(),
                arguments: format!(r#"{{"path":"{}"}}"#, self.target),
            };
            vec![
                Ok(StreamEvent::ToolCallStart {
                    index: 0,
                    id: call.id.clone(),
                    name: call.name.clone(),
                }),
                Ok(StreamEvent::ToolCallDelta {
                    index: 0,
                    delta: call.arguments.clone(),
                }),
                Ok(StreamEvent::ToolCallEnd {
                    index: 0,
                    tool_call: call,
                }),
                Ok(StreamEvent::Done {
                    stop_reason: StopReason::ToolUse,
                    usage: Usage::default(),
                }),
            ]
        } else {
            vec![
                Ok(StreamEvent::TextStart { index: 0 }),
                Ok(StreamEvent::TextDelta {
                    index: 0,
                    delta: "读到了".to_owned(),
                }),
                Ok(StreamEvent::TextEnd {
                    index: 0,
                    text: "读到了".to_owned(),
                }),
                Ok(StreamEvent::Done {
                    stop_reason: StopReason::Stop,
                    usage: Usage::default(),
                }),
            ]
        };
        Ok(Box::pin(futures_util::stream::iter(events)))
    }
}

/// 一份指向临时目录、禁用 daemon 的配置。
fn test_config(root: &Path) -> Config {
    Config {
        model: ModelConfig {
            id: Some("smoke-model".to_owned()),
            thinking: None,
            provider: None,
        },
        approval: ApprovalConfig {
            // 冒烟不测审批回环（`host::client` 与 `app::pending` 各自测过）：
            // 这里要的是工具真的跑起来。
            mode: ApprovalMode::Yolo,
            policies: HashMap::new(),
        },
        tools: ToolsConfig {
            disabled: Vec::new(),
            bash_timeout_secs: 30,
            read_max_lines: 300,
        },
        session: SessionConfig {
            dir: root.join("sessions"),
        },
        daemon: DaemonConfig {
            // 走自托管分支——这正是无 daemon 时的生产路径。
            enabled: false,
            runtime_dir: root.join("run"),
        },
        ui: UiConfig {
            show_thinking: false,
        },
    }
}

fn test_model() -> ResolvedModel {
    ResolvedModel {
        id: "smoke-model".to_owned(),
        provider: ProviderId::Anthropic,
        context_window: 200_000,
        thinking: Thinking::Disabled,
    }
}

/// 生产实现装配 + 自托管连接 + 握手。除 provider 外一个替身都没有。
async fn bring_up(dir: &Path, root: &Path) -> (Arc<Config>, ClientSession) {
    let config = Arc::new(test_config(dir));
    let workspace = Arc::new(Workspace::new(root.to_path_buf()));

    let registry = crate::tools::registry(config.as_ref(), &workspace).expect("装配工具注册表");
    assert_eq!(registry.len(), 8, "八个内置工具都要注册进去");
    let prompts = crate::prompt::build(workspace.as_ref(), config.as_ref(), &test_model())
        .await
        .expect("装配 system prompt");
    assert!(
        prompts.system.first().is_some_and(|s| !s.is_empty()),
        "system prompt 的第 0 段（缓存前缀）不能为空"
    );

    let provider = Arc::new(ScriptedProvider {
        calls: AtomicUsize::new(0),
        target: "hello.txt".to_owned(),
    });
    let deps_config = Arc::clone(&config);
    let deps_workspace = Arc::clone(&workspace);
    let deps_registry = Arc::new(registry);
    let deps_prompts = Arc::new(prompts);

    let connection = connect::connect(config.as_ref(), root, move |secret| {
        async move {
            Ok(HostDeps {
                provider,
                registry: deps_registry,
                sessions_dir: deps_config.session.dir.clone(),
                config: deps_config,
                prompts: deps_prompts,
                model: test_model(),
                workspace: deps_workspace,
                secret,
            })
        }
        .boxed()
    })
    .await
    .expect("自托管连接失败");
    assert_eq!(
        connection.mode,
        connect::ConnectMode::SelfHosted,
        "daemon.enabled=false 必须走自托管"
    );

    let session = ClientSession::open(connection).await.expect("三帧握手失败");
    (config, session)
}

/// 整条链路跑一次：模型要求读文件 → 工具真读磁盘 → 结果回喂 → 模型给出最终答案。
///
/// 断言落在**会话 JSONL 的实际内容**上，而不是渲染出来的文本：落盘才是事实来源
/// （`crates/agent/src/lib.rs:26-29`），渲染是可丢的增量。
#[tokio::test]
async fn full_stack_turn_executes_a_real_tool_and_persists_history() {
    let dir = tempfile::tempdir().expect("临时目录");
    let root = dir.path().join("workspace");
    std::fs::create_dir_all(&root).expect("建工作区");
    std::fs::write(root.join("hello.txt"), "第一行\n第二行\n").expect("写测试文件");

    let (config, session) = bring_up(dir.path(), &root).await;

    let Reply::SessionCreated { summary, .. } = session
        .request(Request::SessionCreate {
            // host 会忽略这个自报值，改用 workspace 根（防止客户端把会话建到任意目录）。
            cwd: "/ignored-by-host".to_owned(),
            model: "smoke-model".to_owned(),
        })
        .await
        .expect("建会话失败")
    else {
        panic!("SessionCreate 必须回 SessionCreated");
    };
    let target: SessionId = summary.id.clone();
    assert_eq!(
        summary.cwd,
        root.to_string_lossy(),
        "会话 cwd 必须来自 workspace 根，不能采信客户端自报值"
    );

    let Reply::Subscribed { .. } = session
        .request(Request::Subscribe {
            session: target.clone(),
            client: ClientId::from("smoke-client"),
            has_local_history: false,
            takeover: false,
            since: None,
        })
        .await
        .expect("订阅失败")
    else {
        panic!("Subscribe 必须回 Subscribed");
    };

    let Reply::TurnStarted { .. } = session
        .request(Request::Prompt {
            session: target.clone(),
            content: vec![UserContent::Text {
                text: "看看 hello.txt".to_owned(),
            }],
        })
        .await
        .expect("发起 turn 失败")
    else {
        panic!("Prompt 必须回 TurnStarted");
    };

    // turn 属于 session、不属于连接，所以要等它自己跑完。轮询会话文件而不是睡固定时长：
    // 固定睡眠在慢机器上是 flaky 的来源。
    let session_file = config.session.dir.join(format!("{target}.jsonl"));
    let history = wait_for_final_answer(&session_file).await;

    let kinds: Vec<&EntryKind> = history.iter().collect();
    assert!(
        kinds.iter().any(|kind| matches!(
            kind,
            EntryKind::Message {
                message: StoredMessage::ToolResult { tool_name, .. }
            } if tool_name == "read"
        )),
        "历史里必须有一条 read 的工具结果，实得：{kinds:?}"
    );

    let tool_output = history
        .iter()
        .find_map(|kind| match kind {
            EntryKind::Message {
                message: StoredMessage::ToolResult { content, .. },
            } => Some(content.clone()),
            _ => None,
        })
        .expect("已断言存在");
    let rendered = format!("{tool_output:?}");
    assert!(
        rendered.contains("第一行") && rendered.contains("第二行"),
        "read 必须真的读到磁盘内容，实得：{rendered}"
    );

    session.shutdown().await.expect("关闭连接失败");
}

/// 轮询会话文件直到出现助手的最终文本消息，返回全部条目。
///
/// 上限 30 秒：本地一切都是内存与临时目录，真跑到这个数只可能是死锁。
async fn wait_for_final_answer(path: &Path) -> Vec<EntryKind> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        if path.exists() {
            let store = SessionStore::open(path).await.expect("打开会话文件");
            let entries: Vec<EntryKind> = store
                .tree()
                .branch()
                .into_iter()
                .map(|entry| entry.kind.clone())
                .collect();
            let done = entries.iter().any(|kind| {
                matches!(
                    kind,
                    EntryKind::Message {
                        message: StoredMessage::Assistant { content, .. }
                    } if content
                        .iter()
                        .any(|block| matches!(block, StoredAssistantContent::Text { .. }))
                )
            });
            if done {
                return entries;
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "等待 turn 完成超时——最终的助手文本消息始终没有落盘"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}
