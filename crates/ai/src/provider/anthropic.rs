//! Anthropic Messages API 适配器。
//!
//! 两条鉴权分支的差别远不止 header 名字：
//!
//! | | API key | Claude Code OAuth |
//! | --- | --- | --- |
//! | 鉴权头 | `x-api-key` | `Authorization: Bearer` |
//! | User-Agent | 默认 | `claude-cli/…` |
//! | `anthropic-beta` | 按需 | 固定一整套 |
//! | system 首块 | 调用方的 | **强制**注入 Claude Agent SDK 身份句 |
//! | `max_tokens` | 调用方给的 | 夹到 64000 |
//! | 无工具时 | 省略 `tools` | 仍发 `tools: []` |
//!
//! OAuth 分支缺任何一项都会被判定成"不是 Claude Code"而拒绝，所以它们必须成套
//! 出现，不能只挑一两个。

use std::sync::Arc;

use async_trait::async_trait;
use futures_util::StreamExt as _;
use reqwest::Client;
use reqwest::header::HeaderMap;

use crate::auth::AuthStore;
use crate::auth::credential::{Access, AccessKind};
use crate::error::{AiError, ApiErrorKind};
use crate::http;
use crate::provider::block::Blocks;
use crate::provider::{EventStream, Provider};
use crate::sse::SseEvent;
use crate::types::{
    AssistantContent, CacheRetention, CompletionRequest, Message, ProviderId, ServiceTier,
    StopReason, StreamEvent, Thinking, Tool, ToolChoice, ToolResultContent, Usage, UserContent,
};

/// 默认 API 根地址。
pub const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";

/// Messages 端点路径。
const MESSAGES_PATH: &str = "/v1/messages";

/// API 版本头取值。
const API_VERSION: &str = "2023-06-01";

/// Claude Code 的 User-Agent。
const CLAUDE_CODE_USER_AGENT: &str = "claude-cli/2.1.220 (external, claude-desktop)";

/// OAuth 分支强制注入的 system 首块。
///
/// 少了这一句，Anthropic 会判定调用方不是 Claude Code 并拒绝 OAuth 令牌。
const CLAUDE_CODE_IDENTITY: &str = "You are a Claude agent, built on Anthropic's Claude Agent SDK.";

/// OAuth 分支固定发送的 beta 列表。
const OAUTH_BETAS: &[&str] = &[
    "claude-code-20250219",
    "interleaved-thinking-2025-05-14",
    "thinking-token-count-2026-05-13",
    "context-management-2025-06-27",
    "prompt-caching-scope-2026-01-05",
    "mid-conversation-system-2026-04-07",
    "advanced-tool-use-2025-11-20",
];

/// 1 小时缓存需要的 beta（仅 API key 分支要显式声明）。
const EXTENDED_CACHE_BETA: &str = "extended-cache-ttl-2025-04-11";

/// OAuth 分支的输出上限。
const CLAUDE_CODE_MAX_OUTPUT_TOKENS: u32 = 64_000;

/// 没给 `max_tokens` 时的兜底值。
const DEFAULT_MAX_OUTPUT_TOKENS: u32 = 32_000;

/// 开思考但没给预算时的最小预算。
const MIN_THINKING_BUDGET: u32 = 1024;

/// 全请求最多几个缓存断点，超过会被服务端拒绝。
const MAX_CACHE_BREAKPOINTS: usize = 4;

/// Anthropic Messages 适配器。
#[derive(Debug, Clone)]
pub struct AnthropicProvider {
    auth: Arc<AuthStore>,
    client: Client,
    base_url: String,
}

impl AnthropicProvider {
    /// 用共享 HTTP 客户端和默认 base URL 构造。
    pub fn new(auth: Arc<AuthStore>) -> Result<Self, AiError> {
        Ok(Self {
            auth,
            client: http::shared_client()?,
            base_url: DEFAULT_BASE_URL.to_owned(),
        })
    }

    /// 覆盖 base URL（自建网关、代理）。
    #[must_use]
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = http::normalize_base_url(base_url.into());
        self
    }

    /// 覆盖 HTTP 客户端。
    #[must_use]
    pub fn with_client(mut self, client: Client) -> Self {
        self.client = client;
        self
    }

    fn endpoint(&self, access: &Access) -> String {
        // OAuth 客户端要走 beta 通道，才能拿到 Claude Code 专属能力。
        if access.kind == AccessKind::OAuth {
            format!("{}{MESSAGES_PATH}?beta=true", self.base_url)
        } else {
            format!("{}{MESSAGES_PATH}", self.base_url)
        }
    }

    fn headers(access: &Access, request: &CompletionRequest) -> HeaderMap {
        let mut headers = HeaderMap::new();
        http::set_header(&mut headers, "content-type", "application/json");
        http::set_header(&mut headers, "anthropic-version", API_VERSION);
        http::set_header(&mut headers, "x-app", "cli");

        match access.kind {
            AccessKind::OAuth => {
                http::set_header(
                    &mut headers,
                    "authorization",
                    &format!("Bearer {}", access.token),
                );
                http::set_header(&mut headers, "user-agent", CLAUDE_CODE_USER_AGENT);
                http::set_header(&mut headers, "accept", "application/json");
                if let Some(session) = request.session_id.as_deref() {
                    http::set_header(&mut headers, "x-claude-code-session-id", session);
                }
            }
            AccessKind::ApiKey => {
                http::set_header(&mut headers, "x-api-key", &access.token);
                http::set_header(&mut headers, "accept", "text/event-stream");
            }
        }
        let betas = Self::betas(access, request);
        if !betas.is_empty() {
            http::set_header(&mut headers, "anthropic-beta", &betas.join(","));
        }
        headers
    }

    fn betas(access: &Access, request: &CompletionRequest) -> Vec<String> {
        let mut betas: Vec<String> = Vec::new();
        if access.kind == AccessKind::OAuth {
            betas.extend(OAUTH_BETAS.iter().map(|beta| (*beta).to_owned()));
        } else if request.cache_retention == CacheRetention::Long {
            // OAuth 通道默认就带 1h 缓存能力，只有 API key 分支需要显式声明。
            betas.push(EXTENDED_CACHE_BETA.to_owned());
        }
        betas
    }

    fn body(access: &Access, request: &CompletionRequest) -> serde_json::Value {
        let oauth = access.kind == AccessKind::OAuth;
        let mut body = serde_json::Map::new();
        drop(body.insert("model".to_owned(), request.model.clone().into()));
        drop(body.insert("max_tokens".to_owned(), max_tokens(request, oauth).into()));
        drop(body.insert("stream".to_owned(), true.into()));
        drop(body.insert("messages".to_owned(), encode_messages(&request.messages)));

        let system = encode_system(request, oauth);
        if !system.is_empty() {
            drop(body.insert("system".to_owned(), system.into()));
        }
        // OAuth 分支即使没有工具也要发空数组，服务端据此识别 Claude Code。
        if !request.tools.is_empty() || oauth {
            drop(body.insert("tools".to_owned(), encode_tools(&request.tools)));
        }
        if let Some(choice) = encode_tool_choice(&request.tool_choice) {
            drop(body.insert("tool_choice".to_owned(), choice));
        }
        if let Some(thinking) = encode_thinking(request.thinking) {
            drop(body.insert("thinking".to_owned(), thinking));
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
            drop(body.insert("stop_sequences".to_owned(), stops.into()));
        }
        // Anthropic 不认 `service_tier`：优先级档位走 `speed: "fast"`。
        if request.service_tier == Some(ServiceTier::Priority) {
            drop(body.insert("speed".to_owned(), "fast".into()));
        }
        let mut value = serde_json::Value::Object(body);
        apply_cache_control(&mut value, request.cache_retention);
        value
    }
}

