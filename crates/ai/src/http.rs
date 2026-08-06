//! 共享 HTTP 客户端与提供商无关的响应处理。
//!
//! 三家提供商共用同一个 [`reqwest::Client`]（连接池、代理、TLS 只配一次），共用
//! 同一套错误信封解析与 `retry-after` 解析。新增提供商时不要另建客户端。

use std::sync::OnceLock;
use std::time::Duration;

use futures_core::Stream;
use reqwest::header::{HeaderMap, HeaderValue};
use reqwest::{Client, RequestBuilder, Response, StatusCode};

use crate::error::{AiError, ApiErrorKind, AuthError};
use crate::sse::{self, SseEvent};
use crate::types::ProviderId;

/// 错误信息保留的最大字符数，与 oh-my-pi `OpenAIHttpError` 对齐。
const MAX_ERROR_DETAIL_CHARS: usize = 4096;

/// 建立连接的超时；流式响应本身不设总时长上限。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

static CLIENT: OnceLock<Result<Client, String>> = OnceLock::new();

/// 安装 rustls 的 process default crypto provider。
///
/// workspace 用 `reqwest/rustls-no-provider` + ring（见根 `Cargo.toml` 注释），
/// reqwest 在 `Client` 构造时若取不到 default provider 会 panic，所以必须先装。
fn install_crypto_provider() {
    static INSTALLED: OnceLock<()> = OnceLock::new();
    INSTALLED.get_or_init(|| {
        // 已被别处装过时返回 Err，那正是我们要的结果，忽略即可。
        drop(rustls::crypto::ring::default_provider().install_default());
    });
}

fn build_client() -> Result<Client, String> {
    install_crypto_provider();
    Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .user_agent(concat!("zcode/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|err| err.to_string())
}

/// 取得共享客户端。克隆代价等同 `Arc::clone`。
pub fn shared_client() -> Result<Client, AiError> {
    match CLIENT.get_or_init(build_client) {
        Ok(client) => Ok(client.clone()),
        Err(detail) => Err(AiError::ClientSetup(detail.clone())),
    }
}

/// 取得共享客户端，错误按鉴权层错误返回。
pub fn shared_client_for_auth() -> Result<Client, AuthError> {
    match CLIENT.get_or_init(build_client) {
        Ok(client) => Ok(client.clone()),
        Err(detail) => Err(AuthError::ClientSetup(detail.clone())),
    }
}

/// 去掉 base URL 末尾的斜杠，避免拼路径时出现 `//`。
#[must_use]
pub fn normalize_base_url(url: String) -> String {
    let trimmed = url.trim_end_matches('/');
    if trimmed.len() == url.len() {
        url
    } else {
        trimmed.to_owned()
    }
}

/// 把 `name: value` 写进 header map；值非法时记日志并跳过，不中断请求。
pub fn set_header(headers: &mut HeaderMap, name: &'static str, value: &str) {
    match HeaderValue::from_str(value) {
        Ok(value) => {
            drop(headers.insert(name, value));
        }
        Err(err) => tracing::warn!(header = name, error = %err, "header 值非法，已跳过"),
    }
}

/// 发送请求并把响应体解成 SSE 事件流；非 2xx 直接转成 [`AiError::Api`]。
pub async fn send_sse(
    provider: ProviderId,
    request: RequestBuilder,
) -> Result<impl Stream<Item = Result<SseEvent, AiError>> + Send, AiError> {
    let response = request
        .send()
        .await
        .map_err(|source| AiError::Transport { provider, source })?;
    let response = ensure_success(provider, response).await?;
    Ok(sse::decode_stream(provider, response.bytes_stream()))
}

/// 状态码非 2xx 时读出错误信封并转成 [`AiError::Api`]。
pub async fn ensure_success(provider: ProviderId, response: Response) -> Result<Response, AiError> {
    if response.status().is_success() {
        return Ok(response);
    }
    Err(api_error(provider, response).await)
}

/// 消费响应体，构造结构化 API 错误。
pub async fn api_error(provider: ProviderId, response: Response) -> AiError {
    let status = response.status();
    let retry_after = retry_after(response.headers());
    let body = response.text().await.unwrap_or_default();
    let envelope = ErrorEnvelope::parse(&body);
    let kind = classify(status, envelope.code.as_deref(), &envelope.message);
    AiError::Api {
        provider,
        status: status.as_u16(),
        kind,
        code: envelope.code,
        message: truncate_chars(&envelope.message, MAX_ERROR_DETAIL_CHARS),
        retry_after,
    }
}

/// 从响应头解析建议等待时长。
///
/// `retry-after-ms` 优先于 `retry-after`；后者只识别秒数形式，HTTP-date 形式
/// 视为无建议（不引日期解析依赖，宁可让调用方退避重试）。
#[must_use]
pub fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    if let Some(millis) = header_str(headers, "retry-after-ms").and_then(|v| v.parse::<f64>().ok())
        && millis.is_finite()
        && millis >= 0.0
    {
        return Some(Duration::from_secs_f64(millis / 1000.0));
    }
    header_str(headers, "retry-after")
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_secs)
}

fn header_str<'h>(headers: &'h HeaderMap, name: &str) -> Option<&'h str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

#[derive(Debug, Default)]
struct ErrorEnvelope {
    message: String,
    code: Option<String>,
}

