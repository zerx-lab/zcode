//! OpenAI Codex（ChatGPT 订阅）OAuth。
//!
//! 两条登录路径共用同一套 token 端点与凭据桶：
//!
//! - **浏览器**：authorization code + PKCE，回调端口**固定** 1455。
//! - **设备码**：本机拉不起浏览器时用；轮询拿到 `authorization_code` +
//!   `code_verifier` 后仍旧走同一个 code 交换，只是 `redirect_uri` 换成
//!   OpenAI 托管的那个。
//!
//! 账号身份不在 token 响应里，而是编码在 access token 的 JWT claim 中：
//! `chatgpt_account_id` 之后要作为 `chatgpt-account-id` 头随每次推理请求发出，
//! 取不到就调不了 `/codex/responses`，因此这里当作硬性字段处理。

use std::time::Duration;

use async_trait::async_trait;
use reqwest::{Client, StatusCode};

use crate::auth::credential::OAuthTokens;
use crate::auth::oauth::callback::{CallbackServer, DEFAULT_TIMEOUT, PortPolicy};
use crate::auth::oauth::pkce::{Pkce, random_state};
use crate::auth::oauth::{
    LoginPrompt, OAuthClient, TokenExchange, await_authorization_code, jwt, parse_tokens,
    post_form, post_json,
};
use crate::error::AuthError;
use crate::http;
use crate::types::ProviderId;

/// Codex CLI 的 OAuth client id。
pub const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";

/// 授权页地址。
pub const AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";

/// token 端点。
pub const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";

/// 固定回调端口。
pub const CALLBACK_PORT: u16 = 1455;

/// 固定回调路径。
pub const CALLBACK_PATH: &str = "/auth/callback";

/// 申请的 scope。
pub const SCOPES: &str =
    "openid profile email offline_access api.connectors.read api.connectors.invoke";

/// 客户端标识。
///
/// 授权 URL 的 `originator` 参数与推理请求的 `originator` 头必须是同一个值，且
/// 必须是 Codex 后端认识的值——换成随意字符串会被拒。
pub const ORIGINATOR: &str = "pi";

/// 设备码申请端点。
pub const DEVICE_USERCODE_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/usercode";

/// 设备码轮询端点。
pub const DEVICE_TOKEN_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/token";

/// 设备码流程交换 token 时使用的 `redirect_uri`。
pub const DEVICE_REDIRECT_URI: &str = "https://auth.openai.com/deviceauth/callback";

/// 让用户输入设备码的页面。
pub const DEVICE_AUTH_URL: &str = "https://auth.openai.com/codex/device";

/// 存放账号 id / 套餐的 JWT claim 命名空间。
const AUTH_CLAIM: &str = "https://api.openai.com/auth";

/// 存放邮箱的 JWT claim 命名空间。
const PROFILE_CLAIM: &str = "https://api.openai.com/profile";

/// 轮询间隔在服务端建议值上再加的安全余量。
const DEVICE_POLL_MARGIN: Duration = Duration::from_secs(3);

/// 服务端未给出 `interval` 时的默认轮询间隔。
const DEVICE_DEFAULT_INTERVAL: Duration = Duration::from_secs(5);

/// 轮询上限，超过即放弃。
const DEVICE_MAX_POLLS: u32 = 120;

/// 登录方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CodexLoginMode {
    /// 起本地回调服务器 + 浏览器跳转。
    #[default]
    Browser,
    /// 设备码轮询，适用于无浏览器 / 远程环境。
    DeviceCode,
}

/// OpenAI Codex OAuth 客户端。
#[derive(Debug, Clone)]
pub struct OpenAiCodexOAuth {
    client: Client,
    mode: CodexLoginMode,
    authorize_url: String,
    token_url: String,
    device_usercode_url: String,
    device_token_url: String,
    /// 轮询间隔在服务端建议值上追加的余量；测试里调零以免真的睡过去。
    poll_margin: Duration,
}

impl OpenAiCodexOAuth {
    /// 用共享 HTTP 客户端构造（浏览器模式）。
    pub fn new() -> Result<Self, AuthError> {
        Ok(Self::with_client(http::shared_client_for_auth()?))
    }