#[async_trait]
impl Provider for AnthropicProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Anthropic
    }

    async fn stream(&self, request: &CompletionRequest) -> Result<EventStream, AiError> {
        let access = self.auth.access(ProviderId::Anthropic).await?;
        let http_request = self
            .client
            .post(self.endpoint(&access))
            .headers(Self::headers(&access, request))
            .json(&Self::body(&access, request));

        let events = http::send_sse(ProviderId::Anthropic, http_request).await?;
        Ok(Box::pin(decode(events)))
    }
}

struct DecodeState<S> {
    events: std::pin::Pin<Box<S>>,
    blocks: Blocks,
    pending: std::collections::VecDeque<StreamEvent>,
    usage: Usage,
    finished: bool,
}

/// 把 Anthropic 的 SSE 事件流翻成统一事件流。
fn decode<S>(events: S) -> impl futures_core::Stream<Item = Result<StreamEvent, AiError>> + Send
where
    S: futures_core::Stream<Item = Result<SseEvent, AiError>> + Send + 'static,
{
    let state = DecodeState {
        events: Box::pin(events),
        blocks: Blocks::new(),
        pending: std::collections::VecDeque::new(),
        usage: Usage::default(),
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
                Some(Ok(sse)) => {
                    let mut out = Vec::new();
                    match handle_event(&sse, &mut state.blocks, &mut state.usage, &mut out) {
                        Ok(Some(stop_reason)) => {
                            state.blocks.close_all(&mut out);
                            out.push(StreamEvent::Done {
                                stop_reason: promote(stop_reason, &state.blocks),
                                usage: state.usage,
                            });
                            state.finished = true;
                        }
                        Ok(None) => {}
                        Err(err) => {
                            state.finished = true;
                            return Some((Err(err), state));
                        }
                    }
                    state.pending.extend(out);
                }
                Some(Err(err)) => {
                    state.finished = true;
                    return Some((Err(err), state));
                }
                None => {
                    // 走到这里说明连接断在 `message_delta` / `message_stop` 之前。
                    // 此刻手上是半截文本或没闭合的工具 JSON，报成功等于把残缺内容
                    // 当完整结果交出去。
                    state.finished = true;
                    return Some((
                        Err(AiError::Protocol {
                            provider: ProviderId::Anthropic,
                            detail: "流在收到 message_stop / stop_reason 之前就结束了".to_owned(),
                        }),
                        state,
                    ));
                }
            }
        }
    })
}

/// 处理一条 SSE 事件；返回 `Some(stop_reason)` 表示流已终止。
fn handle_event(
    sse: &SseEvent,
    blocks: &mut Blocks,
    usage: &mut Usage,
    out: &mut Vec<StreamEvent>,
) -> Result<Option<StopReason>, AiError> {
    let Ok(payload) = serde_json::from_str::<serde_json::Value>(&sse.data) else {
        // `ping` 之类没有 JSON 负载的事件直接跳过。
        return Ok(None);
    };
    let kind = sse
        .event
        .as_deref()
        .or_else(|| payload.get("type").and_then(serde_json::Value::as_str))
        .unwrap_or_default();

    match kind {
        "message_start" => {
            let message = payload.get("message");
            out.push(StreamEvent::Start {
                response_id: message.and_then(|m| str_field(m, "id")),
                model: message.and_then(|m| str_field(m, "model")),
            });
            if let Some(raw) = message.and_then(|m| m.get("usage")) {
                merge_usage(usage, raw);
            }
            Ok(None)
        }
        "content_block_start" => {
            open_content_block(&payload, blocks, out);
            Ok(None)
        }
        "content_block_delta" => {
            apply_content_block_delta(&payload, blocks, out);
            Ok(None)
        }
        "content_block_stop" => {
            blocks.close(&block_key(&payload), out);
            Ok(None)
        }
        "message_delta" => {
            if let Some(raw) = payload.get("usage") {
                merge_usage(usage, raw);
            }
            Ok(payload
                .get("delta")
                .and_then(|d| str_field(d, "stop_reason"))
                .map(|reason| map_stop_reason(&reason)))
        }
        "message_stop" => Ok(Some(StopReason::Stop)),
        "error" => {
            let error = payload.get("error");
            Err(AiError::Api {
                provider: ProviderId::Anthropic,
                // 流内错误没有 HTTP 状态码；`overloaded_error` 一类按上游故障处理。
                status: 200,
                kind: ApiErrorKind::Upstream,
                code: error.and_then(|e| str_field(e, "type")),
                message: error
                    .and_then(|e| str_field(e, "message"))
                    .unwrap_or_else(|| "上游返回未描述的流内错误".to_owned()),
                retry_after: None,
            })
        }
        _ => Ok(None),
    }
}

