//! OpenAI Chat Completions 适配器。
//!
//! 同时服务 `openai` 与 `xai` —— 两家线格式一致，差异（base URL、缓存亲和头、
//! 是否允许 `reasoning_effort`）全部收在 [`ChatConfig`] 里，不另建一套实现。
//!
//! 流式解析里最容易出错的是**工具调用增量的对齐**：多数提供商在
//! `tool_calls[].index` 上标号，但也有只发数组位置、甚至只在首帧发 id 的。三种
//! 情况都要能拼回同一个调用，见 `tool_call_key`。

use std::sync::Arc;

use async_trait::async_trait;
use futures_util::StreamExt as _;
use reqwest::Client;
use reqwest::header::HeaderMap;

use crate::auth::AuthStore;
use crate::auth::credential::Access;
use crate::error::{AiError, ApiErrorKind};
use crate::http;
use crate::provider::block::Blocks;
use crate::provider::{EventStream, Provider};
use crate::sse::SseEvent;
use crate::types::{
    AssistantContent, CompletionRequest, Effort, Message, ProviderId, StopReason, StreamEvent,
    Thinking, Tool, ToolChoice, ToolResultContent, Usage, UserContent,
};

/// OpenAI 平台默认 base URL。
pub const OPENAI_BASE_URL: &str = "https://api.openai.com/v1";

/// Chat Completions 路径。
const CHAT_PATH: &str = "/chat/completions";

/// 线格式差异的开关集合。
#[derive(Debug, Clone)]
pub struct ChatConfig {
    /// 归属提供商。
    pub provider: ProviderId,
    /// API 根地址，不带尾斜杠。
    pub base_url: String,
    /// system 提示用的角色名：推理模型用 `developer`，其余用 `system`。
    pub system_role: &'static str,
    /// 输出上限字段名：新模型是 `max_completion_tokens`，老接口是 `max_tokens`。
    pub max_tokens_field: &'static str,
    /// 是否下发 `reasoning_effort`。
    pub supports_reasoning_effort: bool,
    /// prompt 缓存亲和头（xAI 是 `x-grok-conv-id`），`None` 表示不发。
    pub cache_session_header: Option<&'static str>,
    /// 回放历史时是否把思考写进 `reasoning_content`。
    ///
    /// OpenAI 与 xAI 的原生接口都**不**定义这个字段，默认关闭；只有 `DeepSeek` /
    /// Kimi / llama.cpp 这类要求回传思考的兼容端点才打开。
    pub replay_reasoning_content: bool,
}

impl ChatConfig {
    /// OpenAI 平台的默认配置。
    #[must_use]
    pub fn openai() -> Self {
        Self {
            provider: ProviderId::OpenAi,
            base_url: OPENAI_BASE_URL.to_owned(),
            system_role: "developer",
            max_tokens_field: "max_completion_tokens",
            supports_reasoning_effort: true,
            cache_session_header: None,
            replay_reasoning_content: false,
        }
    }
}

/// Chat Completions 适配器。
#[derive(Debug, Clone)]
pub struct ChatProvider {
    auth: Arc<AuthStore>,
    client: Client,
    config: ChatConfig,
}

impl ChatProvider {
    /// 用共享 HTTP 客户端构造。
    pub fn new(auth: Arc<AuthStore>, config: ChatConfig) -> Result<Self, AiError> {
        Ok(Self {
            auth,
            client: http::shared_client()?,
            config,
        })
    }

    /// 覆盖 HTTP 客户端。
    #[must_use]
    pub fn with_client(mut self, client: Client) -> Self {
        self.client = client;
        self
    }

