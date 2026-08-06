//! OpenAI Responses API 适配器。
//!
//! 同时服务三条线路：`openai`（平台 Responses）、`openai-codex`（ChatGPT 订阅走
//! `chatgpt.com/backend-api/codex/responses`）、`xai-oauth`（SuperGrok）。差异由
//! [`ResponsesConfig`] 与 [`Flavor`] 承载。
//!
//! 与 Chat Completions 最容易混淆的两点：
//!
//! - function 工具是**扁平**的 `{type,name,parameters,strict}`，不是嵌套
//!   `{type:"function",function:{…}}`；
//! - 助手历史里的文本用 `output_text`，用户输入用 `input_text` / `input_image`。
//!
//! 思考内容的回放走 `reasoning` item：整个 item 的 JSON 存在
//! [`ThinkingContent::signature`] 里，回放时原样送回，这样加密的
//! `encrypted_content` 才不会在多轮里丢失。

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
    AssistantContent, CompletionRequest, Message, ProviderId, StopReason, StreamEvent, Thinking,
    ThinkingContent, Tool, ToolChoice, ToolResultContent, Usage, UserContent,
};

/// OpenAI 平台默认 base URL。
pub const OPENAI_BASE_URL: &str = "https://api.openai.com/v1";

/// 平台 Responses 路径。
pub const RESPONSES_PATH: &str = "/responses";

/// 取回加密思考所需的 `include` 值。
pub const INCLUDE_ENCRYPTED_REASONING: &str = "reasoning.encrypted_content";

/// 线路风味。决定要不要带 Codex 专属请求头与请求体约束。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Flavor {
    /// 标准 Responses（OpenAI 平台、xAI SuperGrok）。
    Standard,
    /// ChatGPT 订阅的 Codex 后端。
    Codex(CodexFlavor),
}

/// Codex 后端专属的请求头材料。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexFlavor {
    /// `originator` 头与授权 URL 参数必须一致。
    pub originator: &'static str,
    /// `version` 头，声明 Codex 客户端版本。
    pub client_version: &'static str,
    /// `OpenAI-Beta` 头。
    pub beta: &'static str,
}

/// 线格式差异的开关集合。
///
/// 这里的布尔各自对应一个**独立**的线协议开关，彼此没有蕴含关系；把它们折成枚举
/// 或位标志只会让"这一家到底发不发某个字段"更难查。
#[allow(
    clippy::struct_excessive_bools,
    reason = "每个布尔对应一个独立的线协议开关"
)]
#[derive(Debug, Clone)]
pub struct ResponsesConfig {
    /// 归属提供商。
    pub provider: ProviderId,
    /// API 根地址，不带尾斜杠。
    pub base_url: String,
    /// 端点路径。
    pub path: &'static str,
    /// 线路风味。
    pub flavor: Flavor,
    /// `store` 字段取值。无状态回放要求 `false`。
    pub store: bool,
    /// 是否把 `reasoning.encrypted_content` 放进 `include`。
    pub include_encrypted_reasoning: bool,
    /// `reasoning.summary` 取值；`None` 表示不下发该字段（xAI 要求如此）。
    pub reasoning_summary: Option<&'static str>,
    /// 是否下发 `max_output_tokens`。Codex 后端会因此报 400。
    pub send_max_output_tokens: bool,
    /// 是否下发 `temperature` / `top_p`。Codex 后端会因此报 400。
    pub send_sampling_params: bool,
    /// prompt 缓存亲和头。
    pub cache_session_header: Option<&'static str>,
}

impl ResponsesConfig {
    /// OpenAI 平台的默认配置。
    #[must_use]
    pub fn openai() -> Self {
        Self {
            provider: ProviderId::OpenAi,
            base_url: OPENAI_BASE_URL.to_owned(),
            path: RESPONSES_PATH,
            flavor: Flavor::Standard,
            // 无状态：不让平台留存会话，多轮思考靠 encrypted_content 回放。
            store: false,
            include_encrypted_reasoning: true,
            reasoning_summary: Some("auto"),
            send_max_output_tokens: true,
            send_sampling_params: true,
            cache_session_header: None,
        }
    }
}

/// Responses 适配器。
#[derive(Debug, Clone)]
pub struct ResponsesProvider {
    auth: Arc<AuthStore>,
    client: Client,
    config: ResponsesConfig,
}