/// 按 `content_block.type` 开一个内容块。
fn open_content_block(
    payload: &serde_json::Value,
    blocks: &mut Blocks,
    out: &mut Vec<StreamEvent>,
) {
    let index = block_key(payload);
    let block = payload.get("content_block");
    match block
        .and_then(|b| b.get("type"))
        .and_then(serde_json::Value::as_str)
    {
        Some("tool_use") => {
            let id = block.and_then(|b| str_field(b, "id")).unwrap_or_default();
            let name = block.and_then(|b| str_field(b, "name")).unwrap_or_default();
            blocks.tool_start(&index, &id, &name, out);
        }
        Some("thinking") => blocks.open_thinking(&index, out),
        Some("redacted_thinking") => {
            // 正文已被服务端加密，整块一次到齐，回放时必须编回 `redacted_thinking`，
            // 塞进普通 thinking 块会让服务端签名校验失败。
            let data = block.and_then(|b| str_field(b, "data")).unwrap_or_default();
            blocks.redacted_thinking(&index, &data, out);
        }
        _ => blocks.open_text(&index, out),
    }
}

/// 按 `delta.type` 把增量追加到对应内容块。
fn apply_content_block_delta(
    payload: &serde_json::Value,
    blocks: &mut Blocks,
    out: &mut Vec<StreamEvent>,
) {
    let index = block_key(payload);
    let delta = payload.get("delta");
    let kind = delta
        .and_then(|d| d.get("type"))
        .and_then(serde_json::Value::as_str);
    let Some(kind) = kind else { return };
    match kind {
        "text_delta" => {
            if let Some(text) = delta.and_then(|d| str_field(d, "text")) {
                blocks.text_delta(&index, &text, out);
            }
        }
        "thinking_delta" => {
            if let Some(text) = delta.and_then(|d| str_field(d, "thinking")) {
                blocks.thinking_delta(&index, &text, out);
            }
        }
        "signature_delta" => {
            if let Some(signature) = delta.and_then(|d| str_field(d, "signature")) {
                blocks.append_thinking_signature(&index, &signature, out);
            }
        }
        "input_json_delta" => {
            if let Some(json) = delta.and_then(|d| str_field(d, "partial_json")) {
                blocks.tool_delta(&index, &json, out);
            }
        }
        _ => {}
    }
}

/// 已经产生工具调用却报 `end_turn` 时提升为 `tool_use`。
fn promote(stop_reason: StopReason, blocks: &Blocks) -> StopReason {
    if stop_reason == StopReason::Stop && blocks.has_tool_calls() {
        StopReason::ToolUse
    } else {
        stop_reason
    }
}

fn block_key(payload: &serde_json::Value) -> String {
    payload
        .get("index")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0)
        .to_string()
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

/// 累积 usage。
///
/// `message_start` 给初值、`message_delta` 给终值，后者只带变化的字段，因此这里
/// 是"有就覆盖"，不是相加。
fn merge_usage(usage: &mut Usage, raw: &serde_json::Value) {
    if let Some(value) = u64_field(raw, "input_tokens") {
        usage.input = value;
    }
    if let Some(value) = u64_field(raw, "output_tokens") {
        usage.output = value;
    }
    if let Some(value) = u64_field(raw, "cache_creation_input_tokens") {
        usage.cache_write = value;
    }
    if let Some(value) = u64_field(raw, "cache_read_input_tokens") {
        usage.cache_read = value;
    }
}

/// Anthropic 的 `stop_reason` → 统一枚举。
fn map_stop_reason(reason: &str) -> StopReason {
    match reason {
        "max_tokens" | "model_context_window_exceeded" => StopReason::Length,
        "tool_use" => StopReason::ToolUse,
        "refusal" | "sensitive" => StopReason::Error,
        // `end_turn` / `stop_sequence` / `pause_turn` 以及未知取值都算正常结束。
        _ => StopReason::Stop,
    }
}

fn max_tokens(request: &CompletionRequest, oauth: bool) -> u32 {
    let requested = request
        .max_output_tokens
        .unwrap_or(DEFAULT_MAX_OUTPUT_TOKENS);
    if oauth {
        requested.min(CLAUDE_CODE_MAX_OUTPUT_TOKENS)
    } else {
        requested
    }
}

fn encode_system(request: &CompletionRequest, oauth: bool) -> Vec<serde_json::Value> {
    let mut blocks = Vec::new();
    if oauth {
        blocks.push(serde_json::json!({ "type": "text", "text": CLAUDE_CODE_IDENTITY }));
    }
    for prompt in &request.system {
        if !prompt.is_empty() {
            blocks.push(serde_json::json!({ "type": "text", "text": prompt }));
        }
    }
    blocks
}

/// 把统一消息翻成 Anthropic 的 `messages` 数组。
///
/// **连续的工具结果必须合并进同一条 user 消息**：并行工具调用时，assistant 那条
/// 消息里有 N 个 `tool_use`，服务端要求紧随其后的一条消息里凑齐全部 N 个
/// `tool_result`，拆成 N 条 user 消息会直接 400
/// （`tool_use ids were found without tool_result blocks immediately after`）。
fn encode_messages(messages: &[Message]) -> serde_json::Value {
    let mut out: Vec<serde_json::Value> = Vec::with_capacity(messages.len());
    let mut rest = messages;
    while let Some((head, tail)) = rest.split_first() {
        match head {
            Message::User { content } => {
                out.push(serde_json::json!({
                    "role": "user",
                    "content": content.iter().map(encode_user_content).collect::<Vec<_>>(),
                }));
                rest = tail;
            }
            Message::Assistant { content } => {
                out.push(serde_json::json!({
                    "role": "assistant",
                    "content": content.iter().map(encode_assistant_content).collect::<Vec<_>>(),
                }));
                rest = tail;
            }
            // 工具结果在 Anthropic 里是 user 消息里的 block，不是独立角色。
            Message::ToolResult { .. } => {
                let run_len = rest
                    .iter()
                    .take_while(|message| matches!(message, Message::ToolResult { .. }))
                    .count();
                let (run, remainder) = rest.split_at(run_len);
                out.push(serde_json::json!({
                    "role": "user",
                    "content": encode_tool_result_run(run),
                }));
                rest = remainder;
            }
        }
    }
    out.into()
}