    /// 用指定 HTTP 客户端构造（浏览器模式）。
    #[must_use]
    pub fn with_client(client: Client) -> Self {
        Self {
            client,
            mode: CodexLoginMode::Browser,
            authorize_url: AUTHORIZE_URL.to_owned(),
            token_url: TOKEN_URL.to_owned(),
            device_usercode_url: DEVICE_USERCODE_URL.to_owned(),
            device_token_url: DEVICE_TOKEN_URL.to_owned(),
            poll_margin: DEVICE_POLL_MARGIN,
        }
    }

    /// 切换登录方式。
    #[must_use]
    pub fn with_mode(mut self, mode: CodexLoginMode) -> Self {
        self.mode = mode;
        self
    }

    /// 拼出授权 URL。
    #[must_use]
    pub fn authorization_url(&self, pkce: &Pkce, state: &str, redirect_uri: &str) -> String {
        let mut serializer = url::form_urlencoded::Serializer::new(String::new());
        serializer
            .append_pair("response_type", "code")
            .append_pair("client_id", CLIENT_ID)
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("scope", SCOPES)
            .append_pair("code_challenge", &pkce.challenge)
            .append_pair("code_challenge_method", pkce.method())
            .append_pair("state", state)
            .append_pair("id_token_add_organizations", "true")
            .append_pair("codex_cli_simplified_flow", "true")
            .append_pair("originator", ORIGINATOR);
        format!("{}?{}", self.authorize_url, serializer.finish())
    }

    async fn exchange_code(
        &self,
        code: &str,
        verifier: &str,
        redirect_uri: &str,
    ) -> Result<OAuthTokens, AuthError> {
        let exchange = post_form(
            &self.client,
            &self.token_url,
            &[
                ("grant_type", "authorization_code"),
                ("client_id", CLIENT_ID),
                ("code", code),
                ("code_verifier", verifier),
                ("redirect_uri", redirect_uri),
            ],
        )
        .await?;
        finish(exchange, None)
    }

    async fn login_browser(&self, prompt: &dyn LoginPrompt) -> Result<OAuthTokens, AuthError> {
        let pkce = Pkce::generate()?;
        let state = random_state()?;
        let server = CallbackServer::bind(
            ProviderId::OpenAiCodex,
            CALLBACK_PORT,
            CALLBACK_PATH,
            PortPolicy::Fixed,
            state,
        )
        .await?;
        let redirect_uri = server.redirect_uri().to_owned();

        prompt.authorization_url(
            ProviderId::OpenAiCodex,
            &self.authorization_url(&pkce, server.state(), &redirect_uri),
        );

        let granted =
            await_authorization_code(ProviderId::OpenAiCodex, server, prompt, DEFAULT_TIMEOUT)
                .await?;
        self.exchange_code(&granted.code, &pkce.verifier, &redirect_uri)
            .await
    }

    async fn login_device(&self, prompt: &dyn LoginPrompt) -> Result<OAuthTokens, AuthError> {
        let missing = |field: &str| AuthError::Protocol {
            provider: ProviderId::OpenAiCodex,
            detail: format!("设备码响应缺少 `{field}` 字段"),
        };

        let start = post_json(
            &self.client,
            &self.device_usercode_url,
            &serde_json::json!({ "client_id": CLIENT_ID }),
            &[],
        )
        .await?;
        if !start.status.is_success() {
            return Err(start.into_error(ProviderId::OpenAiCodex, "设备码申请"));
        }
        let device_auth_id = start
            .string("device_auth_id")
            .ok_or_else(|| missing("device_auth_id"))?;
        let user_code = start
            .string("user_code")
            .ok_or_else(|| missing("user_code"))?;
        let interval = start
            .body
            .get("interval")
            .and_then(serde_json::Value::as_u64)
            .map_or(DEVICE_DEFAULT_INTERVAL, Duration::from_secs)
            .saturating_add(self.poll_margin);

        prompt.device_code(ProviderId::OpenAiCodex, DEVICE_AUTH_URL, &user_code);

        for _attempt in 0..DEVICE_MAX_POLLS {
            tokio::time::sleep(interval).await;
            let poll = post_json(
                &self.client,
                &self.device_token_url,
                &serde_json::json!({ "device_auth_id": device_auth_id, "user_code": user_code }),
                &[],
            )
            .await?;
            // 403/404 表示用户还没在浏览器里确认，继续等。
            if matches!(poll.status, StatusCode::FORBIDDEN | StatusCode::NOT_FOUND) {
                continue;
            }
            if !poll.status.is_success() {
                return Err(poll.into_error(ProviderId::OpenAiCodex, "设备码轮询"));
            }
            let code = poll
                .string("authorization_code")
                .ok_or_else(|| missing("authorization_code"))?;
            let verifier = poll
                .string("code_verifier")
                .ok_or_else(|| missing("code_verifier"))?;
            return self
                .exchange_code(&code, &verifier, DEVICE_REDIRECT_URI)
                .await;
        }
        Err(AuthError::Timeout {
            provider: ProviderId::OpenAiCodex,
        })
    }
}