impl ResponsesProvider {
    /// 用共享 HTTP 客户端构造。
    pub fn new(auth: Arc<AuthStore>, config: ResponsesConfig) -> Result<Self, AiError> {
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
        format!("{}{}", self.config.base_url, self.config.path)
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

        if let Flavor::Codex(codex) = &self.config.flavor {
            // 缺 account id 就不是一个能用的 Codex 请求，但这里不该失败——
            // `AuthStore` 已经在登录时保证过它存在。
            if let Some(account_id) = access.account_id.as_deref() {
                http::set_header(&mut headers, "chatgpt-account-id", account_id);
            }
            http::set_header(&mut headers, "openai-beta", codex.beta);
            http::set_header(&mut headers, "originator", codex.originator);
            http::set_header(&mut headers, "version", codex.client_version);
            if let Some(session) = request.session_id.as_deref() {
                http::set_header(&mut headers, "session_id", session);
                http::set_header(&mut headers, "conversation_id", session);
                http::set_header(&mut headers, "x-client-request-id", session);
            }
        }
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
        drop(body.insert("stream".to_owned(), true.into()));
        drop(body.insert("store".to_owned(), self.config.store.into()));
        drop(body.insert("input".to_owned(), encode_input(&request.messages)));

        if !request.system.is_empty() {
            drop(body.insert(
                "instructions".to_owned(),
                request.system.join("\n\n").into(),
            ));
        }
        if self.config.include_encrypted_reasoning {
            drop(body.insert(
                "include".to_owned(),
                serde_json::json!([INCLUDE_ENCRYPTED_REASONING]),
            ));
        }
        if !request.tools.is_empty() {
            drop(body.insert("tools".to_owned(), encode_tools(&request.tools)));
            if let Some(choice) = encode_tool_choice(&request.tool_choice) {
                drop(body.insert("tool_choice".to_owned(), choice));
            }
        }
        if let Some(reasoning) = self.encode_reasoning(request.thinking) {
            drop(body.insert("reasoning".to_owned(), reasoning));
        }
        if self.config.send_max_output_tokens
            && let Some(limit) = request.max_output_tokens
        {
            drop(body.insert("max_output_tokens".to_owned(), limit.into()));
        }
        if self.config.send_sampling_params {
            if let Some(temperature) = request.temperature {
                drop(body.insert("temperature".to_owned(), temperature.into()));
            }
            if let Some(top_p) = request.top_p {
                drop(body.insert("top_p".to_owned(), top_p.into()));
            }
        }
        if let Some(tier) = request.service_tier {
            drop(body.insert("service_tier".to_owned(), tier.as_str().into()));
        }
        if let Some(key) = request.prompt_cache_key.as_deref() {
            drop(body.insert("prompt_cache_key".to_owned(), key.into()));
        }
        serde_json::Value::Object(body)
    }

    fn encode_reasoning(&self, thinking: Thinking) -> Option<serde_json::Value> {
        let effort = match thinking {
            Thinking::Disabled => return None,
            Thinking::Effort(effort) => effort.as_str(),
            // Responses 没有 token 预算旋钮，折成档位。
            Thinking::Budget { .. } => crate::types::Effort::Medium.as_str(),
        };
        let mut reasoning = serde_json::Map::new();
        drop(reasoning.insert("effort".to_owned(), effort.into()));
        // xAI 明确要求不带 summary；带上会被拒。
        if let Some(summary) = self.config.reasoning_summary {
            drop(reasoning.insert("summary".to_owned(), summary.into()));
        }
        Some(serde_json::Value::Object(reasoning))
    }
}