/// 把一串连续的工具结果编成一条 user 消息的 content 数组。
///
/// 失败结果里的图片要摘出来放到所有 `tool_result` 之后：Anthropic 规定
/// `is_error` 为真时 `content` 只能是文本，内联图片会被 400 拒掉。
fn encode_tool_result_run(run: &[Message]) -> Vec<serde_json::Value> {
    let mut blocks = Vec::with_capacity(run.len());
    let mut hoisted = Vec::new();
    for message in run {
        let Message::ToolResult {
            tool_call_id,
            content,
            is_error,
            ..
        } = message
        else {
            continue;
        };
        if *is_error {
            for item in content {
                if let ToolResultContent::Image(image) = item {
                    hoisted.push(encode_image_block(image));
                }
            }
        }
        blocks.push(encode_tool_result(tool_call_id, content, *is_error));
    }
    blocks.extend(hoisted);
    blocks
}

fn encode_image_block(image: &crate::types::ImageContent) -> serde_json::Value {
    serde_json::json!({
        "type": "image",
        "source": {
            "type": "base64",
            "media_type": image.media_type,
            "data": image.data,
        },
    })
}

fn encode_user_content(content: &UserContent) -> serde_json::Value {
    match content {
        UserContent::Text(text) => serde_json::json!({ "type": "text", "text": text }),
        UserContent::Image(image) => encode_image_block(image),
    }
}

fn encode_assistant_content(content: &AssistantContent) -> serde_json::Value {
    match content {
        AssistantContent::Text(text) => serde_json::json!({ "type": "text", "text": text }),
        AssistantContent::Thinking(thinking) => serde_json::json!({
            "type": "thinking",
            "thinking": thinking.text,
            "signature": thinking.signature.clone().unwrap_or_default(),
        }),
        AssistantContent::RedactedThinking(data) => {
            serde_json::json!({ "type": "redacted_thinking", "data": data })
        }
        AssistantContent::ToolCall(call) => serde_json::json!({
            "type": "tool_use",
            "id": call.id,
            "name": call.name,
            "input": serde_json::from_str::<serde_json::Value>(&call.arguments)
                .unwrap_or_else(|_invalid| serde_json::json!({})),
        }),
    }
}

fn encode_tool_result(
    tool_call_id: &str,
    content: &[ToolResultContent],
    is_error: bool,
) -> serde_json::Value {
    let blocks: Vec<serde_json::Value> = content
        .iter()
        .filter_map(|item| match item {
            ToolResultContent::Text(text) => {
                Some(serde_json::json!({ "type": "text", "text": text }))
            }
            // 失败结果只能带文本，图片由 `encode_tool_result_run` 挂到块外。
            ToolResultContent::Image(_) if is_error => None,
            ToolResultContent::Image(image) => Some(encode_image_block(image)),
        })
        .collect();
    // 空内容会被服务端拒绝，失败时至少要有一句话。
    let blocks = if blocks.is_empty() {
        vec![serde_json::json!({ "type": "text", "text": "Tool failed with no output." })]
    } else {
        blocks
    };
    serde_json::json!({
        "type": "tool_result",
        "tool_use_id": tool_call_id,
        "content": blocks,
        "is_error": is_error,
    })
}

