//! `zcode-ai` 的错误类型。
//!
//! 分两层：[`AuthError`] 覆盖凭据存取与 OAuth 流程，[`AiError`] 覆盖推理请求本身并把
//! 前者透传上来。调用方靠 [`ApiErrorKind`] 与 [`AiError::retry_after`] 决定重试策略，
//! 不要去匹配错误字符串。

use std::time::Duration;

use crate::types::ProviderId;

/// 提供商返回的 HTTP 错误按语义归类的结果。
///
/// 对齐 oh-my-pi `error/gateway.ts` 的分类：401/403 → 鉴权，429 → 限流，
/// 4xx 其余 → 请求非法，5xx → 上游故障。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiErrorKind {
    /// 凭据无效、过期或权限不足（HTTP 401/403）。
    Authentication,
    /// 触发速率限制（HTTP 429）。
    RateLimit,
    /// 订阅额度耗尽——与 [`ApiErrorKind::RateLimit`] 不同，等待无法恢复，只能换账号。
    UsageLimit,
    /// 请求本身非法（HTTP 400 等 4xx）。
    InvalidRequest,
    /// 上游过载，可退避重试（HTTP 5xx 或 `overloaded_error`）。
    Upstream,
}

impl ApiErrorKind {
    /// 按 HTTP 状态码归类。
    #[must_use]
    pub fn from_status(status: u16) -> Self {
        match status {
            401 | 403 => Self::Authentication,
            429 => Self::RateLimit,
            400..=499 => Self::InvalidRequest,
            _ => Self::Upstream,
        }
    }

    /// 该类错误是否值得原样重试。
    #[must_use]
    pub fn is_retryable(self) -> bool {
        matches!(self, Self::RateLimit | Self::Upstream)
    }
}