    /// 覆盖 base URL。
    #[must_use]
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.config.base_url = http::normalize_base_url(base_url.into());
        self
    }

    fn endpoint(&self) -> String {
        format!("{}{CHAT_PATH}", self.config.base_url)
    }

    fn headers(&self, access: &Access, request: &CompletionRequest) -> HeaderMap {
        let mut headers = HeaderMap::new();
        http::set_header(&mut headers, "content-type", "application/json");
        http::set_header(&mut headers, "accept", "text/event-stream");
        http::set_header(
            &mut headers,
            "authorization",
            &format!("Bearer {}", access.token),
        );
        if let Some(name) = self.config.cache_session_header
            && let Some(key) = request
                .prompt_cache_key
                .as_deref()
                .or(request.session_id.as_deref())
        {
            http::set_header(&mut headers, name, key);
        }
        headers
    }

    fn body(&self, request: &CompletionRequest) -> serde_json::Value {
        let mut body = serde_json::Map::new();
        drop(body.insert("model".to_owned(), request.model.clone().into()));
        drop(body.insert("messages".to_owned(), self.encode_messages(request)));
        drop(body.insert("stream".to_owned(), true.into()));
        // 不开这个就拿不到 usage，成本统计会全是 0。
        drop(body.insert(
            "stream_options".to_owned(),
            serde_json::json!({ "include_usage": true }),
        ));

        if let Some(limit) = request.max_output_tokens {
            drop(body.insert(self.config.max_tokens_field.to_owned(), limit.into()));
        }
        if !request.tools.is_empty() {
            drop(body.insert("tools".to_owned(), encode_tools(&request.tools)));
            if let Some(choice) = encode_tool_choice(&request.tool_choice) {
                drop(body.insert("tool_choice".to_owned(), choice));
            }
        }
        if let Some(effort) = self.reasoning_effort(request.thinking) {
            drop(body.insert("reasoning_effort".to_owned(), effort.into()));
        }
        if let Some(temperature) = request.temperature {
            drop(body.insert("temperature".to_owned(), temperature.into()));
        }
        if let Some(top_p) = request.top_p {
            drop(body.insert("top_p".to_owned(), top_p.into()));
        }
        if !request.stop_sequences.is_empty() {
            let stops: Vec<serde_json::Value> = request
                .stop_sequences
                .iter()
                .take(4)
                .map(|s| s.clone().into())
                .collect();
            drop(body.insert("stop".to_owned(), stops.into()));
        }
        if let Some(tier) = request.service_tier {
            drop(body.insert("service_tier".to_owned(), tier.as_str().into()));
        }
        if let Some(key) = request.prompt_cache_key.as_deref() {
            drop(body.insert("prompt_cache_key".to_owned(), key.into()));
        }
        serde_json::Value::Object(body)
    }

    fn reasoning_effort(&self, thinking: Thinking) -> Option<&'static str> {
        if !self.config.supports_reasoning_effort {
            return None;
        }
        match thinking {
            Thinking::Disabled => None,
            Thinking::Effort(effort) => Some(effort.as_str()),
            // Chat Completions 没有 token 预算旋钮，只能折成档位。
            Thinking::Budget { .. } => Some(Effort::Medium.as_str()),
        }
    }

    fn encode_messages(&self, request: &CompletionRequest) -> serde_json::Value {
        let mut out: Vec<serde_json::Value> = Vec::new();
        for prompt in &request.system {
            if !prompt.is_empty() {
                out.push(serde_json::json!({ "role": self.config.system_role, "content": prompt }));
            }
        }
        for message in &request.messages {
            match message {
                Message::User { content } => out.push(serde_json::json!({
                    "role": "user",
                    "content": content.iter().map(encode_user_content).collect::<Vec<_>>(),
                })),
                Message::Assistant { content } => {
                    out.push(encode_assistant(
                        content,
                        self.config.replay_reasoning_content,
                    ));
                }
                Message::ToolResult {
                    tool_call_id,
                    tool_name,
                    content,
                    ..
                } => {
                    out.push(serde_json::json!({
                        "role": "tool",
                        "tool_call_id": tool_call_id,
                        "name": tool_name,
                        "content": flatten_tool_result(content),
                    }));
                    // `tool` 角色只收文本；图片必须另起一条 user 消息，否则视觉
                    // 信息全丢。Responses 那条线也是这么处理的。
                    if let Some(attachment) = attach_tool_images(content) {
                        out.push(attachment);
                    }
                }
            }
        }
        out.into()
    }
}

#[async_trait]
impl Provider for ChatProvider {
    fn id(&self) -> ProviderId {
        self.config.provider
    }

    async fn stream(&self, request: &CompletionRequest) -> Result<EventStream, AiError> {
        let provider = self.config.provider;
        let access = self.auth.access(provider).await?;
        let http_request = self
            .client
            .post(self.endpoint())
            .headers(self.headers(&access, request))
            .json(&self.body(request));

        let events = http::send_sse(provider, http_request).await?;
        Ok(Box::pin(decode(provider, events)))
    }
}

struct DecodeState<S> {
    provider: ProviderId,
    events: std::pin::Pin<Box<S>>,
    blocks: Blocks,
    pending: std::collections::VecDeque<StreamEvent>,
    usage: Usage,
    stop_reason: Option<StopReason>,
    started: bool,
    finished: bool,
}

/// 把 Chat Completions 的 SSE 流翻成统一事件流。
fn decode<S>(
    provider: ProviderId,
    events: S,
) -> impl futures_core::Stream<Item = Result<StreamEvent, AiError>> + Send
where
    S: futures_core::Stream<Item = Result<SseEvent, AiError>> + Send + 'static,
{
    let state = DecodeState {
        provider,
        events: Box::pin(events),
        blocks: Blocks::new(),
        pending: std::collections::VecDeque::new(),
        usage: Usage::default(),
        stop_reason: None,
        started: false,
        finished: false,
    };

    futures_util::stream::unfold(state, |mut state| async move {
        loop {
            if let Some(event) = state.pending.pop_front() {
                return Some((Ok(event), state));
            }
            if state.finished {
                return None;
            }
            match state.events.next().await {
                Some(Ok(sse)) if sse.is_done_sentinel() => {
                    if let Some(err) = finish(&mut state) {
                        return Some((Err(err), state));
                    }
                }
                Some(Ok(sse)) => {
                    let mut out = Vec::new();
                    if let Err(err) = handle_chunk(&sse, &mut state, &mut out) {
                        state.finished = true;
                        return Some((Err(err), state));
                    }
                    state.pending.extend(out);
                }
                Some(Err(err)) => {
                    state.finished = true;
                    return Some((Err(err), state));
                }
                None => {
                    if let Some(err) = finish(&mut state) {
                        return Some((Err(err), state));
                    }
                }
            }
        }
    })
}