fn encode_tools(tools: &[Tool]) -> serde_json::Value {
    tools
        .iter()
        .map(|tool| {
            let mut entry = serde_json::Map::new();
            drop(entry.insert("name".to_owned(), tool.name.clone().into()));
            drop(entry.insert("description".to_owned(), tool.description.clone().into()));
            drop(entry.insert("input_schema".to_owned(), tool.parameters.clone()));
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
        // `auto` 是服务端默认，不必显式发。
        ToolChoice::Auto => None,
        ToolChoice::None => Some(serde_json::json!({ "type": "none" })),
        ToolChoice::Required => Some(serde_json::json!({ "type": "any" })),
        ToolChoice::Named(name) => Some(serde_json::json!({ "type": "tool", "name": name })),
    }
}

fn encode_thinking(thinking: Thinking) -> Option<serde_json::Value> {
    match thinking {
        Thinking::Disabled => None,
        Thinking::Budget { tokens } => Some(serde_json::json!({
            "type": "enabled",
            "budget_tokens": tokens.max(MIN_THINKING_BUDGET),
        })),
        // Anthropic 没有 effort 档位，交给服务端自适应定预算。
        Thinking::Effort(_) => Some(serde_json::json!({ "type": "adaptive" })),
    }
}

/// 打缓存断点。
///
/// 全请求最多 4 个。优先给 system（最稳定、复用率最高），剩下的额度从最新的消息
/// 往回发。thinking 块不允许带 `cache_control`，要跳过。
fn apply_cache_control(body: &mut serde_json::Value, retention: CacheRetention) {
    if retention == CacheRetention::None {
        return;
    }
    let marker = if retention == CacheRetention::Long {
        serde_json::json!({ "type": "ephemeral", "ttl": "1h" })
    } else {
        serde_json::json!({ "type": "ephemeral" })
    };

    let mut budget = MAX_CACHE_BREAKPOINTS;
    if let Some(system) = body
        .get_mut("system")
        .and_then(serde_json::Value::as_array_mut)
        && let Some(block) = system.last_mut().and_then(serde_json::Value::as_object_mut)
    {
        drop(block.insert("cache_control".to_owned(), marker.clone()));
        budget -= 1;
    }

    let Some(messages) = body
        .get_mut("messages")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return;
    };
    for message in messages.iter_mut().rev() {
        if budget == 0 {
            break;
        }
        let Some(content) = message
            .get_mut("content")
            .and_then(serde_json::Value::as_array_mut)
        else {
            continue;
        };
        let Some(block) = content
            .iter_mut()
            .rev()
            .filter_map(serde_json::Value::as_object_mut)
            .find(|block| {
                !matches!(
                    block.get("type").and_then(serde_json::Value::as_str),
                    Some("thinking" | "redacted_thinking")
                )
            })
        else {
            continue;
        };
        drop(block.insert("cache_control".to_owned(), marker.clone()));
        budget -= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::credential::{ApiKeyCredential, Credential, OAuthCredential, now_ms};
    use crate::auth::store::{CredentialStore, MemoryCredentialStore};
    use crate::types::{ImageContent, ThinkingContent, ToolCall};

    fn auth_with(credential: Credential) -> Arc<AuthStore> {
        let store = Arc::new(MemoryCredentialStore::new());
        store
            .save(ProviderId::Anthropic, credential)
            .expect("预置凭据");
        Arc::new(AuthStore::bare(store))
    }

    fn api_key_auth() -> Arc<AuthStore> {
        auth_with(Credential::ApiKey(ApiKeyCredential {
            key: "sk-ant".to_owned(),
        }))
    }

    fn oauth_auth() -> Arc<AuthStore> {
        auth_with(Credential::Oauth(OAuthCredential {
            access: "sk-ant-oat".to_owned(),
            refresh: "rt".to_owned(),
            expires: now_ms() + 3_600_000,
            account_id: Some("acct".to_owned()),
            email: None,
            plan: None,
            authorized_at: None,
        }))
    }

    fn provider(auth: Arc<AuthStore>) -> AnthropicProvider {
        AnthropicProvider {
            auth,
            client: http::shared_client().expect("HTTP 客户端"),
            base_url: DEFAULT_BASE_URL.to_owned(),
        }
    }

    fn api_key_access() -> Access {
        Access {
            token: "sk-ant".to_owned(),
            kind: AccessKind::ApiKey,
            account_id: None,
        }
    }

    fn oauth_access() -> Access {
        Access {
            token: "sk-ant-oat".to_owned(),
            kind: AccessKind::OAuth,
            account_id: Some("acct".to_owned()),
        }
    }

    fn request() -> CompletionRequest {
        CompletionRequest::new("claude-sonnet-4-6", vec![Message::user("hi")])
    }

    fn sse(event: &str, mut data: serde_json::Value) -> SseEvent {
        if let Some(object) = data.as_object_mut() {
            let _stamped = object.entry("type").or_insert_with(|| event.into());
        }
        SseEvent {
            event: Some(event.to_owned()),
            data: data.to_string(),
        }
    }

    async fn collect(events: Vec<SseEvent>) -> Vec<Result<StreamEvent, AiError>> {
        decode(futures_util::stream::iter(events.into_iter().map(Ok)))
            .collect::<Vec<_>>()
            .await
    }

    fn ok_events(events: Vec<Result<StreamEvent, AiError>>) -> Vec<StreamEvent> {
        events.into_iter().filter_map(Result::ok).collect()
    }

    #[test]
    fn oauth_and_api_key_take_different_endpoints() {
        let provider = provider(api_key_auth());
        assert_eq!(
            provider.endpoint(&api_key_access()),
            "https://api.anthropic.com/v1/messages"
        );
        assert_eq!(
            provider.endpoint(&oauth_access()),
            "https://api.anthropic.com/v1/messages?beta=true"
        );
    }

    #[tokio::test]
    async fn a_stored_oauth_credential_selects_the_beta_endpoint() {
        let provider = provider(oauth_auth());
        let access = provider
            .auth
            .access(ProviderId::Anthropic)
            .await
            .expect("解析凭据");

        assert_eq!(access.kind, AccessKind::OAuth);
        assert_eq!(
            provider.endpoint(&access),
            "https://api.anthropic.com/v1/messages?beta=true"
        );
        assert_eq!(
            AnthropicProvider::headers(&access, &request())
                .get("authorization")
                .and_then(|value| value.to_str().ok()),
            Some("Bearer sk-ant-oat")
        );
    }

    #[test]
    fn api_key_branch_uses_x_api_key_and_no_claude_code_fingerprint() {
        let headers = AnthropicProvider::headers(&api_key_access(), &request());
        assert_eq!(
            headers.get("x-api-key").and_then(|v| v.to_str().ok()),
            Some("sk-ant")
        );
        assert!(headers.get("authorization").is_none());
        assert!(headers.get("anthropic-beta").is_none());
        assert_eq!(
            headers.get("accept").and_then(|v| v.to_str().ok()),
            Some("text/event-stream")
        );
    }

    #[test]
    fn oauth_branch_sends_the_full_claude_code_fingerprint() {
        let headers = AnthropicProvider::headers(&oauth_access(), &request());
        assert_eq!(
            headers.get("authorization").and_then(|v| v.to_str().ok()),
            Some("Bearer sk-ant-oat")
        );
        assert!(headers.get("x-api-key").is_none());
        assert_eq!(
            headers.get("user-agent").and_then(|v| v.to_str().ok()),
            Some(CLAUDE_CODE_USER_AGENT)
        );
        let betas = headers
            .get("anthropic-beta")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        assert!(betas.contains("claude-code-20250219"), "{betas}");
        assert!(betas.contains("interleaved-thinking-2025-05-14"), "{betas}");
        // 1M 上下文 beta 是刻意不发的。
        assert!(!betas.contains("context-1m"), "{betas}");
    }

    #[test]
    fn extended_cache_beta_is_only_needed_on_the_api_key_branch() {
        let mut request = request();
        request.cache_retention = CacheRetention::Long;
        assert!(
            AnthropicProvider::betas(&api_key_access(), &request)
                .iter()
                .any(|b| b == EXTENDED_CACHE_BETA)
        );
        assert!(
            !AnthropicProvider::betas(&oauth_access(), &request)
                .iter()
                .any(|b| b == EXTENDED_CACHE_BETA)
        );
    }

    #[test]
    fn oauth_body_injects_the_agent_identity_before_caller_prompts() {
        let mut request = request();
        request.system = vec!["自定义指令".to_owned()];
        let body = AnthropicProvider::body(&oauth_access(), &request);

        assert_eq!(
            body.pointer("/system/0/text").and_then(|v| v.as_str()),
            Some(CLAUDE_CODE_IDENTITY)
        );
        assert_eq!(
            body.pointer("/system/1/text").and_then(|v| v.as_str()),
            Some("自定义指令")
        );
    }

    #[test]
    fn api_key_body_does_not_inject_the_identity_line() {
        let mut request = request();
        request.system = vec!["自定义指令".to_owned()];
        let body = AnthropicProvider::body(&api_key_access(), &request);
        assert_eq!(
            body.get("system").and_then(|v| v.as_array()).map(Vec::len),
            Some(1)
        );
    }

    #[test]
    fn oauth_clamps_max_tokens_but_api_key_does_not() {
        let mut request = request();
        request.max_output_tokens = Some(200_000);
        assert_eq!(
            AnthropicProvider::body(&oauth_access(), &request).get("max_tokens"),
            Some(&serde_json::json!(CLAUDE_CODE_MAX_OUTPUT_TOKENS))
        );
        assert_eq!(
            AnthropicProvider::body(&api_key_access(), &request).get("max_tokens"),
            Some(&serde_json::json!(200_000))
        );
    }

    #[test]
    fn oauth_sends_an_empty_tools_array_when_there_are_no_tools() {
        assert_eq!(
            AnthropicProvider::body(&oauth_access(), &request()).get("tools"),
            Some(&serde_json::json!([]))
        );
        assert!(
            AnthropicProvider::body(&api_key_access(), &request())
                .get("tools")
                .is_none()
        );
    }

    #[test]
    fn priority_tier_maps_to_speed_fast_not_service_tier() {
        let mut request = request();
        request.service_tier = Some(ServiceTier::Priority);
        let body = AnthropicProvider::body(&api_key_access(), &request);
        assert_eq!(body.get("speed"), Some(&serde_json::json!("fast")));
        assert!(body.get("service_tier").is_none());
    }

    #[test]
    fn tool_choice_uses_anthropic_vocabulary() {
        assert_eq!(encode_tool_choice(&ToolChoice::Auto), None);
        assert_eq!(
            encode_tool_choice(&ToolChoice::Required),
            Some(serde_json::json!({ "type": "any" }))
        );
        assert_eq!(
            encode_tool_choice(&ToolChoice::Named("search".to_owned())),
            Some(serde_json::json!({ "type": "tool", "name": "search" }))
        );
    }

    #[test]
    fn tool_results_are_encoded_as_user_messages() {
        let mut request = request();
        request.messages = vec![Message::ToolResult {
            tool_call_id: "call_1".to_owned(),
            tool_name: "search".to_owned(),
            content: vec![ToolResultContent::Text("结果".to_owned())],
            is_error: false,
        }];
        let body = AnthropicProvider::body(&api_key_access(), &request);
        assert_eq!(
            body.pointer("/messages/0/role").and_then(|v| v.as_str()),
            Some("user")
        );
        assert_eq!(
            body.pointer("/messages/0/content/0/type")
                .and_then(|v| v.as_str()),
            Some("tool_result")
        );
        assert_eq!(
            body.pointer("/messages/0/content/0/tool_use_id")
                .and_then(|v| v.as_str()),
            Some("call_1")
        );
    }

    #[test]
    fn parallel_tool_results_share_one_user_message() {
        let mut request = request();
        request.messages = vec![
            Message::Assistant {
                content: vec![
                    AssistantContent::ToolCall(ToolCall {
                        id: "call_1".to_owned(),
                        name: "a".to_owned(),
                        arguments: "{}".to_owned(),
                    }),
                    AssistantContent::ToolCall(ToolCall {
                        id: "call_2".to_owned(),
                        name: "b".to_owned(),
                        arguments: "{}".to_owned(),
                    }),
                ],
            },
            Message::ToolResult {
                tool_call_id: "call_1".to_owned(),
                tool_name: "a".to_owned(),
                content: vec![ToolResultContent::Text("一".to_owned())],
                is_error: false,
            },
            Message::ToolResult {
                tool_call_id: "call_2".to_owned(),
                tool_name: "b".to_owned(),
                content: vec![ToolResultContent::Text("二".to_owned())],
                is_error: false,
            },
        ];
        let body = AnthropicProvider::body(&api_key_access(), &request);
        let messages = body
            .get("messages")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        // 拆成两条 user 消息会让第二个 tool_use 没有紧随其后的结果块，直接 400。
        assert_eq!(messages.len(), 2, "并行工具结果必须合并进一条 user 消息");
        assert_eq!(
            body.pointer("/messages/1/content/0/tool_use_id")
                .and_then(|v| v.as_str()),
            Some("call_1")
        );
        assert_eq!(
            body.pointer("/messages/1/content/1/tool_use_id")
                .and_then(|v| v.as_str()),
            Some("call_2")
        );
    }

    #[test]
    fn images_are_hoisted_out_of_failed_tool_results() {
        let mut request = request();
        request.messages = vec![Message::ToolResult {
            tool_call_id: "call_1".to_owned(),
            tool_name: "shot".to_owned(),
            content: vec![
                ToolResultContent::Text("失败了".to_owned()),
                ToolResultContent::Image(ImageContent {
                    media_type: "image/png".to_owned(),
                    data: "QUJD".to_owned(),
                }),
            ],
            is_error: true,
        }];
        let body = AnthropicProvider::body(&api_key_access(), &request);

        // is_error 为真时 tool_result.content 只能是文本，否则 400。
        assert_eq!(
            body.pointer("/messages/0/content/0/content")
                .and_then(|v| v.as_array())
                .map(Vec::len),
            Some(1)
        );
        assert_eq!(
            body.pointer("/messages/0/content/0/content/0/type")
                .and_then(|v| v.as_str()),
            Some("text")
        );
        // 图片挂到 tool_result 块之后，不能直接丢。
        assert_eq!(
            body.pointer("/messages/0/content/1/type")
                .and_then(|v| v.as_str()),
            Some("image")
        );
    }

    #[test]
    fn successful_tool_results_keep_images_inline() {
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
        let body = AnthropicProvider::body(&api_key_access(), &request);
        assert_eq!(
            body.pointer("/messages/0/content/0/content/0/type")
                .and_then(|v| v.as_str()),
            Some("image")
        );
    }

    #[test]
    fn empty_tool_results_get_a_placeholder_instead_of_an_empty_array() {
        let mut request = request();
        request.messages = vec![Message::ToolResult {
            tool_call_id: "call_1".to_owned(),
            tool_name: "search".to_owned(),
            content: Vec::new(),
            is_error: true,
        }];
        let body = AnthropicProvider::body(&api_key_access(), &request);
        assert_eq!(
            body.pointer("/messages/0/content/0/content/0/text")
                .and_then(|v| v.as_str()),
            Some("Tool failed with no output.")
        );
    }

    #[test]
    fn assistant_history_round_trips_thinking_signature_and_tool_calls() {
        let mut request = request();
        request.messages = vec![Message::Assistant {
            content: vec![
                AssistantContent::Thinking(ThinkingContent {
                    text: "想".to_owned(),
                    signature: Some("sig".to_owned()),
                }),
                AssistantContent::ToolCall(ToolCall {
                    id: "call_1".to_owned(),
                    name: "search".to_owned(),
                    arguments: r#"{"q":1}"#.to_owned(),
                }),
            ],
        }];
        let body = AnthropicProvider::body(&api_key_access(), &request);
        assert_eq!(
            body.pointer("/messages/0/content/0/signature")
                .and_then(|v| v.as_str()),
            Some("sig")
        );
        assert_eq!(
            body.pointer("/messages/0/content/1/input/q")
                .and_then(serde_json::Value::as_u64),
            Some(1)
        );
    }

    #[test]
    fn malformed_tool_arguments_degrade_to_an_empty_object() {
        let mut request = request();
        request.messages = vec![Message::Assistant {
            content: vec![AssistantContent::ToolCall(ToolCall {
                id: "call_1".to_owned(),
                name: "search".to_owned(),
                arguments: "{not json".to_owned(),
            })],
        }];
        let body = AnthropicProvider::body(&api_key_access(), &request);
        assert_eq!(
            body.pointer("/messages/0/content/0/input"),
            Some(&serde_json::json!({}))
        );
    }

    #[test]
    fn images_are_encoded_as_base64_sources() {
        let mut request = request();
        request.messages = vec![Message::User {
            content: vec![UserContent::Image(ImageContent {
                media_type: "image/png".to_owned(),
                data: "QUJD".to_owned(),
            })],
        }];
        let body = AnthropicProvider::body(&api_key_access(), &request);
        assert_eq!(
            body.pointer("/messages/0/content/0/source/media_type")
                .and_then(|v| v.as_str()),
            Some("image/png")
        );
        assert_eq!(
            body.pointer("/messages/0/content/0/source/data")
                .and_then(|v| v.as_str()),
            Some("QUJD")
        );
    }

    #[test]
    fn cache_control_stays_within_four_breakpoints_and_skips_thinking_blocks() {
        let mut request = request();
        request.system = vec!["s".to_owned()];
        request.cache_retention = CacheRetention::Long;
        request.messages = vec![
            Message::user("1"),
            Message::Assistant {
                content: vec![AssistantContent::Thinking(ThinkingContent::default())],
            },
            Message::user("2"),
            Message::user("3"),
            Message::user("4"),
        ];
        let body = AnthropicProvider::body(&api_key_access(), &request);
        let serialized = body.to_string();
        assert_eq!(
            serialized.matches("cache_control").count(),
            MAX_CACHE_BREAKPOINTS
        );
        assert!(serialized.contains(r#""ttl":"1h""#));
        assert!(
            body.pointer("/messages/1/content/0/cache_control")
                .is_none(),
            "thinking 块被打了缓存断点"
        );
    }

    #[test]
    fn short_retention_omits_the_ttl_field() {
        let mut request = request();
        request.system = vec!["s".to_owned()];
        request.cache_retention = CacheRetention::Short;
        let serialized = AnthropicProvider::body(&api_key_access(), &request).to_string();
        assert!(serialized.contains("cache_control"));
        assert!(!serialized.contains("ttl"));
    }

    #[test]
    fn no_retention_adds_no_breakpoints() {
        let mut request = request();
        request.system = vec!["s".to_owned()];
        request.cache_retention = CacheRetention::None;
        let serialized = AnthropicProvider::body(&api_key_access(), &request).to_string();
        assert!(!serialized.contains("cache_control"));
    }

    #[test]
    fn thinking_budget_respects_the_service_minimum() {
        assert_eq!(
            encode_thinking(Thinking::Budget { tokens: 10 }),
            Some(serde_json::json!({ "type": "enabled", "budget_tokens": MIN_THINKING_BUDGET }))
        );
        assert_eq!(encode_thinking(Thinking::Disabled), None);
    }

    #[test]
    fn stop_reasons_map_onto_the_unified_enum() {
        assert_eq!(map_stop_reason("end_turn"), StopReason::Stop);
        assert_eq!(map_stop_reason("stop_sequence"), StopReason::Stop);
        assert_eq!(map_stop_reason("pause_turn"), StopReason::Stop);
        assert_eq!(map_stop_reason("max_tokens"), StopReason::Length);
        assert_eq!(
            map_stop_reason("model_context_window_exceeded"),
            StopReason::Length
        );
        assert_eq!(map_stop_reason("tool_use"), StopReason::ToolUse);
        assert_eq!(map_stop_reason("refusal"), StopReason::Error);
        assert_eq!(map_stop_reason("sensitive"), StopReason::Error);
        assert_eq!(map_stop_reason("brand_new_reason"), StopReason::Stop);
    }

    #[tokio::test]
    async fn text_stream_produces_ordered_events_and_final_usage() {
        let events = ok_events(
            collect(vec![
                sse(
                    "message_start",
                    serde_json::json!({
                        "message": {
                            "id": "msg_1", "model": "claude-sonnet-4-6",
                            "usage": { "input_tokens": 10, "cache_read_input_tokens": 4 }
                        }
                    }),
                ),
                sse(
                    "content_block_start",
                    serde_json::json!({ "index": 0, "content_block": { "type": "text" } }),
                ),
                sse(
                    "content_block_delta",
                    serde_json::json!({
                        "index": 0, "delta": { "type": "text_delta", "text": "你好" }
                    }),
                ),
                sse("content_block_stop", serde_json::json!({ "index": 0 })),
                sse(
                    "message_delta",
                    serde_json::json!({
                        "delta": { "stop_reason": "end_turn" },
                        "usage": { "output_tokens": 7 }
                    }),
                ),
            ])
            .await,
        );

        assert_eq!(
            events.first(),
            Some(&StreamEvent::Start {
                response_id: Some("msg_1".to_owned()),
                model: Some("claude-sonnet-4-6".to_owned()),
            })
        );
        assert!(events.contains(&StreamEvent::TextDelta {
            index: 0,
            delta: "你好".to_owned()
        }));
        assert!(events.contains(&StreamEvent::TextEnd {
            index: 0,
            text: "你好".to_owned()
        }));
        assert_eq!(
            events.last(),
            Some(&StreamEvent::Done {
                stop_reason: StopReason::Stop,
                usage: Usage {
                    input: 10,
                    output: 7,
                    cache_read: 4,
                    ..Usage::default()
                },
            })
        );
    }

    #[tokio::test]
    async fn thinking_signature_and_tool_arguments_are_reassembled() {
        let events = ok_events(
            collect(vec![
                sse(
                    "message_start",
                    serde_json::json!({ "message": { "id": "m" } }),
                ),
                sse(
                    "content_block_start",
                    serde_json::json!({ "index": 0, "content_block": { "type": "thinking" } }),
                ),
                sse(
                    "content_block_delta",
                    serde_json::json!({
                        "index": 0, "delta": { "type": "thinking_delta", "thinking": "推理" }
                    }),
                ),
                sse(
                    "content_block_delta",
                    serde_json::json!({
                        "index": 0, "delta": { "type": "signature_delta", "signature": "sig" }
                    }),
                ),
                sse("content_block_stop", serde_json::json!({ "index": 0 })),
                sse(
                    "content_block_start",
                    serde_json::json!({
                        "index": 1,
                        "content_block": { "type": "tool_use", "id": "call_1", "name": "search" }
                    }),
                ),
                sse(
                    "content_block_delta",
                    serde_json::json!({
                        "index": 1,
                        "delta": { "type": "input_json_delta", "partial_json": "{\"q\":" }
                    }),
                ),
                sse(
                    "content_block_delta",
                    serde_json::json!({
                        "index": 1, "delta": { "type": "input_json_delta", "partial_json": "1}" }
                    }),
                ),
                sse("content_block_stop", serde_json::json!({ "index": 1 })),
                sse(
                    "message_delta",
                    serde_json::json!({ "delta": { "stop_reason": "tool_use" } }),
                ),
            ])
            .await,
        );

        assert!(events.contains(&StreamEvent::ThinkingEnd {
            index: 0,
            content: ThinkingContent {
                text: "推理".to_owned(),
                signature: Some("sig".to_owned()),
            },
        }));
        assert!(events.contains(&StreamEvent::ToolCallEnd {
            index: 1,
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
    async fn tool_calls_promote_a_plain_stop_into_tool_use() {
        let events = ok_events(
            collect(vec![
                sse(
                    "message_start",
                    serde_json::json!({ "message": { "id": "m" } }),
                ),
                sse(
                    "content_block_start",
                    serde_json::json!({
                        "index": 0,
                        "content_block": { "type": "tool_use", "id": "c", "name": "n" }
                    }),
                ),
                sse("content_block_stop", serde_json::json!({ "index": 0 })),
                sse(
                    "message_delta",
                    serde_json::json!({ "delta": { "stop_reason": "end_turn" } }),
                ),
            ])
            .await,
        );
        assert!(matches!(
            events.last(),
            Some(StreamEvent::Done {
                stop_reason: StopReason::ToolUse,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn redacted_thinking_keeps_its_opaque_payload_for_replay() {
        let events = ok_events(
            collect(vec![
                sse(
                    "message_start",
                    serde_json::json!({ "message": { "id": "m" } }),
                ),
                sse(
                    "content_block_start",
                    serde_json::json!({
                        "index": 0,
                        "content_block": { "type": "redacted_thinking", "data": "opaque" }
                    }),
                ),
                sse("content_block_stop", serde_json::json!({ "index": 0 })),
                sse("message_stop", serde_json::json!({})),
            ])
            .await,
        );
        // 必须是独立的 RedactedThinking：编回历史时要还原成
        // `{"type":"redacted_thinking","data":…}`，塞进普通 thinking 块会被服务端
        // 判定签名非法。
        assert!(events.contains(&StreamEvent::RedactedThinking {
            index: 0,
            data: "opaque".to_owned(),
        }));
    }

    #[tokio::test]
    async fn in_band_error_events_surface_as_retryable_api_errors() {
        let events = collect(vec![
            sse(
                "message_start",
                serde_json::json!({ "message": { "id": "m" } }),
            ),
            sse(
                "error",
                serde_json::json!({
                    "error": { "type": "overloaded_error", "message": "Overloaded" }
                }),
            ),
        ])
        .await;

        let error = events
            .into_iter()
            .find_map(Result::err)
            .expect("应当有错误项");
        match error {
            AiError::Api { code, kind, .. } => {
                assert_eq!(code.as_deref(), Some("overloaded_error"));
                assert_eq!(kind, ApiErrorKind::Upstream);
                assert!(kind.is_retryable());
            }
            other => panic!("期望 Api 错误，实际 {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_truncated_stream_is_a_protocol_error_not_a_short_answer() {
        let events = collect(vec![
            sse(
                "message_start",
                serde_json::json!({ "message": { "id": "m" } }),
            ),
            sse(
                "content_block_delta",
                serde_json::json!({
                    "index": 0, "delta": { "type": "text_delta", "text": "半句" }
                }),
            ),
        ])
        .await;

        // 半截文本必须以错误收场：报成 Done 会让调用方把残缺回答当完整结果。
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
    async fn a_stream_cut_mid_tool_arguments_is_a_protocol_error() {
        let events = collect(vec![
            sse(
                "message_start",
                serde_json::json!({ "message": { "id": "m" } }),
            ),
            sse(
                "content_block_start",
                serde_json::json!({
                    "index": 0,
                    "content_block": { "type": "tool_use", "id": "c", "name": "n" }
                }),
            ),
            sse(
                "content_block_delta",
                serde_json::json!({
                    "index": 0,
                    "delta": { "type": "input_json_delta", "partial_json": "{\"q\":" }
                }),
            ),
        ])
        .await;
        assert!(matches!(
            events.into_iter().find_map(Result::err),
            Some(AiError::Protocol { .. })
        ));
    }

    #[tokio::test]
    async fn ping_events_are_ignored() {
        let events = ok_events(
            collect(vec![
                SseEvent {
                    event: Some("ping".to_owned()),
                    data: String::new(),
                },
                sse("message_stop", serde_json::json!({})),
            ])
            .await,
        );
        assert_eq!(events.len(), 1);
        assert!(matches!(events.first(), Some(StreamEvent::Done { .. })));
    }
}