#[async_trait]
impl Provider for ResponsesProvider {
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

/// 把 Responses 的 SSE 流翻成统一事件流。
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
                    match handle_event(&sse, &mut state, &mut out) {
                        Ok(true) => {
                            state.pending.extend(out);
                            if let Some(err) = finish(&mut state) {
                                return Some((Err(err), state));
                            }
                        }
                        Ok(false) => state.pending.extend(out),
                        Err(err) => {
                            state.finished = true;
                            return Some((Err(err), state));
                        }
                    }
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
/// 只有见过 `response.completed` / `.done` / `.incomplete` 才算协议意义上的正常
/// 终止。缺了它就说明连接断在中途，此刻手上是半截文本或没闭合的工具 JSON，报成功
/// 等于把残缺内容当完整结果交出去，所以返回 [`AiError::Protocol`]。
fn finish<S>(state: &mut DecodeState<S>) -> Option<AiError> {
    state.finished = true;
    let Some(stop_reason) = state.stop_reason else {
        return Some(AiError::Protocol {
            provider: state.provider,
            detail: "流在收到 response.completed / .incomplete 之前就结束了".to_owned(),
        });
    };
    let mut out = Vec::new();
    state.blocks.close_all(&mut out);
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

/// 处理一条事件；返回 `true` 表示流已终止。
fn handle_event<S>(
    sse: &SseEvent,
    state: &mut DecodeState<S>,
    out: &mut Vec<StreamEvent>,
) -> Result<bool, AiError> {
    let payload: serde_json::Value =
        serde_json::from_str(&sse.data).map_err(|source| AiError::Decode {
            provider: state.provider,
            source,
        })?;
    let kind = sse
        .event
        .as_deref()
        .or_else(|| payload.get("type").and_then(serde_json::Value::as_str))
        .unwrap_or_default();

    match kind {
        "response.created" | "response.in_progress" => {
            if !state.started {
                state.started = true;
                let response = payload.get("response");
                out.push(StreamEvent::Start {
                    response_id: response.and_then(|r| str_field(r, "id")),
                    model: response.and_then(|r| str_field(r, "model")),
                });
            }
            Ok(false)
        }
        "response.output_text.delta" => {
            if let Some(delta) = str_field(&payload, "delta") {
                state.blocks.text_delta(&text_key(&payload), &delta, out);
            }
            Ok(false)
        }
        "response.output_text.done" => {
            state.blocks.close(&text_key(&payload), out);
            Ok(false)
        }
        // 推理摘要与推理正文都归到同一个思考块。
        "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
            if let Some(delta) = str_field(&payload, "delta") {
                state
                    .blocks
                    .thinking_delta(&reasoning_key(&payload), &delta, out);
            }
            Ok(false)
        }
        "response.function_call_arguments.delta" => {
            if let Some(delta) = str_field(&payload, "delta") {
                state.blocks.tool_delta(&item_key(&payload), &delta, out);
            }
            Ok(false)
        }
        "response.function_call_arguments.done" => {
            let key = item_key(&payload);
            if let Some(arguments) = str_field(&payload, "arguments") {
                state.blocks.set_tool_arguments(&key, &arguments, out);
            }
            if let Some(name) = str_field(&payload, "name") {
                state.blocks.tool_start(&key, "", &name, out);
            }
            Ok(false)
        }
        "response.output_item.added" | "response.output_item.done" => {
            handle_output_item(&payload, &mut state.blocks, out);
            Ok(false)
        }
        "response.completed" | "response.done" | "response.incomplete" => {
            let response = payload.get("response");
            if let Some(raw) = response.and_then(|r| r.get("usage")) {
                state.usage = parse_usage(raw);
            }
            let status = response.and_then(|r| str_field(r, "status"));
            let incomplete_reason = response
                .and_then(|r| r.get("incomplete_details"))
                .and_then(|d| str_field(d, "reason"));
            state.stop_reason = Some(map_status(status.as_deref(), incomplete_reason.as_deref()));
            if incomplete_reason.as_deref() == Some("content_filter") {
                return Err(AiError::Api {
                    provider: state.provider,
                    status: 200,
                    kind: ApiErrorKind::InvalidRequest,
                    code: Some("content_filter".to_owned()),
                    message: "响应被内容过滤中止".to_owned(),
                    retry_after: None,
                });
            }
            Ok(true)
        }
        "response.failed" | "error" => {
            let error = payload
                .get("response")
                .and_then(|r| r.get("error"))
                .or_else(|| payload.get("error"))
                .unwrap_or(&payload);
            Err(AiError::Api {
                provider: state.provider,
                status: 200,
                kind: ApiErrorKind::Upstream,
                code: str_field(error, "code").or_else(|| str_field(error, "type")),
                message: str_field(error, "message")
                    .unwrap_or_else(|| "上游在流中返回了未描述的错误".to_owned()),
                retry_after: None,
            })
        }
        _ => Ok(false),
    }
}

/// `output_item.added/done` 携带完整 item：从中补齐工具调用身份与思考签名。
fn handle_output_item(
    payload: &serde_json::Value,
    blocks: &mut Blocks,
    out: &mut Vec<StreamEvent>,
) {
    let Some(item) = payload.get("item") else {
        return;
    };
    let key = item_key(payload);
    match item.get("type").and_then(serde_json::Value::as_str) {
        Some("function_call") => {
            let id = str_field(item, "call_id").unwrap_or_default();
            let name = str_field(item, "name").unwrap_or_default();
            blocks.tool_start(&key, &id, &name, out);
            if let Some(arguments) = str_field(item, "arguments").filter(|a| !a.is_empty()) {
                blocks.set_tool_arguments(&key, &arguments, out);
            }
        }
        Some("reasoning") => {
            let key = reasoning_key(payload);
            // 摘要文本在 delta 里已经流过，这里只补签名——整个 item 的 JSON。
            // 回放时原样送回，`encrypted_content` 才不会在多轮里丢。
            blocks.open_thinking(&key, out);
            // `.added` 与 `.done` 会先后各送一次完整 item，必须覆盖而不是追加，
            // 否则签名变成 `{…}{…}` 这种非法 JSON，回放时整块被丢。
            blocks.set_thinking_signature(&key, &item.to_string(), out);
        }
        _ => {}
    }
}

fn item_key(payload: &serde_json::Value) -> String {
    let item_id = str_field(payload, "item_id")
        .or_else(|| payload.get("item").and_then(|item| str_field(item, "id")))
        .unwrap_or_default();
    let output_index = payload
        .get("output_index")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    format!("item:{output_index}:{item_id}")
}

fn text_key(payload: &serde_json::Value) -> String {
    let content_index = payload
        .get("content_index")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    format!("{}:text:{content_index}", item_key(payload))
}

/// 推理摘要分片（`summary_index`）不该各自成块，一律并进同一个思考块。
fn reasoning_key(payload: &serde_json::Value) -> String {
    format!("{}:reasoning", item_key(payload))
}

fn parse_usage(raw: &serde_json::Value) -> Usage {
    let input = u64_field(raw, "input_tokens").unwrap_or(0);
    let cache_read = raw
        .get("input_tokens_details")
        .and_then(|d| u64_field(d, "cached_tokens"))
        .or_else(|| u64_field(raw, "prompt_cache_hit_tokens"))
        .unwrap_or(0);
    let cache_write = raw
        .get("input_tokens_details")
        .and_then(|d| u64_field(d, "cache_write_tokens"))
        .unwrap_or(0);
    Usage {
        input: input.saturating_sub(cache_read).saturating_sub(cache_write),
        output: u64_field(raw, "output_tokens").unwrap_or(0),
        cache_read,
        cache_write,
        reasoning: raw
            .get("output_tokens_details")
            .and_then(|d| u64_field(d, "reasoning_tokens"))
            .unwrap_or(0),
    }
}

fn map_status(status: Option<&str>, incomplete_reason: Option<&str>) -> StopReason {
    match status {
        Some("incomplete") => match incomplete_reason {
            Some("content_filter") => StopReason::Error,
            // `max_output_tokens` 以及未标注原因的 incomplete 都算截断。
            _ => StopReason::Length,
        },
        Some("failed" | "cancelled") => StopReason::Error,
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

/// 把统一消息翻成 Responses 的 `input` 数组。
fn encode_input(messages: &[Message]) -> serde_json::Value {
    let mut items: Vec<serde_json::Value> = Vec::new();
    for message in messages {
        match message {
            Message::User { content } => items.push(serde_json::json!({
                "role": "user",
                "content": content.iter().map(encode_user_content).collect::<Vec<_>>(),
            })),
            Message::Assistant { content } => encode_assistant(content, &mut items),
            Message::ToolResult {
                tool_call_id,
                content,
                ..
            } => {
                items.push(serde_json::json!({
                    "type": "function_call_output",
                    "call_id": tool_call_id,
                    "output": flatten_tool_result(content),
                }));
                // 图片不能塞进 `function_call_output`，只能另起一条 user 消息。
                let images: Vec<serde_json::Value> = content
                    .iter()
                    .filter_map(|item| match item {
                        ToolResultContent::Image(image) => Some(serde_json::json!({
                            "type": "input_image",
                            "image_url": image.to_data_url(),
                        })),
                        ToolResultContent::Text(_) => None,
                    })
                    .collect();
                if !images.is_empty() {
                    items.push(serde_json::json!({ "role": "user", "content": images }));
                }
            }
        }
    }
    items.into()
}

fn encode_user_content(content: &UserContent) -> serde_json::Value {
    match content {
        UserContent::Text(text) => serde_json::json!({ "type": "input_text", "text": text }),
        UserContent::Image(image) => serde_json::json!({
            "type": "input_image",
            "image_url": image.to_data_url(),
        }),
    }
}

/// 把助手消息拆成 Responses 的 input item，**保持原始顺序**。
///
/// 连续的文本合并成一条 `message`，但遇到 reasoning / `function_call` 就先落盘，
/// 免得 `[Text, ToolCall]` 在线上被翻成 `[function_call, message]`——顺序变了，
/// 模型看到的因果关系也就变了。
fn encode_assistant(content: &[AssistantContent], items: &mut Vec<serde_json::Value>) {
    let mut text_parts: Vec<serde_json::Value> = Vec::new();
    for item in content {
        match item {
            AssistantContent::Text(text) => text_parts.push(serde_json::json!({
                "type": "output_text",
                "text": text,
                "annotations": [],
            })),
            AssistantContent::Thinking(thinking) => {
                flush_assistant_text(&mut text_parts, items);
                if let Some(reasoning) = replay_reasoning(thinking) {
                    items.push(reasoning);
                }
            }
            // Responses 没有对应概念，且原始 payload 已在 Thinking 分支回放。
            AssistantContent::RedactedThinking(_) => {}
            AssistantContent::ToolCall(call) => {
                flush_assistant_text(&mut text_parts, items);
                items.push(serde_json::json!({
                    "type": "function_call",
                    "call_id": call.id,
                    "name": call.name,
                    "arguments": call.arguments,
                }));
            }
        }
    }
    flush_assistant_text(&mut text_parts, items);
}

fn flush_assistant_text(
    text_parts: &mut Vec<serde_json::Value>,
    items: &mut Vec<serde_json::Value>,
) {
    if text_parts.is_empty() {
        return;
    }
    items.push(serde_json::json!({
        "type": "message",
        "role": "assistant",
        "status": "completed",
        "content": std::mem::take(text_parts),
    }));
}

/// 把思考块还原成可回放的 `reasoning` item。
///
/// 只有当签名解出来确实是一条带 `id` 的 `reasoning` item 时才回放——否则送回去
/// 会被服务端判为非法输入。纯文本摘要没有回放价值，直接丢弃。
fn replay_reasoning(thinking: &ThinkingContent) -> Option<serde_json::Value> {
    let signature = thinking.signature.as_deref()?;
    let item: serde_json::Value = serde_json::from_str(signature).ok()?;
    if item.get("type").and_then(serde_json::Value::as_str) != Some("reasoning") {
        return None;
    }
    item.get("id").and_then(serde_json::Value::as_str)?;
    Some(item)
}

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

/// Responses 的 function 工具是扁平结构，别照搬 Chat Completions 的嵌套形状。
fn encode_tools(tools: &[Tool]) -> serde_json::Value {
    tools
        .iter()
        .map(|tool| {
            let mut entry = serde_json::Map::new();
            drop(entry.insert("type".to_owned(), "function".into()));
            drop(entry.insert("name".to_owned(), tool.name.clone().into()));
            drop(entry.insert("description".to_owned(), tool.description.clone().into()));
            drop(entry.insert("parameters".to_owned(), tool.parameters.clone()));
            if let Some(strict) = tool.strict {
                drop(entry.insert("strict".to_owned(), strict.into()));
            }
            serde_json::Value::Object(entry)
        })
        .collect::<Vec<_>>()
        .into()
}

fn encode_tool_choice(choice: &ToolChoice) -> Option<serde_json::Value> {
    match choice {
        ToolChoice::Auto => None,
        ToolChoice::None => Some("none".into()),
        ToolChoice::Required => Some("required".into()),
        ToolChoice::Named(name) => Some(serde_json::json!({ "type": "function", "name": name })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::credential::{AccessKind, ApiKeyCredential, Credential};
    use crate::auth::store::{CredentialStore, MemoryCredentialStore};
    use crate::types::{Effort, ImageContent, ToolCall};

    fn provider_with(config: ResponsesConfig) -> ResponsesProvider {
        let store = Arc::new(MemoryCredentialStore::new());
        store
            .save(
                config.provider,
                Credential::ApiKey(ApiKeyCredential {
                    key: "sk".to_owned(),
                }),
            )
            .expect("预置凭据");
        ResponsesProvider {
            auth: Arc::new(AuthStore::bare(store)),
            client: http::shared_client().expect("HTTP 客户端"),
            config,
        }
    }

    fn openai() -> ResponsesProvider {
        provider_with(ResponsesConfig::openai())
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

    fn sse(kind: &str, mut data: serde_json::Value) -> SseEvent {
        if let Some(object) = data.as_object_mut() {
            drop(object.insert("type".to_owned(), kind.into()));
        }
        SseEvent {
            event: Some(kind.to_owned()),
            data: data.to_string(),
        }
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
    fn endpoint_joins_base_url_and_path() {
        assert_eq!(openai().endpoint(), "https://api.openai.com/v1/responses");
    }

    #[test]
    fn standard_flavor_sends_no_codex_headers() {
        let headers = openai().headers(&access(), &request());
        assert!(headers.get("chatgpt-account-id").is_none());
        assert!(headers.get("originator").is_none());
        assert!(headers.get("openai-beta").is_none());
    }

    #[test]
    fn codex_flavor_sends_the_account_id_and_client_identity() {
        let provider = provider_with(ResponsesConfig {
            provider: ProviderId::OpenAiCodex,
            flavor: Flavor::Codex(CodexFlavor {
                originator: "pi",
                client_version: "0.144.1",
                beta: "responses=experimental",
            }),
            ..ResponsesConfig::openai()
        });
        let access = Access {
            token: "at".to_owned(),
            kind: AccessKind::OAuth,
            account_id: Some("acct-1".to_owned()),
        };
        let mut request = request();
        request.session_id = Some("sess-1".to_owned());

        let headers = provider.headers(&access, &request);
        assert_eq!(
            headers
                .get("chatgpt-account-id")
                .and_then(|v| v.to_str().ok()),
            Some("acct-1")
        );
        assert_eq!(
            headers.get("openai-beta").and_then(|v| v.to_str().ok()),
            Some("responses=experimental")
        );
        assert_eq!(
            headers.get("originator").and_then(|v| v.to_str().ok()),
            Some("pi")
        );
        assert_eq!(
            headers.get("version").and_then(|v| v.to_str().ok()),
            Some("0.144.1")
        );
        assert_eq!(
            headers.get("session_id").and_then(|v| v.to_str().ok()),
            Some("sess-1")
        );
    }

    #[test]
    fn stateless_defaults_pair_store_false_with_encrypted_reasoning() {
        let body = openai().body(&request());
        assert_eq!(body.get("store"), Some(&serde_json::json!(false)));
        assert_eq!(
            body.get("include"),
            Some(&serde_json::json!([INCLUDE_ENCRYPTED_REASONING]))
        );
    }

    #[test]
    fn codex_omits_sampling_and_output_limits() {
        let provider = provider_with(ResponsesConfig {
            provider: ProviderId::OpenAiCodex,
            send_max_output_tokens: false,
            send_sampling_params: false,
            ..ResponsesConfig::openai()
        });
        let mut request = request();
        request.max_output_tokens = Some(1000);
        request.temperature = Some(0.5);
        request.top_p = Some(0.9);

        let body = provider.body(&request);
        assert!(body.get("max_output_tokens").is_none());
        assert!(body.get("temperature").is_none());
        assert!(body.get("top_p").is_none());
    }

    #[test]
    fn reasoning_summary_is_omitted_when_configured_off() {
        let mut request = request();
        request.thinking = Thinking::Effort(Effort::High);

        let body = openai().body(&request);
        assert_eq!(
            body.pointer("/reasoning/effort").and_then(|v| v.as_str()),
            Some("high")
        );
        assert_eq!(
            body.pointer("/reasoning/summary").and_then(|v| v.as_str()),
            Some("auto")
        );

        let grok = provider_with(ResponsesConfig {
            reasoning_summary: None,
            ..ResponsesConfig::openai()
        });
        let body = grok.body(&request);
        assert_eq!(
            body.pointer("/reasoning/effort").and_then(|v| v.as_str()),
            Some("high")
        );
        assert!(body.pointer("/reasoning/summary").is_none());
    }

    #[test]
    fn system_prompts_become_top_level_instructions() {
        let mut request = request();
        request.system = vec!["一".to_owned(), "二".to_owned()];
        assert_eq!(
            openai()
                .body(&request)
                .get("instructions")
                .and_then(|v| v.as_str()),
            Some("一\n\n二")
        );
    }

    #[test]
    fn function_tools_are_flat_not_nested() {
        let mut request = request();
        request.tools = vec![Tool {
            name: "search".to_owned(),
            description: "找东西".to_owned(),
            parameters: serde_json::json!({ "type": "object", "properties": {} }),
            strict: Some(true),
        }];
        let body = openai().body(&request);
        assert_eq!(
            body.pointer("/tools/0/type").and_then(|v| v.as_str()),
            Some("function")
        );
        assert_eq!(
            body.pointer("/tools/0/name").and_then(|v| v.as_str()),
            Some("search")
        );
        assert!(
            body.pointer("/tools/0/function").is_none(),
            "不该是 Chat Completions 的嵌套形状"
        );
    }

    #[test]
    fn user_content_uses_input_text_and_input_image() {
        let mut request = request();
        request.messages = vec![Message::User {
            content: vec![
                UserContent::Text("看图".to_owned()),
                UserContent::Image(ImageContent {
                    media_type: "image/png".to_owned(),
                    data: "QUJD".to_owned(),
                }),
            ],
        }];
        let body = openai().body(&request);
        assert_eq!(
            body.pointer("/input/0/content/0/type")
                .and_then(|v| v.as_str()),
            Some("input_text")
        );
        assert_eq!(
            body.pointer("/input/0/content/1/image_url")
                .and_then(|v| v.as_str()),
            Some("data:image/png;base64,QUJD")
        );
    }

    #[test]
    fn assistant_text_uses_output_text() {
        let mut request = request();
        request.messages = vec![Message::assistant("答案")];
        let body = openai().body(&request);
        assert_eq!(
            body.pointer("/input/0/type").and_then(|v| v.as_str()),
            Some("message")
        );
        assert_eq!(
            body.pointer("/input/0/content/0/type")
                .and_then(|v| v.as_str()),
            Some("output_text")
        );
    }

    #[test]
    fn tool_calls_and_outputs_are_separate_input_items() {
        let mut request = request();
        request.messages = vec![
            Message::Assistant {
                content: vec![AssistantContent::ToolCall(ToolCall {
                    id: "call_1".to_owned(),
                    name: "search".to_owned(),
                    arguments: r#"{"q":1}"#.to_owned(),
                })],
            },
            Message::ToolResult {
                tool_call_id: "call_1".to_owned(),
                tool_name: "search".to_owned(),
                content: vec![ToolResultContent::Text("结果".to_owned())],
                is_error: false,
            },
        ];
        let body = openai().body(&request);
        assert_eq!(
            body.pointer("/input/0/type").and_then(|v| v.as_str()),
            Some("function_call")
        );
        assert_eq!(
            body.pointer("/input/0/call_id").and_then(|v| v.as_str()),
            Some("call_1")
        );
        assert_eq!(
            body.pointer("/input/1/type").and_then(|v| v.as_str()),
            Some("function_call_output")
        );
        assert_eq!(
            body.pointer("/input/1/output").and_then(|v| v.as_str()),
            Some("结果")
        );
    }

    #[test]
    fn images_in_tool_results_are_appended_as_a_user_message() {
        let mut request = request();
        request.messages = vec![Message::ToolResult {
            tool_call_id: "call_1".to_owned(),
            tool_name: "shot".to_owned(),
            content: vec![ToolResultContent::Image(ImageContent {
                media_type: "image/png".to_owned(),
                data: "QUJD".to_owned(),
            })],
            is_error: false,
        }];
        let body = openai().body(&request);
        assert_eq!(
            body.pointer("/input/1/content/0/type")
                .and_then(|v| v.as_str()),
            Some("input_image")
        );
    }

    #[test]
    fn assistant_history_keeps_text_and_tool_calls_in_their_original_order() {
        let mut request = request();
        request.messages = vec![Message::Assistant {
            content: vec![
                AssistantContent::Text("先说一句".to_owned()),
                AssistantContent::ToolCall(ToolCall {
                    id: "call_1".to_owned(),
                    name: "search".to_owned(),
                    arguments: "{}".to_owned(),
                }),
                AssistantContent::Text("再说一句".to_owned()),
            ],
        }];
        let body = openai().body(&request);

        // 把 message 一律排到 function_call 之后会改写因果顺序，模型看到的就不是
        // 它当时实际产生的序列了。
        assert_eq!(
            body.pointer("/input/0/content/0/text")
                .and_then(|v| v.as_str()),
            Some("先说一句")
        );
        assert_eq!(
            body.pointer("/input/1/type").and_then(|v| v.as_str()),
            Some("function_call")
        );
        assert_eq!(
            body.pointer("/input/2/content/0/text")
                .and_then(|v| v.as_str()),
            Some("再说一句")
        );
    }

    #[test]
    fn consecutive_text_blocks_still_collapse_into_one_message() {
        let mut request = request();
        request.messages = vec![Message::Assistant {
            content: vec![
                AssistantContent::Text("一".to_owned()),
                AssistantContent::Text("二".to_owned()),
            ],
        }];
        let body = openai().body(&request);
        assert_eq!(
            body.get("input").and_then(|v| v.as_array()).map(Vec::len),
            Some(1)
        );
        assert_eq!(
            body.pointer("/input/0/content")
                .and_then(|v| v.as_array())
                .map(Vec::len),
            Some(2)
        );
    }

    #[test]
    fn reasoning_items_replay_verbatim_so_encrypted_content_survives() {
        let item = serde_json::json!({
            "type": "reasoning",
            "id": "rs_1",
            "summary": [{ "type": "summary_text", "text": "摘要" }],
            "encrypted_content": "opaque"
        });
        let replayed = replay_reasoning(&ThinkingContent {
            text: "摘要".to_owned(),
            signature: Some(item.to_string()),
        });
        assert_eq!(replayed, Some(item));
    }

    #[test]
    fn thinking_without_a_replayable_item_is_dropped() {
        assert_eq!(
            replay_reasoning(&ThinkingContent {
                text: "只有文本".to_owned(),
                signature: None
            }),
            None
        );
        // 不是 reasoning item 的签名（例如 Anthropic 的签名串）不能送回去。
        assert_eq!(
            replay_reasoning(&ThinkingContent {
                text: String::new(),
                signature: Some("anthropic-signature".to_owned()),
            }),
            None
        );
        // 缺 id 的 item 会被服务端拒绝。
        assert_eq!(
            replay_reasoning(&ThinkingContent {
                text: String::new(),
                signature: Some(r#"{"type":"reasoning"}"#.to_owned()),
            }),
            None
        );
    }

    #[test]
    fn status_maps_onto_the_unified_stop_reason() {
        assert_eq!(map_status(Some("completed"), None), StopReason::Stop);
        assert_eq!(
            map_status(Some("incomplete"), Some("max_output_tokens")),
            StopReason::Length
        );
        assert_eq!(
            map_status(Some("incomplete"), Some("content_filter")),
            StopReason::Error
        );
        assert_eq!(map_status(Some("failed"), None), StopReason::Error);
        assert_eq!(map_status(Some("cancelled"), None), StopReason::Error);
        assert_eq!(map_status(None, None), StopReason::Stop);
    }

    #[test]
    fn usage_deducts_cached_tokens_and_reads_reasoning_details() {
        let usage = parse_usage(&serde_json::json!({
            "input_tokens": 100, "output_tokens": 30,
            "input_tokens_details": { "cached_tokens": 40 },
            "output_tokens_details": { "reasoning_tokens": 18 }
        }));
        assert_eq!(
            usage,
            Usage {
                input: 60,
                output: 30,
                cache_read: 40,
                cache_write: 0,
                reasoning: 18
            }
        );
    }

    #[tokio::test]
    async fn text_deltas_become_ordered_events() {
        let events = ok_events(vec![
            sse("response.created", serde_json::json!({ "response": { "id": "resp_1", "model": "gpt-5.2" } })),
            sse(
                "response.output_text.delta",
                serde_json::json!({ "item_id": "msg_1", "output_index": 0, "content_index": 0, "delta": "你" }),
            ),
            sse(
                "response.output_text.delta",
                serde_json::json!({ "item_id": "msg_1", "output_index": 0, "content_index": 0, "delta": "好" }),
            ),
            sse(
                "response.output_text.done",
                serde_json::json!({ "item_id": "msg_1", "output_index": 0, "content_index": 0, "text": "你好" }),
            ),
            sse(
                "response.completed",
                serde_json::json!({
                    "response": {
                        "id": "resp_1", "status": "completed",
                        "usage": { "input_tokens": 5, "output_tokens": 2 }
                    }
                }),
            ),
        ])
        .await;

        assert_eq!(
            events.first(),
            Some(&StreamEvent::Start {
                response_id: Some("resp_1".to_owned()),
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
                    input: 5,
                    output: 2,
                    ..Usage::default()
                },
            })
        );
    }

    #[tokio::test]
    async fn reasoning_summary_and_item_json_land_in_one_thinking_block() {
        let item = serde_json::json!({
            "type": "reasoning", "id": "rs_1", "encrypted_content": "opaque"
        });
        let events = ok_events(vec![
            sse("response.created", serde_json::json!({ "response": { "id": "r" } })),
            sse(
                "response.reasoning_summary_text.delta",
                serde_json::json!({ "item_id": "rs_1", "output_index": 0, "summary_index": 0, "delta": "推" }),
            ),
            sse(
                "response.reasoning_summary_text.delta",
                serde_json::json!({ "item_id": "rs_1", "output_index": 0, "summary_index": 1, "delta": "理" }),
            ),
            sse(
                "response.output_item.done",
                serde_json::json!({ "item_id": "rs_1", "output_index": 0, "item": item }),
            ),
            sse("response.completed", serde_json::json!({ "response": { "status": "completed" } })),
        ])
        .await;

        let thinking = events
            .iter()
            .find_map(|event| match event {
                StreamEvent::ThinkingEnd { content, .. } => Some(content.clone()),
                _ => None,
            })
            .expect("应当有思考块");
        // 两个 summary_index 必须并进同一个块。
        assert_eq!(thinking.text, "推理");
        assert_eq!(
            thinking.signature.as_deref(),
            Some(item.to_string().as_str())
        );
    }

    #[tokio::test]
    async fn a_reasoning_item_sent_twice_stays_replayable() {
        // 真实流对同一个 reasoning item 必发两次：`.added`（无 encrypted_content）
        // 与 `.done`（完整体）。签名若是追加，就会拼成非法 JSON，回放整块被丢。
        let added = serde_json::json!({ "type": "reasoning", "id": "rs_1", "summary": [] });
        let done = serde_json::json!({
            "type": "reasoning", "id": "rs_1",
            "summary": [{ "type": "summary_text", "text": "推理" }],
            "encrypted_content": "opaque"
        });
        let events = ok_events(vec![
            sse(
                "response.created",
                serde_json::json!({ "response": { "id": "r" } }),
            ),
            sse(
                "response.output_item.added",
                serde_json::json!({ "item_id": "rs_1", "output_index": 0, "item": added }),
            ),
            sse(
                "response.reasoning_summary_text.delta",
                serde_json::json!({
                    "item_id": "rs_1", "output_index": 0, "summary_index": 0, "delta": "推理"
                }),
            ),
            sse(
                "response.output_item.done",
                serde_json::json!({ "item_id": "rs_1", "output_index": 0, "item": done }),
            ),
            sse(
                "response.completed",
                serde_json::json!({ "response": { "status": "completed" } }),
            ),
        ])
        .await;

        let thinking = events
            .iter()
            .find_map(|event| match event {
                StreamEvent::ThinkingEnd { content, .. } => Some(content.clone()),
                _ => None,
            })
            .expect("应当有思考块");

        // 关键不变量：签名仍是可解析的单个 reasoning item，且带 encrypted_content。
        assert_eq!(replay_reasoning(&thinking), Some(done));
    }

    #[tokio::test]
    async fn function_call_arguments_are_reassembled() {
        let events = ok_events(vec![
            sse("response.created", serde_json::json!({ "response": { "id": "r" } })),
            sse(
                "response.output_item.added",
                serde_json::json!({
                    "item_id": "fc_1", "output_index": 0,
                    "item": { "type": "function_call", "call_id": "call_1", "name": "search" }
                }),
            ),
            sse(
                "response.function_call_arguments.delta",
                serde_json::json!({ "item_id": "fc_1", "output_index": 0, "delta": "{\"q\":" }),
            ),
            sse(
                "response.function_call_arguments.done",
                serde_json::json!({ "item_id": "fc_1", "output_index": 0, "arguments": "{\"q\":1}" }),
            ),
            sse("response.completed", serde_json::json!({ "response": { "status": "completed" } })),
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
    async fn incomplete_due_to_output_cap_reports_length() {
        let events = ok_events(vec![
            sse(
                "response.created",
                serde_json::json!({ "response": { "id": "r" } }),
            ),
            sse(
                "response.incomplete",
                serde_json::json!({
                    "response": {
                        "status": "incomplete",
                        "incomplete_details": { "reason": "max_output_tokens" }
                    }
                }),
            ),
        ])
        .await;
        assert!(matches!(
            events.last(),
            Some(StreamEvent::Done {
                stop_reason: StopReason::Length,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn content_filter_is_an_error_not_a_truncation() {
        let events = collect(vec![sse(
            "response.incomplete",
            serde_json::json!({
                "response": {
                    "status": "incomplete",
                    "incomplete_details": { "reason": "content_filter" }
                }
            }),
        )])
        .await;
        assert!(matches!(
            events.into_iter().find_map(Result::err),
            Some(AiError::Api { code, .. }) if code.as_deref() == Some("content_filter")
        ));
    }

    #[tokio::test]
    async fn response_failed_surfaces_the_upstream_error() {
        let events = collect(vec![sse(
            "response.failed",
            serde_json::json!({
                "response": { "error": { "code": "server_error", "message": "boom" } }
            }),
        )])
        .await;
        assert!(matches!(
            events.into_iter().find_map(Result::err),
            Some(AiError::Api { code, .. }) if code.as_deref() == Some("server_error")
        ));
    }

    #[tokio::test]
    async fn a_stream_cut_before_completion_is_a_protocol_error() {
        let events = collect(vec![
            sse(
                "response.created",
                serde_json::json!({ "response": { "id": "r", "model": "gpt-5.2" } }),
            ),
            sse(
                "response.output_text.delta",
                serde_json::json!({
                    "item_id": "msg_1", "output_index": 0, "content_index": 0, "delta": "半"
                }),
            ),
        ])
        .await;

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
    async fn unknown_events_are_ignored() {
        let events = ok_events(vec![
            sse(
                "response.output_text.annotation.added",
                serde_json::json!({}),
            ),
            sse(
                "response.completed",
                serde_json::json!({ "response": { "status": "completed" } }),
            ),
        ])
        .await;
        assert_eq!(events.len(), 1);
        assert!(matches!(events.first(), Some(StreamEvent::Done { .. })));
    }
}
