//! 端到端流式冒烟测试。
//!
//! 单元测试各自只覆盖一段（请求编码 / SSE 解码 / 凭据解析）。这里把整条链路真的
//! 跑一遍：从 [`AuthStore`] 取凭据 → 组请求 → 走真实 HTTP → 解 SSE → 产出统一事件。
//! 上游换成本机 mock 服务器，除了远端本身，其余每一环都是生产代码。

// clippy 的 `allow-expect-in-tests` 只认 `#[test]` 函数体，识别不出集成测试里的
// 辅助函数；测试代码本就不受库代码的 panic 约束（见 rule://rust-testing）。
#![expect(clippy::expect_used, reason = "集成测试的辅助函数，失败即测试失败")]

use std::sync::Arc;

use futures_util::StreamExt as _;
use zcode_ai::auth::AuthStore;
use zcode_ai::auth::credential::{ApiKeyCredential, Credential, OAuthCredential, now_ms};
use zcode_ai::auth::store::{CredentialStore, MemoryCredentialStore};
use zcode_ai::provider::anthropic::AnthropicProvider;
use zcode_ai::provider::openai_chat::{ChatConfig, ChatProvider};
use zcode_ai::provider::openai_responses::{ResponsesConfig, ResponsesProvider};
use zcode_ai::{
    AiError, CompletionRequest, Message, Provider, ProviderId, StopReason, StreamEvent, Usage,
};

fn auth_with(provider: ProviderId, credential: Credential) -> Arc<AuthStore> {
    let store = Arc::new(MemoryCredentialStore::new());
    store.save(provider, credential).expect("预置凭据");
    let erased: Arc<dyn CredentialStore> = store;
    Arc::new(AuthStore::bare(erased))
}

fn api_key_auth(provider: ProviderId) -> Arc<AuthStore> {
    auth_with(
        provider,
        Credential::ApiKey(ApiKeyCredential {
            key: "sk-test".to_owned(),
        }),
    )
}

fn oauth_auth(provider: ProviderId) -> Arc<AuthStore> {
    auth_with(
        provider,
        Credential::Oauth(OAuthCredential {
            access: "oat-token".to_owned(),
            refresh: "rt".to_owned(),
            expires: now_ms() + 3_600_000,
            account_id: Some("acct-1".to_owned()),
            email: None,
            plan: None,
            authorized_at: None,
        }),
    )
}

fn request() -> CompletionRequest {
    CompletionRequest::new("test-model", vec![Message::user("你好")])
}

/// 拉完整条流并把事件按类型压平，便于断言。
async fn drain(
    provider: &dyn Provider,
    request: &CompletionRequest,
) -> (Vec<StreamEvent>, Vec<AiError>) {
    let mut stream = provider.stream(request).await.expect("发起流式请求");
    let mut events = Vec::new();
    let mut errors = Vec::new();
    while let Some(item) = stream.next().await {
        match item {
            Ok(event) => events.push(event),
            Err(err) => errors.push(err),
        }
    }
    (events, errors)
}

fn text_of(events: &[StreamEvent]) -> String {
    events
        .iter()
        .filter_map(|event| match event {
            StreamEvent::TextDelta { delta, .. } => Some(delta.as_str()),
            _ => None,
        })
        .collect()
}

fn done_of(events: &[StreamEvent]) -> Option<(StopReason, Usage)> {
    events.iter().find_map(|event| match event {
        StreamEvent::Done { stop_reason, usage } => Some((*stop_reason, *usage)),
        _ => None,
    })
}