impl ErrorEnvelope {
    /// 解析三家共有的几种错误体形状。
    ///
    /// - `{"error": {"message": .., "type": .., "code": ..}}`（OpenAI / Anthropic / xAI）
    /// - `{"error": "文本"}`
    /// - `{"message": ".."}`
    /// - 其余：原始文本
    fn parse(body: &str) -> Self {
        let trimmed = body.trim();
        if trimmed.is_empty() {
            return Self {
                message: "(响应体为空)".to_owned(),
                code: None,
            };
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
            return Self {
                message: trimmed.to_owned(),
                code: None,
            };
        };
        let error = value.get("error");
        if let Some(detail) = error.and_then(serde_json::Value::as_object) {
            let message = detail
                .get("message")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(trimmed)
                .to_owned();
            let code = detail
                .get("code")
                .and_then(serde_json::Value::as_str)
                .or_else(|| detail.get("type").and_then(serde_json::Value::as_str))
                .map(str::to_owned);
            return Self { message, code };
        }
        if let Some(text) = error.and_then(serde_json::Value::as_str) {
            return Self {
                message: text.to_owned(),
                code: None,
            };
        }
        if let Some(text) = value.get("message").and_then(serde_json::Value::as_str) {
            return Self {
                message: text.to_owned(),
                code: None,
            };
        }
        Self {
            message: trimmed.to_owned(),
            code: None,
        }
    }
}

/// 订阅额度耗尽的判定：等待无法恢复，只能换账号，因此与限流分开。
fn classify(status: StatusCode, code: Option<&str>, message: &str) -> ApiErrorKind {
    if matches!(code, Some("usage_limit_reached" | "usage_not_included")) {
        return ApiErrorKind::UsageLimit;
    }
    if status == StatusCode::FORBIDDEN && mentions_credit_exhaustion(message) {
        return ApiErrorKind::UsageLimit;
    }
    ApiErrorKind::from_status(status.as_u16())
}

fn mentions_credit_exhaustion(message: &str) -> bool {
    let lowered = message.to_ascii_lowercase();
    lowered.contains("run out of credits") || lowered.contains("spending-limit")
}

fn truncate_chars(text: &str, limit: usize) -> String {
    match text.char_indices().nth(limit) {
        None => text.to_owned(),
        Some((cut, _)) => {
            let mut out = text.get(..cut).unwrap_or(text).to_owned();
            out.push('…');
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            let Ok(value) = HeaderValue::from_str(value) else {
                continue;
            };
            let Ok(name) = reqwest::header::HeaderName::from_bytes(name.as_bytes()) else {
                continue;
            };
            drop(map.insert(name, value));
        }
        map
    }

    #[test]
    fn nested_error_object_yields_message_and_code() {
        let envelope = ErrorEnvelope::parse(
            r#"{"error":{"message":"bad request","type":"invalid_request_error"}}"#,
        );
        assert_eq!(envelope.message, "bad request");
        assert_eq!(envelope.code.as_deref(), Some("invalid_request_error"));
    }

    #[test]
    fn code_field_wins_over_type_field() {
        let envelope = ErrorEnvelope::parse(r#"{"error":{"message":"m","type":"t","code":"c"}}"#);
        assert_eq!(envelope.code.as_deref(), Some("c"));
    }

    #[test]
    fn plain_text_body_becomes_the_message() {
        let envelope = ErrorEnvelope::parse("upstream exploded");
        assert_eq!(envelope.message, "upstream exploded");
        assert_eq!(envelope.code, None);
    }

    #[test]
    fn string_error_and_bare_message_shapes_are_understood() {
        assert_eq!(ErrorEnvelope::parse(r#"{"error":"nope"}"#).message, "nope");
        assert_eq!(
            ErrorEnvelope::parse(r#"{"message":"nope"}"#).message,
            "nope"
        );
    }

    #[test]
    fn retry_after_ms_takes_priority_and_supports_fractions() {
        let map = headers(&[("retry-after-ms", "1500"), ("retry-after", "9")]);
        assert_eq!(retry_after(&map), Some(Duration::from_millis(1500)));
    }

    #[test]
    fn retry_after_seconds_is_used_when_ms_absent() {
        assert_eq!(
            retry_after(&headers(&[("retry-after", "7")])),
            Some(Duration::from_secs(7))
        );
    }

    #[test]
    fn http_date_retry_after_is_reported_as_no_advice() {
        let map = headers(&[("retry-after", "Wed, 21 Oct 2026 07:28:00 GMT")]);
        assert_eq!(retry_after(&map), None);
    }

    #[test]
    fn codex_usage_limit_code_outranks_status_classification() {
        let kind = classify(
            StatusCode::TOO_MANY_REQUESTS,
            Some("usage_limit_reached"),
            "",
        );
        assert_eq!(kind, ApiErrorKind::UsageLimit);
    }

    #[test]
    fn xai_credit_exhaustion_is_a_usage_limit_not_an_auth_failure() {
        let kind = classify(StatusCode::FORBIDDEN, None, "You have run out of credits");
        assert_eq!(kind, ApiErrorKind::UsageLimit);
    }

    #[test]
    fn plain_forbidden_stays_an_authentication_error() {
        assert_eq!(
            classify(StatusCode::FORBIDDEN, None, "forbidden"),
            ApiErrorKind::Authentication
        );
    }

    #[test]
    fn truncation_respects_char_boundaries() {
        let text = "中文".repeat(4000);
        let cut = truncate_chars(&text, 10);
        assert_eq!(cut.chars().count(), 11);
        assert!(cut.ends_with('…'));
    }
}