#[async_trait]
impl OAuthClient for OpenAiCodexOAuth {
    fn provider(&self) -> ProviderId {
        ProviderId::OpenAiCodex
    }

    async fn login(&self, prompt: &dyn LoginPrompt) -> Result<OAuthTokens, AuthError> {
        match self.mode {
            CodexLoginMode::Browser => self.login_browser(prompt).await,
            CodexLoginMode::DeviceCode => self.login_device(prompt).await,
        }
    }

    async fn refresh(&self, refresh_token: &str) -> Result<OAuthTokens, AuthError> {
        let exchange = post_form(
            &self.client,
            &self.token_url,
            &[
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh_token),
                ("client_id", CLIENT_ID),
            ],
        )
        .await?;
        finish(exchange, Some(refresh_token))
    }
}

/// 校验响应并从 JWT 里补齐账号身份。
fn finish(
    exchange: TokenExchange,
    fallback_refresh: Option<&str>,
) -> Result<OAuthTokens, AuthError> {
    if !exchange.status.is_success() {
        let operation = if fallback_refresh.is_some() {
            "刷新"
        } else {
            "授权码交换"
        };
        return Err(exchange.into_error(ProviderId::OpenAiCodex, operation));
    }
    let mut tokens = parse_tokens(
        ProviderId::OpenAiCodex,
        &exchange,
        fallback_refresh,
        Duration::ZERO,
    )?;
    let identity = read_identity(&tokens.access, exchange.string("id_token").as_deref());
    let account_id = identity.account_id.ok_or_else(|| AuthError::Protocol {
        provider: ProviderId::OpenAiCodex,
        detail: "access token 的 JWT 里没有 chatgpt_account_id，无法调用 Codex 后端".to_owned(),
    })?;
    tokens.account_id = Some(account_id);
    tokens.email = identity.email;
    tokens.plan = identity.plan;
    Ok(tokens)
}

#[derive(Debug, Default, PartialEq, Eq)]
struct Identity {
    account_id: Option<String>,
    email: Option<String>,
    plan: Option<String>,
}