/// 收尾。
///
/// 只有见过 `finish_reason` 才算协议意义上的正常终止。缺了它就说明连接断在中途，
/// 此刻手上是半截文本或没闭合的工具 JSON，报成功等于把残缺内容当完整结果交出去，
/// 所以返回 [`AiError::Protocol`] 而不是补一个 `Done`。
fn finish<S>(state: &mut DecodeState<S>) -> Option<AiError> {
    state.finished = true;
    let Some(stop_reason) = state.stop_reason else {
        return Some(AiError::Protocol {
            provider: state.provider,
            detail: "流在收到 finish_reason 之前就结束了".to_owned(),
        });
    };
    let mut out = Vec::new();
    state.blocks.close_all(&mut out);
    // 有工具调用却报 stop 的提供商不少，这里统一提升。
    let stop_reason = if stop_reason == StopReason::Stop && state.blocks.has_tool_calls() {
        StopReason::ToolUse
    } else {
        stop_reason
    };
    out.push(StreamEvent::Done {
        stop_reason,
        usage: state.usage,
    });
    state.pending.extend(out);
    None
}

fn handle_chunk<S>(
    sse: &SseEvent,
    state: &mut DecodeState<S>,
    out: &mut Vec<StreamEvent>,
) -> Result<(), AiError> {
    let chunk: serde_json::Value =
        serde_json::from_str(&sse.data).map_err(|source| AiError::Decode {
            provider: state.provider,
            source,
        })?;

    // 少数网关会把错误塞进正常的 chunk 里。
    if let Some(error) = chunk.get("error").filter(|value| !value.is_null()) {
        return Err(AiError::Api {
            provider: state.provider,
            status: 200,
            kind: ApiErrorKind::Upstream,
            code: str_field(error, "code").or_else(|| str_field(error, "type")),
            message: str_field(error, "message")
                .unwrap_or_else(|| "上游在流中返回了未描述的错误".to_owned()),
            retry_after: None,
        });
    }

    if !state.started {
        state.started = true;
        out.push(StreamEvent::Start {
            response_id: str_field(&chunk, "id"),
            model: str_field(&chunk, "model"),
        });
    }

    // usage 可能出现在顶层，也可能挂在 choice 上；末包常常没有 choices。
    if let Some(raw) = chunk.get("usage").filter(|value| !value.is_null()) {
        state.usage = parse_usage(raw);
    }

    let Some(choice) = chunk.get("choices").and_then(|c| c.get(0)) else {
        return Ok(());
    };
    if let Some(raw) = choice.get("usage").filter(|value| !value.is_null()) {
        state.usage = parse_usage(raw);
    }
    if let Some(delta) = choice.get("delta") {
        apply_delta(delta, &mut state.blocks, out);
    }
    if let Some(reason) = str_field(choice, "finish_reason") {
        state.stop_reason = Some(map_finish_reason(&reason));
    }
    Ok(())
}

fn apply_delta(delta: &serde_json::Value, blocks: &mut Blocks, out: &mut Vec<StreamEvent>) {
    if let Some(text) = delta.get("content").and_then(content_to_text)
        && !text.is_empty()
    {
        blocks.text_delta("text", &text, out);
    }
    // 各家推理字段名不统一，按优先级取第一个非空的。
    if let Some(thinking) = ["reasoning_content", "reasoning", "reasoning_text"]
        .into_iter()
        .find_map(|key| delta.get(key).and_then(content_to_text))
        .filter(|text| !text.is_empty())
    {
        blocks.thinking_delta("thinking", &thinking, out);
    }
    let Some(calls) = delta
        .get("tool_calls")
        .and_then(serde_json::Value::as_array)
    else {
        return;
    };
    for (offset, call) in calls.iter().enumerate() {
        let key = tool_call_key(call, offset);
        let id = str_field(call, "id").unwrap_or_default();
        let function = call.get("function");
        let name = function
            .and_then(|f| str_field(f, "name"))
            .unwrap_or_default();
        if !id.is_empty() || !name.is_empty() {
            blocks.tool_start(&key, &id, &name, out);
        }
        match function.and_then(|f| f.get("arguments")) {
            Some(serde_json::Value::String(fragment)) if !fragment.is_empty() => {
                blocks.tool_delta(&key, fragment, out);
            }
            // MiniMax 一类会直接给对象而不是 JSON 字符串。
            Some(object @ serde_json::Value::Object(_)) => {
                blocks.set_tool_arguments(&key, &object.to_string(), out);
            }
            _ => {
                // 只带 id/name 的首帧，确保块已经开出来。
                blocks.tool_start(&key, &id, &name, out);
            }
        }
    }
}

/// 决定一条 `tool_calls[]` 增量属于哪个调用。
///
/// 优先用服务端给的 `index`；没有 index 就用 `id`；两者都没有（部分自建网关）
/// 才退到数组位置。退到位置是最后手段——它只在单帧内成立。
fn tool_call_key(call: &serde_json::Value, offset: usize) -> String {
    if let Some(index) = call.get("index").and_then(serde_json::Value::as_u64) {
        return format!("tool:{index}");
    }
    if let Some(id) = call
        .get("id")
        .and_then(serde_json::Value::as_str)
        .filter(|id| !id.is_empty())
    {
        return format!("tool-id:{id}");
    }
    format!("tool-pos:{offset}")
}

/// `content` 既可能是字符串，也可能是 `[{type:"text",text:".."}]`。
fn content_to_text(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(text) => Some(text.clone()),
        serde_json::Value::Array(parts) => {
            let mut text = String::new();
            for part in parts {
                if let Some(chunk) = part.get("text").and_then(serde_json::Value::as_str) {
                    text.push_str(chunk);
                }
            }
            Some(text)
        }
        _ => None,
    }
}