#[tokio::test]
async fn anthropic_streams_end_to_end_over_http() {
    let mut server = mockito::Server::new_async().await;
    let body = concat!(
        "event: message_start\n",
        r#"data: {"type":"message_start","message":{"id":"msg_1","model":"claude-sonnet-4-6","usage":{"input_tokens":9}}}"#,
        "\n\n",
        "event: content_block_start\n",
        r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"text"}}"#,
        "\n\n",
        "event: content_block_delta\n",
        r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"你好"}}"#,
        "\n\n",
        "event: content_block_stop\n",
        r#"data: {"type":"content_block_stop","index":0}"#,
        "\n\n",
        "event: message_delta\n",
        r#"data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":4}}"#,
        "\n\n",
    );
    let mock = server
        .mock("POST", "/v1/messages")
        .match_header("x-api-key", "sk-test")
        .match_header("anthropic-version", "2023-06-01")
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(body)
        .create_async()
        .await;

    let provider = AnthropicProvider::new(api_key_auth(ProviderId::Anthropic))
        .expect("构造适配器")
        .with_base_url(server.url());
    let (events, errors) = drain(&provider, &request()).await;

    mock.assert_async().await;
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(text_of(&events), "你好");
    assert_eq!(
        done_of(&events),
        Some((
            StopReason::Stop,
            Usage {
                input: 9,
                output: 4,
                ..Usage::default()
            }
        ))
    );
}

#[tokio::test]
async fn anthropic_oauth_requests_carry_the_claude_code_fingerprint() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/v1/messages?beta=true")
        .match_header("authorization", "Bearer oat-token")
        .match_header(
            "user-agent",
            mockito::Matcher::Regex("^claude-cli/".to_owned()),
        )
        .match_header(
            "anthropic-beta",
            mockito::Matcher::Regex("claude-code-".to_owned()),
        )
        .match_body(mockito::Matcher::PartialJson(serde_json::json!({
            "system": [{
                "type": "text",
                "text": "You are a Claude agent, built on Anthropic's Claude Agent SDK."
            }]
        })))
        .with_status(200)
        .with_body(concat!(
            "event: message_start\n",
            r#"data: {"type":"message_start","message":{"id":"m"}}"#,
            "\n\n",
            "event: message_stop\n",
            r#"data: {"type":"message_stop"}"#,
            "\n\n",
        ))
        .create_async()
        .await;

    let provider = AnthropicProvider::new(oauth_auth(ProviderId::Anthropic))
        .expect("构造适配器")
        .with_base_url(server.url());
    let (events, errors) = drain(&provider, &request()).await;

    mock.assert_async().await;
    assert!(errors.is_empty(), "{errors:?}");
    assert!(done_of(&events).is_some());
}

#[tokio::test]
async fn chat_completions_streams_text_and_tool_calls_end_to_end() {
    let mut server = mockito::Server::new_async().await;
    let body = concat!(
        r#"data: {"id":"c1","model":"gpt-5.2","choices":[{"delta":{"content":"你好"}}]}"#,
        "\n\n",
        r#"data: {"id":"c1","choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"search","arguments":"{\"q\":1}"}}]}}]}"#,
        "\n\n",
        r#"data: {"id":"c1","choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
        "\n\n",
        r#"data: {"id":"c1","choices":[],"usage":{"prompt_tokens":11,"completion_tokens":3}}"#,
        "\n\n",
        "data: [DONE]\n\n",
    );
    let mock = server
        .mock("POST", "/chat/completions")
        .match_header("authorization", "Bearer sk-test")
        .match_body(mockito::Matcher::PartialJson(serde_json::json!({
            "stream": true,
            "stream_options": { "include_usage": true }
        })))
        .with_status(200)
        .with_body(body)
        .create_async()
        .await;

    let provider = ChatProvider::new(api_key_auth(ProviderId::OpenAi), ChatConfig::openai())
        .expect("构造适配器")
        .with_base_url(server.url());
    let (events, errors) = drain(&provider, &request()).await;

    mock.assert_async().await;
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(text_of(&events), "你好");
    assert!(events.iter().any(|event| matches!(
        event,
        StreamEvent::ToolCallEnd { tool_call, .. }
            if tool_call.name == "search" && tool_call.arguments == r#"{"q":1}"#
    )));
    assert_eq!(
        done_of(&events),
        Some((
            StopReason::ToolUse,
            Usage {
                input: 11,
                output: 3,
                ..Usage::default()
            }
        ))
    );
}

