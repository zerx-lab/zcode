//! OpenAI Codex（ChatGPT 订阅）适配器。
//!
//! 复用 [`ResponsesProvider`]，只换配置——线格式与平台 Responses 一致，不同的是
//! 落点和一组硬性约束：
//!
//! - base URL 是 `https://chatgpt.com/backend-api`，路径 `/codex/responses`，
//!   **不是** `api.openai.com`；
//! - 必带 `chatgpt-account-id`（来自 access token 的 JWT claim）、`originator`、
//!   `version`、`OpenAI-Beta: responses=experimental`；
//! - 请求体禁止 `max_output_tokens` / `temperature` / `top_p` —— 后端会直接
//!   400 `Unsupported parameter`；
//! - `store` 恒为 `false`，`include` 必含 `reasoning.encrypted_content`。

use std::sync::Arc;

use crate::auth::AuthStore;
use crate::auth::oauth::openai_codex::ORIGINATOR;
use crate::error::AiError;
use crate::provider::openai_responses::{CodexFlavor, Flavor, ResponsesConfig, ResponsesProvider};
use crate::types::ProviderId;

/// ChatGPT 后端根地址。
pub const BASE_URL: &str = "https://chatgpt.com/backend-api";

/// Codex Responses 路径。
pub const RESPONSES_PATH: &str = "/codex/responses";

/// `version` 头声明的客户端版本。
pub const CLIENT_VERSION: &str = "0.144.1";

/// `OpenAI-Beta` 头取值。
pub const BETA: &str = "responses=experimental";

/// Codex 线路的配置。
#[must_use]
pub fn config() -> ResponsesConfig {
    ResponsesConfig {
        provider: ProviderId::OpenAiCodex,
        base_url: BASE_URL.to_owned(),
        path: RESPONSES_PATH,
        flavor: Flavor::Codex(CodexFlavor {
            originator: ORIGINATOR,
            client_version: CLIENT_VERSION,
            beta: BETA,
        }),
        store: false,
        include_encrypted_reasoning: true,
        reasoning_summary: Some("detailed"),
        // 这两项会让 Codex 后端返回 400 Unsupported parameter。
        send_max_output_tokens: false,
        send_sampling_params: false,
        cache_session_header: None,
    }
}

/// 构造 Codex 适配器。
pub fn provider(auth: Arc<AuthStore>) -> Result<ResponsesProvider, AiError> {
    ResponsesProvider::new(auth, config())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_targets_the_chatgpt_backend_not_the_platform_api() {
        let config = config();
        assert_eq!(config.base_url, "https://chatgpt.com/backend-api");
        assert_eq!(config.path, "/codex/responses");
        assert_eq!(config.provider, ProviderId::OpenAiCodex);
    }

    #[test]
    fn codex_suppresses_the_parameters_its_backend_rejects() {
        let config = config();
        assert!(!config.send_max_output_tokens);
        assert!(!config.send_sampling_params);
    }

    #[test]
    fn codex_stays_stateless_and_keeps_encrypted_reasoning() {
        let config = config();
        assert!(!config.store);
        assert!(config.include_encrypted_reasoning);
    }

    #[test]
    fn codex_identity_headers_match_the_oauth_originator() {
        match config().flavor {
            Flavor::Codex(codex) => {
                // 授权 URL 与请求头必须是同一个 originator，否则后端拒绝。
                assert_eq!(codex.originator, ORIGINATOR);
                assert_eq!(codex.beta, BETA);
                assert_eq!(codex.client_version, CLIENT_VERSION);
            }
            Flavor::Standard => panic!("Codex 配置必须是 Codex 风味"),
        }
    }
}