fn parse_usage(raw: &serde_json::Value) -> Usage {
    let prompt = u64_field(raw, "prompt_tokens").unwrap_or(0);
    let cache_read = first_positive(&[
        raw.get("prompt_tokens_details")
            .and_then(|d| u64_field(d, "cached_tokens")),
        u64_field(raw, "prompt_cache_hit_tokens"),
        u64_field(raw, "cached_tokens"),
    ]);
    let cache_write = raw
        .get("prompt_tokens_details")
        .and_then(|d| u64_field(d, "cache_write_tokens"))
        .unwrap_or(0);
    Usage {
        // `prompt_tokens` 是含缓存的总量，扣掉才不会重复计数。
        input: prompt
            .saturating_sub(cache_read)
            .saturating_sub(cache_write),
        output: u64_field(raw, "completion_tokens").unwrap_or(0),
        cache_read,
        cache_write,
        reasoning: raw
            .get("completion_tokens_details")
            .and_then(|d| u64_field(d, "reasoning_tokens"))
            .unwrap_or(0),
    }
}

fn first_positive(candidates: &[Option<u64>]) -> u64 {
    candidates
        .iter()
        .flatten()
        .copied()
        .find(|value| *value > 0)
        .unwrap_or(0)
}

fn map_finish_reason(reason: &str) -> StopReason {
    match reason {
        "length" => StopReason::Length,
        "tool_calls" | "function_call" => StopReason::ToolUse,
        "content_filter" | "error" | "network_error" => StopReason::Error,
        // `stop` / `end` / 未知取值都算正常结束。
        _ => StopReason::Stop,
    }
}