/// 凭据存取与 OAuth 流程的错误。
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    /// 存储里没有该提供商的可用凭据。
    #[error("未找到 {0} 的凭据，请先执行登录")]
    Missing(ProviderId),
    /// 有 OAuth 凭据但刷新失败，且失败是确定性的（refresh token 已失效）。
    #[error("{provider} 的 OAuth 凭据刷新失败：{detail}")]
    RefreshFailed {
        /// 出问题的提供商。
        provider: ProviderId,
        /// 上游给出的原因。
        detail: String,
    },
    /// 授权服务器明确拒绝（`error=access_denied` 之类）。
    #[error("{provider} 拒绝授权：{error}{}", .description.as_deref().map_or(String::new(), |d| format!("（{d}）")))]
    Denied {
        /// 出问题的提供商。
        provider: ProviderId,
        /// OAuth `error` 字段。
        error: String,
        /// OAuth `error_description` 字段。
        description: Option<String>,
    },
    /// 交互式流程超时（用户没有在时限内完成授权）。
    #[error("{provider} 的授权流程超时")]
    Timeout {
        /// 出问题的提供商。
        provider: ProviderId,
    },
    /// 授权服务器的响应缺字段或形状不对。
    #[error("{provider} 的授权响应无法解析：{detail}")]
    Protocol {
        /// 出问题的提供商。
        provider: ProviderId,
        /// 具体缺什么。
        detail: String,
    },
    /// 与授权服务器通信失败。
    #[error("授权请求失败：{0}")]
    Transport(#[from] reqwest::Error),
    /// 凭据文件读写失败。
    #[error("凭据存储读写失败：{0}")]
    Io(#[from] std::io::Error),
    /// 凭据文件内容不是合法 JSON。
    #[error("凭据存储内容损坏：{0}")]
    Corrupt(#[source] serde_json::Error),
    /// 共享 HTTP 客户端初始化失败。
    #[error("HTTP 客户端初始化失败：{0}")]
    ClientSetup(String),
    /// 操作系统熵源不可用。
    ///
    /// PKCE verifier 与 CSRF state 必须不可预测，取不到随机数只能中止登录，
    /// 绝不能降级成固定值。
    #[error("系统熵源不可用，无法生成 OAuth 随机数：{0}")]
    Entropy(String),
}

/// 推理请求的错误。
#[derive(Debug, thiserror::Error)]
pub enum AiError {
    /// 鉴权层错误。
    #[error(transparent)]
    Auth(#[from] AuthError),
    /// 共享 HTTP 客户端初始化失败。
    #[error("HTTP 客户端初始化失败：{0}")]
    ClientSetup(String),
    /// 建立连接或读取响应体失败。
    #[error("{provider} 请求失败：{source}")]
    Transport {
        /// 出问题的提供商。
        provider: ProviderId,
        /// 底层 reqwest 错误。
        #[source]
        source: reqwest::Error,
    },
    /// 提供商返回了非 2xx 响应。
    #[error("{provider} 返回 HTTP {status}：{message}")]
    Api {
        /// 出问题的提供商。
        provider: ProviderId,
        /// HTTP 状态码。
        status: u16,
        /// 归类结果。
        kind: ApiErrorKind,
        /// 响应体里的 `error.type` / `error.code`。
        code: Option<String>,
        /// 面向用户的错误描述。
        message: String,
        /// `retry-after` / `retry-after-ms` 解析出的建议等待时长。
        retry_after: Option<Duration>,
    },
    /// 流式响应违反协议（缺终止事件、事件字段缺失等）。
    #[error("{provider} 的流式响应异常：{detail}")]
    Protocol {
        /// 出问题的提供商。
        provider: ProviderId,
        /// 具体哪里不对。
        detail: String,
    },
    /// 响应 JSON 无法按预期结构解析。
    #[error("{provider} 的响应无法解析：{source}")]
    Decode {
        /// 出问题的提供商。
        provider: ProviderId,
        /// 底层 serde 错误。
        #[source]
        source: serde_json::Error,
    },
    /// 调用方主动取消。
    #[error("请求已被取消")]
    Aborted,
}

impl AiError {
    /// 该错误建议等待多久后重试，没有建议时返回 `None`。
    #[must_use]
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::Api { retry_after, .. } => *retry_after,
            _ => None,
        }
    }

    /// 该错误是否值得重试。
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Api { kind, .. } => kind.is_retryable(),
            Self::Transport { source, .. } => source.is_timeout() || source.is_connect(),
            _ => false,
        }
    }

    /// 该错误是否指向凭据问题——调用方据此决定刷新 token 或换账号。
    #[must_use]
    pub fn is_auth_failure(&self) -> bool {
        match self {
            Self::Auth(_) => true,
            Self::Api { kind, .. } => {
                matches!(
                    kind,
                    ApiErrorKind::Authentication | ApiErrorKind::UsageLimit
                )
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_classification_matches_gateway_rules() {
        assert_eq!(ApiErrorKind::from_status(401), ApiErrorKind::Authentication);
        assert_eq!(ApiErrorKind::from_status(403), ApiErrorKind::Authentication);
        assert_eq!(ApiErrorKind::from_status(429), ApiErrorKind::RateLimit);
        assert_eq!(ApiErrorKind::from_status(400), ApiErrorKind::InvalidRequest);
        assert_eq!(ApiErrorKind::from_status(404), ApiErrorKind::InvalidRequest);
        assert_eq!(ApiErrorKind::from_status(500), ApiErrorKind::Upstream);
        assert_eq!(ApiErrorKind::from_status(503), ApiErrorKind::Upstream);
    }

    #[test]
    fn only_transient_kinds_are_retryable() {
        assert!(ApiErrorKind::RateLimit.is_retryable());
        assert!(ApiErrorKind::Upstream.is_retryable());
        assert!(!ApiErrorKind::Authentication.is_retryable());
        assert!(!ApiErrorKind::InvalidRequest.is_retryable());
        assert!(!ApiErrorKind::UsageLimit.is_retryable());
    }

    #[test]
    fn usage_limit_counts_as_auth_failure_but_not_retryable() {
        let err = AiError::Api {
            provider: ProviderId::Xai,
            status: 403,
            kind: ApiErrorKind::UsageLimit,
            code: Some("spending-limit".to_owned()),
            message: "run out of credits".to_owned(),
            retry_after: None,
        };
        assert!(err.is_auth_failure());
        assert!(!err.is_retryable());
    }
}
