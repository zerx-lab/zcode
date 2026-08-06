//! xAI 适配器配置。
//!
//! xAI 没有自己的线格式，两条线路各自复用一个 OpenAI 兼容适配器：
//!
//! | Provider | 线格式 | 鉴权 |
//! | --- | --- | --- |
//! | `xai` | Chat Completions | 平台 API key |
//! | `xai-oauth` | Responses | SuperGrok 设备码 OAuth |
//!
//! 相对标准 OpenAI 的偏差都收在这里：
//!
//! - prompt 缓存亲和头是 `x-grok-conv-id`，不是 OpenAI 那套；
//! - `xai-oauth` 下 `reasoning.summary` **必须不发**，带上会被拒；
//! - 只有部分 Grok 型号认 `reasoning_effort`，其余会 400，因此 Chat 线路默认
//!   不发，由 [`chat_config_with_reasoning_effort`] 显式开；
//! - 不回传加密思考（`includeEncryptedReasoning` 为假）。

use std::sync::Arc;

use crate::auth::AuthStore;
use crate::error::AiError;
use crate::provider::openai_chat::{ChatConfig, ChatProvider};
use crate::provider::openai_responses::{Flavor, ResponsesConfig, ResponsesProvider};
use crate::types::ProviderId;

/// 两条线路共用的 API 根地址。
pub const BASE_URL: &str = "https://api.x.ai/v1";

/// prompt 缓存亲和头。
pub const CACHE_SESSION_HEADER: &str = "x-grok-conv-id";

/// 能接受 `reasoning_effort` 的模型 id 前缀。
///
/// 名单之外的 Grok 发了会 400，所以是白名单而非黑名单。
pub const REASONING_EFFORT_PREFIXES: &[&str] = &[
    "grok-3-mini",
    "grok-4.20-multi-agent",
    "grok-4.3",
    "grok-4.5",
];

/// 该模型是否接受 `reasoning_effort`。
#[must_use]
pub fn supports_reasoning_effort(model: &str) -> bool {
    REASONING_EFFORT_PREFIXES
        .iter()
        .any(|prefix| model.starts_with(prefix))
}

/// API key 线路的 Chat Completions 配置（不发 `reasoning_effort`）。
#[must_use]
pub fn chat_config() -> ChatConfig {
    chat_config_with_reasoning_effort(false)
}

/// API key 线路的配置，显式指定是否下发 `reasoning_effort`。
///
/// 调用方应先用 [`supports_reasoning_effort`] 判断目标模型。
#[must_use]
pub fn chat_config_with_reasoning_effort(supports_reasoning_effort: bool) -> ChatConfig {
    ChatConfig {
        provider: ProviderId::Xai,
        base_url: BASE_URL.to_owned(),
        // Grok 不认 `developer` 角色。
        system_role: "system",
        max_tokens_field: "max_completion_tokens",
        supports_reasoning_effort,
        cache_session_header: Some(CACHE_SESSION_HEADER),
        // Grok 的原生接口不认 `reasoning_content`。
        replay_reasoning_content: false,
    }
}

/// SuperGrok OAuth 线路的 Responses 配置。
#[must_use]
pub fn oauth_config() -> ResponsesConfig {
    ResponsesConfig {
        provider: ProviderId::XaiOAuth,
        base_url: BASE_URL.to_owned(),
        path: crate::provider::openai_responses::RESPONSES_PATH,
        flavor: Flavor::Standard,
        store: false,
        // xAI 不回传加密思考，请求 include 只会招来 400。
        include_encrypted_reasoning: false,
        // 带上 summary 会被拒——这是 xAI 与 OpenAI 最容易踩的一处差异。
        reasoning_summary: None,
        send_max_output_tokens: true,
        send_sampling_params: true,
        cache_session_header: Some(CACHE_SESSION_HEADER),
    }
}

/// 构造 API key 线路的适配器。
pub fn chat_provider(auth: Arc<AuthStore>, model: &str) -> Result<ChatProvider, AiError> {
    ChatProvider::new(
        auth,
        chat_config_with_reasoning_effort(supports_reasoning_effort(model)),
    )
}

/// 构造 SuperGrok OAuth 线路的适配器。
pub fn oauth_provider(auth: Arc<AuthStore>) -> Result<ResponsesProvider, AiError> {
    ResponsesProvider::new(auth, oauth_config())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_lines_share_the_same_base_url() {
        assert_eq!(chat_config().base_url, BASE_URL);
        assert_eq!(oauth_config().base_url, BASE_URL);
    }

    #[test]
    fn both_lines_use_the_grok_cache_affinity_header() {
        assert_eq!(
            chat_config().cache_session_header,
            Some(CACHE_SESSION_HEADER)
        );
        assert_eq!(
            oauth_config().cache_session_header,
            Some(CACHE_SESSION_HEADER)
        );
    }

    #[test]
    fn oauth_line_never_sends_a_reasoning_summary() {
        assert_eq!(oauth_config().reasoning_summary, None);
    }

    #[test]
    fn oauth_line_does_not_request_encrypted_reasoning() {
        assert!(!oauth_config().include_encrypted_reasoning);
    }

    #[test]
    fn reasoning_effort_is_allowlisted_by_model_prefix() {
        assert!(supports_reasoning_effort("grok-3-mini"));
        assert!(supports_reasoning_effort("grok-4.5-fast"));
        assert!(supports_reasoning_effort("grok-4.20-multi-agent-x"));
        assert!(!supports_reasoning_effort("grok-build"));
        assert!(!supports_reasoning_effort("grok-2"));
        assert!(!supports_reasoning_effort("gpt-5.2"));
    }

    #[test]
    fn chat_config_follows_the_model_allowlist() {
        assert!(!chat_config().supports_reasoning_effort);
        assert!(chat_config_with_reasoning_effort(true).supports_reasoning_effort);
    }

    #[test]
    fn grok_uses_the_system_role_not_developer() {
        assert_eq!(chat_config().system_role, "system");
    }

    #[test]
    fn the_two_lines_are_distinct_providers() {
        assert_eq!(chat_config().provider, ProviderId::Xai);
        assert_eq!(oauth_config().provider, ProviderId::XaiOAuth);
    }
}