fn str_field(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

fn u64_field(value: &serde_json::Value, key: &str) -> Option<u64> {
    value.get(key).and_then(serde_json::Value::as_u64)
}

fn encode_user_content(content: &UserContent) -> serde_json::Value {
    match content {
        UserContent::Text(text) => serde_json::json!({ "type": "text", "text": text }),
        UserContent::Image(image) => serde_json::json!({
            "type": "image_url",
            "image_url": { "url": image.to_data_url() },
        }),
    }
}

fn encode_assistant(
    content: &[AssistantContent],
    replay_reasoning_content: bool,
) -> serde_json::Value {
    let mut text = String::new();
    let mut reasoning = String::new();
    let mut calls: Vec<serde_json::Value> = Vec::new();
    for item in content {
        match item {
            AssistantContent::Text(chunk) => text.push_str(chunk),
            AssistantContent::Thinking(thinking) => reasoning.push_str(&thinking.text),
            // 加密思考无法在 Chat Completions 里回放，丢弃比发坏数据好。
            AssistantContent::RedactedThinking(_) => {}
            AssistantContent::ToolCall(call) => calls.push(serde_json::json!({
                "id": call.id,
                "type": "function",
                "function": { "name": call.name, "arguments": call.arguments },
            })),
        }
    }
    let mut message = serde_json::Map::new();
    drop(message.insert("role".to_owned(), "assistant".into()));
    // 有工具调用时 `content` 必须存在（可以是空串），不能是 null。
    drop(message.insert("content".to_owned(), text.into()));
    if replay_reasoning_content && !reasoning.is_empty() {
        drop(message.insert("reasoning_content".to_owned(), reasoning.into()));
    }
    if !calls.is_empty() {
        drop(message.insert("tool_calls".to_owned(), calls.into()));
    }
    serde_json::Value::Object(message)
}

/// Chat Completions 的 `tool` 消息只收文本；图片由 [`attach_tool_images`] 外挂。
fn flatten_tool_result(content: &[ToolResultContent]) -> String {
    let mut text = String::new();
    for item in content {
        if let ToolResultContent::Text(chunk) = item {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(chunk);
        }
    }
    text
}

/// 把工具结果里的图片包成紧随其后的一条 user 消息。
fn attach_tool_images(content: &[ToolResultContent]) -> Option<serde_json::Value> {
    let mut parts: Vec<serde_json::Value> = content
        .iter()
        .filter_map(|item| match item {
            ToolResultContent::Image(image) => Some(serde_json::json!({
                "type": "image_url",
                "image_url": { "url": image.to_data_url() },
            })),
            ToolResultContent::Text(_) => None,
        })
        .collect();
    if parts.is_empty() {
        return None;
    }
    parts.insert(
        0,
        serde_json::json!({ "type": "text", "text": "Attached image(s) from tool result:" }),
    );
    Some(serde_json::json!({ "role": "user", "content": parts }))
}

fn encode_tools(tools: &[Tool]) -> serde_json::Value {
    tools
        .iter()
        .map(|tool| {
            let mut function = serde_json::Map::new();
            drop(function.insert("name".to_owned(), tool.name.clone().into()));
            drop(function.insert("description".to_owned(), tool.description.clone().into()));
            drop(function.insert("parameters".to_owned(), tool.parameters.clone()));
            if let Some(strict) = tool.strict {
                drop(function.insert("strict".to_owned(), strict.into()));
            }
            serde_json::json!({ "type": "function", "function": function })
        })
        .collect::<Vec<_>>()
        .into()
}

fn encode_tool_choice(choice: &ToolChoice) -> Option<serde_json::Value> {
    match choice {
        ToolChoice::Auto => None,
        ToolChoice::None => Some("none".into()),
        ToolChoice::Required => Some("required".into()),
        ToolChoice::Named(name) => {
            Some(serde_json::json!({ "type": "function", "function": { "name": name } }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::credential::{AccessKind, ApiKeyCredential, Credential};
    use crate::auth::store::{CredentialStore, MemoryCredentialStore};
    use crate::types::{ImageContent, ThinkingContent, ToolCall};

    fn provider_with(config: ChatConfig) -> ChatProvider {
        let store = Arc::new(MemoryCredentialStore::new());
        store
            .save(
                config.provider,
                Credential::ApiKey(ApiKeyCredential {
                    key: "sk".to_owned(),
                }),
            )
            .expect("预置凭据");
        ChatProvider {
            auth: Arc::new(AuthStore::bare(store)),
            client: http::shared_client().expect("HTTP 客户端"),
            config,
        }
    }

    fn openai() -> ChatProvider {
        provider_with(ChatConfig::openai())
    }

    fn access() -> Access {
        Access {
            token: "sk".to_owned(),
            kind: AccessKind::ApiKey,
            account_id: None,
        }
    }

    fn request() -> CompletionRequest {
        CompletionRequest::new("gpt-5.2", vec![Message::user("hi")])
    }

    fn chunk(mut value: serde_json::Value) -> SseEvent {
        if let Some(object) = value.as_object_mut() {
            let _stamped = object
                .entry("object")
                .or_insert_with(|| "chat.completion.chunk".into());
        }
        SseEvent {
            event: None,
            data: value.to_string(),
        }
    }

    fn delta_chunk(delta: serde_json::Value) -> SseEvent {
        let mut choice = serde_json::Map::new();
        drop(choice.insert("delta".to_owned(), delta));
        chunk(serde_json::json!({
            "id": "c1", "model": "gpt-5.2", "choices": [serde_json::Value::Object(choice)]
        }))
    }

    async fn collect(events: Vec<SseEvent>) -> Vec<Result<StreamEvent, AiError>> {
        decode(
            ProviderId::OpenAi,
            futures_util::stream::iter(events.into_iter().map(Ok)),
        )
        .collect::<Vec<_>>()
        .await
    }

    async fn ok_events(events: Vec<SseEvent>) -> Vec<StreamEvent> {
        collect(events)
            .await
            .into_iter()
            .filter_map(Result::ok)
            .collect()
    }

    #[test]
    fn endpoint_appends_the_chat_path_to_the_base_url() {
        assert_eq!(
            openai().endpoint(),
            "https://api.openai.com/v1/chat/completions"
        );
    }

    #[test]
    fn authorization_uses_a_bearer_token() {
        let headers = openai().headers(&access(), &request());
        assert_eq!(
            headers.get("authorization").and_then(|v| v.to_str().ok()),
            Some("Bearer sk")
        );
    }

    #[test]
    fn cache_session_header_is_only_sent_when_configured() {
        let mut request = request();
        request.prompt_cache_key = Some("conv-1".to_owned());
        assert!(
            openai()
                .headers(&access(), &request)
                .get("x-grok-conv-id")
                .is_none()
        );

        let grok = provider_with(ChatConfig {
            cache_session_header: Some("x-grok-conv-id"),
            ..ChatConfig::openai()
        });
        assert_eq!(
            grok.headers(&access(), &request)
                .get("x-grok-conv-id")
                .and_then(|v| v.to_str().ok()),
            Some("conv-1")
        );
    }

    #[test]
    fn streaming_usage_is_always_requested() {
        let body = openai().body(&request());
        assert_eq!(body.get("stream"), Some(&serde_json::json!(true)));
        assert_eq!(
            body.get("stream_options"),
            Some(&serde_json::json!({ "include_usage": true }))
        );
    }

    #[test]
    fn output_limit_uses_the_configured_field_name() {
        let mut request = request();
        request.max_output_tokens = Some(4096);
        assert_eq!(
            openai().body(&request).get("max_completion_tokens"),
            Some(&serde_json::json!(4096))
        );

        let legacy = provider_with(ChatConfig {
            max_tokens_field: "max_tokens",
            ..ChatConfig::openai()
        });
        assert_eq!(
            legacy.body(&request).get("max_tokens"),
            Some(&serde_json::json!(4096))
        );
    }

    #[test]
    fn reasoning_effort_is_suppressed_when_unsupported() {
        let mut request = request();
        request.thinking = Thinking::Effort(Effort::High);
        assert_eq!(
            openai().body(&request).get("reasoning_effort"),
            Some(&"high".into())
        );

        let grok = provider_with(ChatConfig {
            supports_reasoning_effort: false,
            ..ChatConfig::openai()
        });
        assert!(grok.body(&request).get("reasoning_effort").is_none());
    }

    #[test]
    fn a_token_budget_degrades_to_a_medium_effort() {
        let mut request = request();
        request.thinking = Thinking::Budget { tokens: 8192 };
        assert_eq!(
            openai().body(&request).get("reasoning_effort"),
            Some(&"medium".into())
        );
    }

    #[test]
    fn system_prompts_use_the_configured_role_and_come_first() {
        let mut request = request();
        request.system = vec!["规则".to_owned()];
        let body = openai().body(&request);
        assert_eq!(
            body.pointer("/messages/0/role").and_then(|v| v.as_str()),
            Some("developer")
        );
        assert_eq!(
            body.pointer("/messages/1/role").and_then(|v| v.as_str()),
            Some("user")
        );

        let classic = provider_with(ChatConfig {
            system_role: "system",
            ..ChatConfig::openai()
        });
        assert_eq!(
            classic
                .body(&request)
                .pointer("/messages/0/role")
                .and_then(|v| v.as_str()),
            Some("system")
        );
    }

    #[test]
    fn images_become_data_urls() {
        let mut request = request();
        request.messages = vec![Message::User {
            content: vec![UserContent::Image(ImageContent {
                media_type: "image/png".to_owned(),
                data: "QUJD".to_owned(),
            })],
        }];
        assert_eq!(
            openai()
                .body(&request)
                .pointer("/messages/0/content/0/image_url/url")
                .and_then(|v| v.as_str()),
            Some("data:image/png;base64,QUJD")
        );
    }

    #[test]
    fn assistant_tool_calls_keep_arguments_as_json_strings() {
        let mut request = request();
        request.messages = vec![Message::Assistant {
            content: vec![
                AssistantContent::Thinking(ThinkingContent {
                    text: "想".to_owned(),
                    signature: None,
                }),
                AssistantContent::ToolCall(ToolCall {
                    id: "call_1".to_owned(),
                    name: "search".to_owned(),
                    arguments: r#"{"q":1}"#.to_owned(),
                }),
            ],
        }];
        let body = openai().body(&request);
        assert_eq!(
            body.pointer("/messages/0/tool_calls/0/function/arguments")
                .and_then(|v| v.as_str()),
            Some(r#"{"q":1}"#)
        );
        // 默认不回放 `reasoning_content`：OpenAI / xAI 原生接口不认这个字段。
        assert!(body.pointer("/messages/0/reasoning_content").is_none());
        // 有工具调用时 content 必须存在且非 null。
        assert_eq!(
            body.pointer("/messages/0/content"),
            Some(&serde_json::json!(""))
        );
    }

    #[test]
    fn tool_results_become_tool_role_messages() {
        let mut request = request();
        request.messages = vec![Message::ToolResult {
            tool_call_id: "call_1".to_owned(),
            tool_name: "search".to_owned(),
            content: vec![ToolResultContent::Text("结果".to_owned())],
            is_error: false,
        }];
        let body = openai().body(&request);
        assert_eq!(
            body.pointer("/messages/0/role").and_then(|v| v.as_str()),
            Some("tool")
        );
        assert_eq!(
            body.pointer("/messages/0/tool_call_id")
                .and_then(|v| v.as_str()),
            Some("call_1")
        );
        assert_eq!(
            body.pointer("/messages/0/content").and_then(|v| v.as_str()),
            Some("结果")
        );
    }

    #[test]
    fn images_in_tool_results_are_attached_as_a_following_user_message() {
        let mut request = request();
        request.messages = vec![Message::ToolResult {
            tool_call_id: "call_1".to_owned(),
            tool_name: "shot".to_owned(),
            content: vec![
                ToolResultContent::Text("截图如下".to_owned()),
                ToolResultContent::Image(ImageContent {
                    media_type: "image/png".to_owned(),
                    data: "QUJD".to_owned(),
                }),
            ],
            is_error: false,
        }];
        let body = openai().body(&request);

        // tool 角色只留文本。
        assert_eq!(
            body.pointer("/messages/0/content").and_then(|v| v.as_str()),
            Some("截图如下")
        );
        // 图片紧跟一条 user 消息送出，不能丢。
        assert_eq!(
            body.pointer("/messages/1/role").and_then(|v| v.as_str()),
            Some("user")
        );
        assert_eq!(
            body.pointer("/messages/1/content/1/image_url/url")
                .and_then(|v| v.as_str()),
            Some("data:image/png;base64,QUJD")
        );
    }

    #[test]
    fn tool_results_without_images_add_no_extra_message() {
        let mut request = request();
        request.messages = vec![Message::ToolResult {
            tool_call_id: "call_1".to_owned(),
            tool_name: "search".to_owned(),
            content: vec![ToolResultContent::Text("纯文本".to_owned())],
            is_error: false,
        }];
        assert_eq!(
            openai()
                .body(&request)
                .get("messages")
                .and_then(|v| v.as_array())
                .map(Vec::len),
            Some(1)
        );
    }

    #[test]
    fn reasoning_content_is_off_by_default_and_opt_in_per_endpoint() {
        let mut request = request();
        request.messages = vec![Message::Assistant {
            content: vec![AssistantContent::Thinking(ThinkingContent {
                text: "想".to_owned(),
                signature: None,
            })],
        }];
        // OpenAI / xAI 原生接口不认这个字段。
        assert!(
            openai()
                .body(&request)
                .pointer("/messages/0/reasoning_content")
                .is_none()
        );

        let compat = provider_with(ChatConfig {
            replay_reasoning_content: true,
            ..ChatConfig::openai()
        });
        assert_eq!(
            compat
                .body(&request)
                .pointer("/messages/0/reasoning_content")
                .and_then(|v| v.as_str()),
            Some("想")
        );
    }

    #[test]
    fn tool_choice_maps_onto_the_chat_vocabulary() {
        assert_eq!(encode_tool_choice(&ToolChoice::Auto), None);
        assert_eq!(
            encode_tool_choice(&ToolChoice::Required),
            Some("required".into())
        );
        assert_eq!(
            encode_tool_choice(&ToolChoice::Named("search".to_owned())),
            Some(serde_json::json!({ "type": "function", "function": { "name": "search" } }))
        );
    }

    #[test]
    fn tool_choice_is_omitted_when_there_are_no_tools() {
        let mut request = request();
        request.tool_choice = ToolChoice::Required;
        assert!(openai().body(&request).get("tool_choice").is_none());
    }

    #[test]
    fn finish_reasons_map_onto_the_unified_enum() {
        assert_eq!(map_finish_reason("stop"), StopReason::Stop);
        assert_eq!(map_finish_reason("length"), StopReason::Length);
        assert_eq!(map_finish_reason("tool_calls"), StopReason::ToolUse);
        assert_eq!(map_finish_reason("function_call"), StopReason::ToolUse);
        assert_eq!(map_finish_reason("content_filter"), StopReason::Error);
        assert_eq!(map_finish_reason("something_new"), StopReason::Stop);
    }

    #[test]
    fn cached_and_written_tokens_are_deducted_from_the_input_count() {
        let usage = parse_usage(&serde_json::json!({
            "prompt_tokens": 100,
            "completion_tokens": 20,
            "prompt_tokens_details": { "cached_tokens": 30, "cache_write_tokens": 10 },
            "completion_tokens_details": { "reasoning_tokens": 12 }
        }));
        assert_eq!(
            usage,
            Usage {
                input: 60,
                output: 20,
                cache_read: 30,
                cache_write: 10,
                reasoning: 12
            }
        );
        assert_eq!(usage.total_input(), 100);
    }

    #[test]
    fn deepseek_style_cache_hit_field_is_understood() {
        let usage = parse_usage(&serde_json::json!({
            "prompt_tokens": 50, "completion_tokens": 5, "prompt_cache_hit_tokens": 20
        }));
        assert_eq!(usage.cache_read, 20);
        assert_eq!(usage.input, 30);
    }

    #[test]
    fn tool_call_keys_prefer_index_then_id_then_position() {
        assert_eq!(
            tool_call_key(&serde_json::json!({ "index": 2, "id": "x" }), 0),
            "tool:2"
        );
        assert_eq!(
            tool_call_key(&serde_json::json!({ "id": "x" }), 0),
            "tool-id:x"
        );
        assert_eq!(tool_call_key(&serde_json::json!({}), 3), "tool-pos:3");
    }

    #[tokio::test]
    async fn text_and_usage_flow_through_to_a_done_event() {
        let events = ok_events(vec![
            delta_chunk(serde_json::json!({ "content": "你" })),
            delta_chunk(serde_json::json!({ "content": "好" })),
            chunk(serde_json::json!({
                "id": "c1", "choices": [{ "delta": {}, "finish_reason": "stop" }]
            })),
            chunk(serde_json::json!({
                "id": "c1", "choices": [],
                "usage": { "prompt_tokens": 8, "completion_tokens": 2 }
            })),
            SseEvent {
                event: None,
                data: "[DONE]".to_owned(),
            },
        ])
        .await;

        assert_eq!(
            events.first(),
            Some(&StreamEvent::Start {
                response_id: Some("c1".to_owned()),
                model: Some("gpt-5.2".to_owned()),
            })
        );
        assert!(events.contains(&StreamEvent::TextEnd {
            index: 0,
            text: "你好".to_owned()
        }));
        assert_eq!(
            events.last(),
            Some(&StreamEvent::Done {
                stop_reason: StopReason::Stop,
                usage: Usage {
                    input: 8,
                    output: 2,
                    ..Usage::default()
                },
            })
        );
    }

    #[tokio::test]
    async fn reasoning_deltas_land_in_a_thinking_block() {
        let events = ok_events(vec![
            delta_chunk(serde_json::json!({ "reasoning_content": "推" })),
            delta_chunk(serde_json::json!({ "reasoning": "理" })),
            chunk(serde_json::json!({
                "id": "c1", "choices": [{ "delta": {}, "finish_reason": "stop" }]
            })),
            SseEvent {
                event: None,
                data: "[DONE]".to_owned(),
            },
        ])
        .await;
        assert!(events.contains(&StreamEvent::ThinkingEnd {
            index: 0,
            content: ThinkingContent {
                text: "推理".to_owned(),
                signature: None
            },
        }));
    }

    #[tokio::test]
    async fn tool_call_fragments_are_reassembled_by_index() {
        let events = ok_events(vec![
            delta_chunk(serde_json::json!({
                "tool_calls": [{
                    "index": 0, "id": "call_1",
                    "function": { "name": "search", "arguments": "" }
                }]
            })),
            delta_chunk(serde_json::json!({
                "tool_calls": [{ "index": 0, "function": { "arguments": "{\"q\":" } }]
            })),
            delta_chunk(serde_json::json!({
                "tool_calls": [{ "index": 0, "function": { "arguments": "1}" } }]
            })),
            chunk(serde_json::json!({
                "id": "c1", "choices": [{ "delta": {}, "finish_reason": "tool_calls" }]
            })),
            SseEvent {
                event: None,
                data: "[DONE]".to_owned(),
            },
        ])
        .await;

        assert!(events.contains(&StreamEvent::ToolCallEnd {
            index: 0,
            tool_call: ToolCall {
                id: "call_1".to_owned(),
                name: "search".to_owned(),
                arguments: r#"{"q":1}"#.to_owned(),
            },
        }));
        assert!(matches!(
            events.last(),
            Some(StreamEvent::Done {
                stop_reason: StopReason::ToolUse,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn parallel_tool_calls_stay_separate() {
        let events = ok_events(vec![
            delta_chunk(serde_json::json!({
                "tool_calls": [
                    { "index": 0, "id": "a", "function": { "name": "one", "arguments": "{}" } },
                    { "index": 1, "id": "b", "function": { "name": "two", "arguments": "{}" } }
                ]
            })),
            chunk(serde_json::json!({
                "id": "c1", "choices": [{ "delta": {}, "finish_reason": "tool_calls" }]
            })),
            SseEvent {
                event: None,
                data: "[DONE]".to_owned(),
            },
        ])
        .await;

        let ends: Vec<&StreamEvent> = events
            .iter()
            .filter(|event| matches!(event, StreamEvent::ToolCallEnd { .. }))
            .collect();
        assert_eq!(ends.len(), 2);
        assert!(events.contains(&StreamEvent::ToolCallEnd {
            index: 1,
            tool_call: ToolCall {
                id: "b".to_owned(),
                name: "two".to_owned(),
                arguments: "{}".to_owned(),
            },
        }));
    }

    #[tokio::test]
    async fn tool_calls_without_index_fall_back_to_the_call_id() {
        let events = ok_events(vec![
            delta_chunk(serde_json::json!({
                "tool_calls": [{ "id": "a", "function": { "name": "one", "arguments": "{\"x\":" } }]
            })),
            delta_chunk(serde_json::json!({
                "tool_calls": [{ "id": "a", "function": { "arguments": "1}" } }]
            })),
            chunk(serde_json::json!({
                "id": "c1", "choices": [{ "delta": {}, "finish_reason": "tool_calls" }]
            })),
            SseEvent {
                event: None,
                data: "[DONE]".to_owned(),
            },
        ])
        .await;
        assert!(events.contains(&StreamEvent::ToolCallEnd {
            index: 0,
            tool_call: ToolCall {
                id: "a".to_owned(),
                name: "one".to_owned(),
                arguments: r#"{"x":1}"#.to_owned(),
            },
        }));
    }

    #[tokio::test]
    async fn object_shaped_arguments_are_serialized_once() {
        let events = ok_events(vec![
            delta_chunk(serde_json::json!({
                "tool_calls": [{
                    "index": 0, "id": "a",
                    "function": { "name": "one", "arguments": { "q": 1 } }
                }]
            })),
            chunk(serde_json::json!({
                "id": "c1", "choices": [{ "delta": {}, "finish_reason": "tool_calls" }]
            })),
            SseEvent {
                event: None,
                data: "[DONE]".to_owned(),
            },
        ])
        .await;
        assert!(events.contains(&StreamEvent::ToolCallEnd {
            index: 0,
            tool_call: ToolCall {
                id: "a".to_owned(),
                name: "one".to_owned(),
                arguments: r#"{"q":1}"#.to_owned(),
            },
        }));
    }

    #[tokio::test]
    async fn a_stop_finish_reason_is_promoted_when_tool_calls_were_streamed() {
        let events = ok_events(vec![
            delta_chunk(serde_json::json!({
                "tool_calls": [{ "index": 0, "id": "a", "function": { "name": "one" } }]
            })),
            chunk(serde_json::json!({
                "id": "c1", "choices": [{ "delta": {}, "finish_reason": "stop" }]
            })),
            SseEvent {
                event: None,
                data: "[DONE]".to_owned(),
            },
        ])
        .await;
        assert!(matches!(
            events.last(),
            Some(StreamEvent::Done {
                stop_reason: StopReason::ToolUse,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn in_band_error_chunks_surface_as_api_errors() {
        let events = collect(vec![chunk(serde_json::json!({
            "error": { "message": "rate limited", "type": "rate_limit_error" }
        }))])
        .await;
        let error = events
            .into_iter()
            .find_map(Result::err)
            .expect("应当有错误项");
        assert!(
            matches!(error, AiError::Api { code, .. } if code.as_deref() == Some("rate_limit_error"))
        );
    }

    #[tokio::test]
    async fn malformed_chunks_are_decode_errors_not_silent_skips() {
        let events = collect(vec![SseEvent {
            event: None,
            data: "{not json".to_owned(),
        }])
        .await;
        assert!(matches!(
            events.into_iter().find_map(Result::err),
            Some(AiError::Decode { .. })
        ));
    }

    #[tokio::test]
    async fn a_stream_cut_before_finish_reason_is_a_protocol_error() {
        let events = collect(vec![delta_chunk(serde_json::json!({ "content": "半" }))]).await;
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, Ok(StreamEvent::Done { .. }))),
            "截断的流不该产生 Done"
        );
        assert!(matches!(
            events.into_iter().find_map(Result::err),
            Some(AiError::Protocol { .. })
        ));
    }

    #[tokio::test]
    async fn a_done_sentinel_without_finish_reason_is_also_a_protocol_error() {
        let events = collect(vec![
            delta_chunk(serde_json::json!({ "content": "半" })),
            SseEvent {
                event: None,
                data: "[DONE]".to_owned(),
            },
        ])
        .await;
        assert!(matches!(
            events.into_iter().find_map(Result::err),
            Some(AiError::Protocol { .. })
        ));
    }
}