#[tokio::test]
async fn responses_streams_end_to_end_over_http() {
    let mut server = mockito::Server::new_async().await;
    let body = concat!(
        "event: response.created\n",
        r#"data: {"type":"response.created","response":{"id":"resp_1","model":"gpt-5.2"}}"#,
        "\n\n",
        "event: response.output_text.delta\n",
        r#"data: {"type":"response.output_text.delta","item_id":"msg_1","output_index":0,"content_index":0,"delta":"你好"}"#,
        "\n\n",
        "event: response.completed\n",
        r#"data: {"type":"response.completed","response":{"id":"resp_1","status":"completed","usage":{"input_tokens":7,"output_tokens":2}}}"#,
        "\n\n",
    );
    let mock = server
        .mock("POST", "/responses")
        .match_body(mockito::Matcher::PartialJson(serde_json::json!({
            "stream": true,
            "store": false,
            "include": ["reasoning.encrypted_content"]
        })))
        .with_status(200)
        .with_body(body)
        .create_async()
        .await;

    let provider =
        ResponsesProvider::new(api_key_auth(ProviderId::OpenAi), ResponsesConfig::openai())
            .expect("构造适配器")
            .with_base_url(server.url());
    let (events, errors) = drain(&provider, &request()).await;

    mock.assert_async().await;
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(text_of(&events), "你好");
    assert_eq!(
        done_of(&events),
        Some((
            StopReason::Stop,
            Usage {
                input: 7,
                output: 2,
                ..Usage::default()
            }
        ))
    );
}

#[tokio::test]
async fn http_errors_are_classified_before_any_event_is_emitted() {
    let mut server = mockito::Server::new_async().await;
    drop(
        server
            .mock("POST", "/v1/messages")
            .with_status(429)
            .with_header("retry-after", "3")
            .with_body(r#"{"error":{"type":"rate_limit_error","message":"slow down"}}"#)
            .create_async()
            .await,
    );

    let provider = AnthropicProvider::new(api_key_auth(ProviderId::Anthropic))
        .expect("构造适配器")
        .with_base_url(server.url());
    let Err(err) = provider.stream(&request()).await else {
        panic!("429 应当直接失败");
    };

    match &err {
        AiError::Api {
            status,
            code,
            retry_after,
            ..
        } => {
            assert_eq!(*status, 429);
            assert_eq!(code.as_deref(), Some("rate_limit_error"));
            assert_eq!(*retry_after, Some(std::time::Duration::from_secs(3)));
        }
        other => panic!("期望 Api 错误，实际 {other:?}"),
    }
    assert!(err.is_retryable());
}

#[tokio::test]
async fn a_connection_dropped_mid_stream_never_reports_success() {
    let mut server = mockito::Server::new_async().await;
    // 只发到一半就结束响应体，模拟连接被中途切断。
    drop(
        server
            .mock("POST", "/chat/completions")
            .with_status(200)
            .with_body(concat!(
                r#"data: {"id":"c1","choices":[{"delta":{"content":"半"}}]}"#,
                "\n\n",
            ))
            .create_async()
            .await,
    );

    let provider = ChatProvider::new(api_key_auth(ProviderId::OpenAi), ChatConfig::openai())
        .expect("构造适配器")
        .with_base_url(server.url());
    let (events, errors) = drain(&provider, &request()).await;

    assert!(
        !events
            .iter()
            .any(|event| matches!(event, StreamEvent::Done { .. })),
        "截断的流不该产生 Done"
    );
    assert!(
        matches!(errors.first(), Some(AiError::Protocol { .. })),
        "{errors:?}"
    );
}

#[tokio::test]
async fn missing_credentials_fail_before_any_network_call() {
    let store = Arc::new(MemoryCredentialStore::new());
    let erased: Arc<dyn CredentialStore> = store;
    let auth = Arc::new(AuthStore::bare(erased));

    let provider = AnthropicProvider::new(auth).expect("构造适配器");
    let Err(err) = provider.stream(&request()).await else {
        panic!("没有凭据应当失败");
    };
    assert!(err.is_auth_failure(), "{err:?}");
}