/// 从 access token（必要时回落 `id_token`）读出账号身份。
fn read_identity(access_token: &str, id_token: Option<&str>) -> Identity {
    let access = jwt::decode_payload(access_token);
    let id = id_token.and_then(jwt::decode_payload);

    let account_id = access
        .as_ref()
        .and_then(|payload| jwt::nested_string_claim(payload, AUTH_CLAIM, "chatgpt_account_id"));
    let email = access
        .as_ref()
        .and_then(|payload| jwt::nested_string_claim(payload, PROFILE_CLAIM, "email"))
        .map(|email| email.trim().to_lowercase())
        .filter(|email| !email.is_empty());
    // 套餐优先取 access token；OpenAI 在部分流程里只把它放进 id_token。
    let plan = access
        .as_ref()
        .and_then(|payload| jwt::nested_string_claim(payload, AUTH_CLAIM, "chatgpt_plan_type"))
        .or_else(|| {
            id.as_ref().and_then(|payload| {
                jwt::nested_string_claim(payload, AUTH_CLAIM, "chatgpt_plan_type")
            })
        })
        .map(|plan| plan.trim().to_lowercase())
        .filter(|plan| !plan.is_empty());

    Identity {
        account_id,
        email,
        plan,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;

    fn token(payload: &serde_json::Value) -> String {
        let body = URL_SAFE_NO_PAD.encode(serde_json::to_vec(payload).expect("序列化"));
        format!("h.{body}.s")
    }

    fn access_token(account: &str, plan: Option<&str>, email: Option<&str>) -> String {
        let mut auth = serde_json::Map::new();
        drop(auth.insert("chatgpt_account_id".to_owned(), account.into()));
        if let Some(plan) = plan {
            drop(auth.insert("chatgpt_plan_type".to_owned(), plan.into()));
        }
        let mut payload = serde_json::Map::new();
        drop(payload.insert(AUTH_CLAIM.to_owned(), auth.into()));
        if let Some(email) = email {
            drop(payload.insert(
                PROFILE_CLAIM.to_owned(),
                serde_json::json!({ "email": email }),
            ));
        }
        token(&serde_json::Value::Object(payload))
    }

    fn codex_at(server: &mockito::ServerGuard) -> OpenAiCodexOAuth {
        let mut oauth =
            OpenAiCodexOAuth::with_client(http::shared_client_for_auth().expect("HTTP 客户端"));
        oauth.token_url = format!("{}/oauth/token", server.url());
        oauth.device_usercode_url = format!("{}/deviceauth/usercode", server.url());
        oauth.device_token_url = format!("{}/deviceauth/token", server.url());
        // 真实节奏是「服务端间隔 + 3s 余量」；测试只验证轮询分支，不验证节奏。
        oauth.poll_margin = Duration::ZERO;
        oauth
    }

    #[test]
    fn authorization_url_carries_the_codex_specific_parameters() {
        let oauth =
            OpenAiCodexOAuth::with_client(http::shared_client_for_auth().expect("HTTP 客户端"));
        let pkce = Pkce::from_verifier("v".to_owned());
        let url = oauth.authorization_url(&pkce, "st4te", "http://localhost:1455/auth/callback");

        let parsed = url::Url::parse(&url).expect("URL 合法");
        assert_eq!(parsed.host_str(), Some("auth.openai.com"));
        let params: std::collections::HashMap<_, _> = parsed.query_pairs().into_owned().collect();
        assert_eq!(params.get("client_id").map(String::as_str), Some(CLIENT_ID));
        assert_eq!(params.get("scope").map(String::as_str), Some(SCOPES));
        assert_eq!(
            params.get("id_token_add_organizations").map(String::as_str),
            Some("true")
        );
        assert_eq!(
            params.get("codex_cli_simplified_flow").map(String::as_str),
            Some("true")
        );
        assert_eq!(
            params.get("originator").map(String::as_str),
            Some(ORIGINATOR)
        );
        assert_eq!(
            params.get("code_challenge_method").map(String::as_str),
            Some("S256")
        );
        assert_eq!(
            params.get("redirect_uri").map(String::as_str),
            Some("http://localhost:1455/auth/callback")
        );
    }

    #[test]
    fn identity_prefers_access_token_and_normalizes_case() {
        let identity = read_identity(
            &access_token("acct-1", Some("  PRO "), Some(" Me@Example.COM ")),
            None,
        );
        assert_eq!(identity.account_id.as_deref(), Some("acct-1"));
        assert_eq!(identity.plan.as_deref(), Some("pro"));
        assert_eq!(identity.email.as_deref(), Some("me@example.com"));
    }

    #[test]
    fn plan_falls_back_to_the_id_token() {
        let id = token(&serde_json::json!({ AUTH_CLAIM: { "chatgpt_plan_type": "team" } }));
        let identity = read_identity(&access_token("acct-1", None, None), Some(&id));
        assert_eq!(identity.plan.as_deref(), Some("team"));
    }

    #[test]
    fn missing_account_id_is_a_protocol_error_not_a_silent_none() {
        let exchange = TokenExchange {
            status: StatusCode::OK,
            body: serde_json::json!({
                "access_token": token(&serde_json::json!({})),
                "refresh_token": "rt",
                "expires_in": 60
            }),
        };
        assert!(matches!(
            finish(exchange, None),
            Err(AuthError::Protocol { .. })
        ));
    }

    #[tokio::test]
    async fn code_exchange_posts_a_form_body() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/oauth/token")
            .match_header("content-type", "application/x-www-form-urlencoded")
            .match_body(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded("grant_type".into(), "authorization_code".into()),
                mockito::Matcher::UrlEncoded("client_id".into(), CLIENT_ID.into()),
                mockito::Matcher::UrlEncoded("code".into(), "the-code".into()),
                mockito::Matcher::UrlEncoded("code_verifier".into(), "the-verifier".into()),
            ]))
            .with_status(200)
            .with_body(
                serde_json::json!({
                    "access_token": access_token("acct-9", Some("pro"), None),
                    "refresh_token": "rt", "expires_in": 3600
                })
                .to_string(),
            )
            .create_async()
            .await;

        let tokens = codex_at(&server)
            .exchange_code(
                "the-code",
                "the-verifier",
                "http://localhost:1455/auth/callback",
            )
            .await
            .expect("交换成功");

        mock.assert_async().await;
        assert_eq!(tokens.account_id.as_deref(), Some("acct-9"));
        assert_eq!(tokens.plan.as_deref(), Some("pro"));
    }

    #[tokio::test]
    async fn refresh_keeps_the_old_refresh_token_when_none_is_returned() {
        let mut server = mockito::Server::new_async().await;
        drop(
            server
                .mock("POST", "/oauth/token")
                .match_body(mockito::Matcher::UrlEncoded(
                    "grant_type".into(),
                    "refresh_token".into(),
                ))
                .with_status(200)
                .with_body(
                    serde_json::json!({
                        "access_token": access_token("acct-9", None, None),
                        "expires_in": 60
                    })
                    .to_string(),
                )
                .create_async()
                .await,
        );

        let tokens = codex_at(&server).refresh("old-rt").await.expect("刷新成功");
        assert_eq!(tokens.refresh, "old-rt");
    }

    #[derive(Debug, Default)]
    struct RecordingPrompt {
        device: std::sync::Mutex<Vec<(String, String)>>,
    }

    #[async_trait]
    impl LoginPrompt for RecordingPrompt {
        fn authorization_url(&self, _provider: ProviderId, _url: &str) {}

        fn device_code(&self, _provider: ProviderId, verification_uri: &str, user_code: &str) {
            if let Ok(mut seen) = self.device.lock() {
                seen.push((verification_uri.to_owned(), user_code.to_owned()));
            }
        }
    }

    #[tokio::test]
    async fn device_flow_polls_past_pending_and_then_exchanges() {
        let mut server = mockito::Server::new_async().await;
        drop(
            server
                .mock("POST", "/deviceauth/usercode")
                .with_status(200)
                .with_body(
                    serde_json::json!({
                        "device_auth_id": "dev-1", "user_code": "ABCD-EFGH", "interval": 0
                    })
                    .to_string(),
                )
                .create_async()
                .await,
        );
        let pending = server
            .mock("POST", "/deviceauth/token")
            .with_status(403)
            .with_body("{}")
            .expect(2)
            .create_async()
            .await;
        let granted = server
            .mock("POST", "/deviceauth/token")
            .with_status(200)
            .with_body(
                serde_json::json!({
                    "authorization_code": "dev-code", "code_verifier": "dev-verifier"
                })
                .to_string(),
            )
            .create_async()
            .await;
        let exchanged = server
            .mock("POST", "/oauth/token")
            .match_body(mockito::Matcher::UrlEncoded(
                "redirect_uri".into(),
                DEVICE_REDIRECT_URI.into(),
            ))
            .with_status(200)
            .with_body(
                serde_json::json!({
                    "access_token": access_token("acct-dev", None, None),
                    "refresh_token": "rt", "expires_in": 60
                })
                .to_string(),
            )
            .create_async()
            .await;

        let prompt = RecordingPrompt::default();
        let tokens = codex_at(&server)
            .with_mode(CodexLoginMode::DeviceCode)
            .login(&prompt)
            .await
            .expect("设备码登录成功");

        pending.assert_async().await;
        granted.assert_async().await;
        exchanged.assert_async().await;
        assert_eq!(tokens.account_id.as_deref(), Some("acct-dev"));
        assert_eq!(
            prompt
                .device
                .lock()
                .ok()
                .and_then(|seen| seen.first().cloned()),
            Some((DEVICE_AUTH_URL.to_owned(), "ABCD-EFGH".to_owned()))
        );
    }
}
