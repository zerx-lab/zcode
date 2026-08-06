//! Anthropic（Claude Pro / Max）OAuth。
//!
//! authorization code + PKCE，回调端口**固定** 54545——`redirect_uri` 必须与在
//! Anthropic 侧注册的完全一致，端口被占用时只能报错，不能换端口。
//!
//! token 端点收 JSON body（不是表单），且刷新请求要额外带 `anthropic-beta` 与
//! Claude Code 的 User-Agent，这两点与 OpenAI / xAI 都不同。

use std::time::Duration;

use async_trait::async_trait;
use reqwest::Client;

use crate::auth::credential::OAuthTokens;
use crate::auth::oauth::callback::{CallbackServer, DEFAULT_TIMEOUT, PortPolicy};
use crate::auth::oauth::pkce::{Pkce, random_state};
use crate::auth::oauth::{
    LoginPrompt, OAuthClient, TokenExchange, await_authorization_code, parse_tokens, post_json,
};
use crate::error::AuthError;
use crate::http;
use crate::types::ProviderId;

/// Claude Code 的 OAuth client id。
pub const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";

/// 授权页地址。
///
/// 必须用 `claude.ai`：`platform.claude.com` 只签发 console token（仅
/// `org:create_api_key`），拿不到 `user:inference`，无法直接推理。
pub const AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";

/// token 端点。
pub const TOKEN_URL: &str = "https://api.anthropic.com/v1/oauth/token";

/// 申请的 scope。
pub const SCOPES: &str = "org:create_api_key user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload";

/// 固定回调端口。
pub const CALLBACK_PORT: u16 = 54545;

/// 固定回调路径。
pub const CALLBACK_PATH: &str = "/callback";

/// 刷新请求要带的 beta 标记。
const REFRESH_BETA: &str = "oauth-2025-04-20";

/// 刷新请求要带的 User-Agent，与 Claude Code 一致。
const REFRESH_USER_AGENT: &str = "anthropic-sdk-typescript/0.94.0 userOAuthProvider";

/// 客户端侧额外扣掉的过期提前量。
const EXPIRY_SKEW: Duration = Duration::from_mins(5);

/// Anthropic OAuth 客户端。
#[derive(Debug, Clone)]
pub struct AnthropicOAuth {
    client: Client,
    authorize_url: String,
    token_url: String,
}

impl AnthropicOAuth {
    /// 用共享 HTTP 客户端构造。
    pub fn new() -> Result<Self, AuthError> {
        Ok(Self::with_client(http::shared_client_for_auth()?))
    }

    /// 用指定 HTTP 客户端构造。
    #[must_use]
    pub fn with_client(client: Client) -> Self {
        Self {
            client,
            authorize_url: AUTHORIZE_URL.to_owned(),
            token_url: TOKEN_URL.to_owned(),
        }
    }

    /// 改写端点，仅供测试指向本地 mock 服务器。
    #[cfg(test)]
    fn with_endpoints(mut self, authorize_url: String, token_url: String) -> Self {
        self.authorize_url = authorize_url;
        self.token_url = token_url;
        self
    }

    /// 拼出授权 URL。
    ///
    /// `code=true` 是 Claude Code 特有参数：让授权页在无法回环时把 code 直接
    /// 展示给用户，用于粘贴兜底。
    #[must_use]
    pub fn authorization_url(&self, pkce: &Pkce, state: &str, redirect_uri: &str) -> String {
        let query = serde_urlencoded_pairs(&[
            ("code", "true"),
            ("client_id", CLIENT_ID),
            ("response_type", "code"),
            ("redirect_uri", redirect_uri),
            ("scope", SCOPES),
            ("code_challenge", &pkce.challenge),
            ("code_challenge_method", pkce.method()),
            ("state", state),
        ]);
        format!("{}?{query}", self.authorize_url)
    }

    async fn exchange_code(
        &self,
        code: &str,
        state: &str,
        redirect_uri: &str,
        verifier: &str,
    ) -> Result<OAuthTokens, AuthError> {
        // 粘贴兜底时用户可能带上 `code#state` 片段，拆开后 fragment 才是真 state。
        let (code, state) = split_code_fragment(code, state);
        let exchange = post_json(
            &self.client,
            &self.token_url,
            &serde_json::json!({
                "grant_type": "authorization_code",
                "client_id": CLIENT_ID,
                "code": code,
                "state": state,
                "redirect_uri": redirect_uri,
                "code_verifier": verifier,
            }),
            &[],
        )
        .await?;
        finish(&exchange, None)
    }
}

#[async_trait]
impl OAuthClient for AnthropicOAuth {
    fn provider(&self) -> ProviderId {
        ProviderId::Anthropic
    }

    async fn login(&self, prompt: &dyn LoginPrompt) -> Result<OAuthTokens, AuthError> {
        let pkce = Pkce::generate()?;
        let state = random_state()?;
        let server = CallbackServer::bind(
            ProviderId::Anthropic,
            CALLBACK_PORT,
            CALLBACK_PATH,
            PortPolicy::Fixed,
            state.clone(),
        )
        .await?;
        let redirect_uri = server.redirect_uri().to_owned();

        prompt.authorization_url(
            ProviderId::Anthropic,
            &self.authorization_url(&pkce, &state, &redirect_uri),
        );

        let granted =
            await_authorization_code(ProviderId::Anthropic, server, prompt, DEFAULT_TIMEOUT)
                .await?;
        self.exchange_code(&granted.code, &granted.state, &redirect_uri, &pkce.verifier)
            .await
    }

    async fn refresh(&self, refresh_token: &str) -> Result<OAuthTokens, AuthError> {
        let exchange = post_json(
            &self.client,
            &self.token_url,
            &serde_json::json!({
                "grant_type": "refresh_token",
                "client_id": CLIENT_ID,
                "refresh_token": refresh_token,
            }),
            &[
                ("anthropic-beta", REFRESH_BETA),
                ("user-agent", REFRESH_USER_AGENT),
            ],
        )
        .await?;
        finish(&exchange, Some(refresh_token))
    }
}

/// 校验响应并补上身份字段。
fn finish(
    exchange: &TokenExchange,
    fallback_refresh: Option<&str>,
) -> Result<OAuthTokens, AuthError> {
    if !exchange.status.is_success() {
        let operation = if fallback_refresh.is_some() {
            "刷新"
        } else {
            "授权码交换"
        };
        return Err(TokenExchange {
            status: exchange.status,
            body: exchange.body.clone(),
        }
        .into_error(ProviderId::Anthropic, operation));
    }
    let mut tokens = parse_tokens(
        ProviderId::Anthropic,
        exchange,
        fallback_refresh,
        EXPIRY_SKEW,
    )?;
    // token 响应自带 account / organization，省掉一次 profile 往返。
    tokens.account_id = nested_str(&exchange.body, "account", "uuid");
    tokens.email = nested_str(&exchange.body, "account", "email_address");
    tokens.plan = nested_str(&exchange.body, "organization", "name");
    Ok(tokens)
}

fn nested_str(body: &serde_json::Value, object: &str, field: &str) -> Option<String> {
    body.get(object)?
        .get(field)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// 拆分粘贴兜底里的 `code#state`。
fn split_code_fragment<'a>(code: &'a str, state: &'a str) -> (&'a str, &'a str) {
    match code.split_once('#') {
        Some((code, fragment)) if !fragment.is_empty() => (code, fragment),
        Some((code, _empty)) => (code, state),
        None => (code, state),
    }
}

/// 拼 `application/x-www-form-urlencoded` 查询串。
fn serde_urlencoded_pairs(pairs: &[(&str, &str)]) -> String {
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    for (key, value) in pairs {
        serializer.append_pair(key, value);
    }
    serializer.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorization_url_carries_every_required_parameter() {
        let oauth =
            AnthropicOAuth::with_client(http::shared_client_for_auth().expect("HTTP 客户端"));
        let pkce = Pkce::from_verifier("verifier".to_owned());
        let url = oauth.authorization_url(&pkce, "st4te", "http://localhost:54545/callback");

        let parsed = url::Url::parse(&url).expect("URL 合法");
        assert_eq!(parsed.host_str(), Some("claude.ai"));
        assert_eq!(parsed.path(), "/oauth/authorize");
        let params: std::collections::HashMap<_, _> = parsed.query_pairs().into_owned().collect();
        assert_eq!(params.get("client_id").map(String::as_str), Some(CLIENT_ID));
        assert_eq!(
            params.get("response_type").map(String::as_str),
            Some("code")
        );
        assert_eq!(params.get("code").map(String::as_str), Some("true"));
        assert_eq!(
            params.get("code_challenge_method").map(String::as_str),
            Some("S256")
        );
        assert_eq!(
            params.get("code_challenge").map(String::as_str),
            Some(pkce.challenge.as_str())
        );
        assert_eq!(params.get("state").map(String::as_str), Some("st4te"));
        assert_eq!(params.get("scope").map(String::as_str), Some(SCOPES));
        assert_eq!(
            params.get("redirect_uri").map(String::as_str),
            Some("http://localhost:54545/callback")
        );
    }

    #[test]
    fn pasted_code_with_fragment_overrides_the_state() {
        assert_eq!(split_code_fragment("abc#frag", "orig"), ("abc", "frag"));
        assert_eq!(split_code_fragment("abc#", "orig"), ("abc", "orig"));
        assert_eq!(split_code_fragment("abc", "orig"), ("abc", "orig"));
    }

    #[tokio::test]
    async fn code_exchange_posts_json_and_lifts_account_identity() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/oauth/token")
            .match_header("content-type", "application/json")
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "grant_type": "authorization_code",
                "client_id": CLIENT_ID,
                "code": "the-code",
                "code_verifier": "the-verifier",
            })))
            .with_status(200)
            .with_body(
                serde_json::json!({
                    "access_token": "at", "refresh_token": "rt", "expires_in": 3600,
                    "account": { "uuid": "acct-1", "email_address": "me@example.com" },
                    "organization": { "uuid": "org-1", "name": "Max" }
                })
                .to_string(),
            )
            .create_async()
            .await;

        let oauth =
            AnthropicOAuth::with_client(http::shared_client_for_auth().expect("HTTP 客户端"))
                .with_endpoints(
                    AUTHORIZE_URL.to_owned(),
                    format!("{}/v1/oauth/token", server.url()),
                );
        let tokens = oauth
            .exchange_code(
                "the-code",
                "st4te",
                "http://localhost:54545/callback",
                "the-verifier",
            )
            .await
            .expect("交换成功");

        mock.assert_async().await;
        assert_eq!(tokens.access, "at");
        assert_eq!(tokens.refresh, "rt");
        assert_eq!(tokens.account_id.as_deref(), Some("acct-1"));
        assert_eq!(tokens.email.as_deref(), Some("me@example.com"));
        assert_eq!(tokens.plan.as_deref(), Some("Max"));
    }

    #[tokio::test]
    async fn refresh_sends_the_claude_code_beta_and_user_agent() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/v1/oauth/token")
            .match_header("anthropic-beta", REFRESH_BETA)
            .match_header("user-agent", REFRESH_USER_AGENT)
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "grant_type": "refresh_token",
                "refresh_token": "old-rt",
            })))
            .with_status(200)
            .with_body(serde_json::json!({ "access_token": "at2", "expires_in": 60 }).to_string())
            .create_async()
            .await;

        let oauth =
            AnthropicOAuth::with_client(http::shared_client_for_auth().expect("HTTP 客户端"))
                .with_endpoints(
                    AUTHORIZE_URL.to_owned(),
                    format!("{}/v1/oauth/token", server.url()),
                );
        let tokens = oauth.refresh("old-rt").await.expect("刷新成功");

        mock.assert_async().await;
        assert_eq!(tokens.access, "at2");
        // 服务端没回 refresh_token 时必须沿用旧的，否则凭据下次就废了。
        assert_eq!(tokens.refresh, "old-rt");
    }

    #[tokio::test]
    async fn invalid_grant_surfaces_as_denied() {
        let mut server = mockito::Server::new_async().await;
        drop(
            server
                .mock("POST", "/v1/oauth/token")
                .with_status(400)
                .with_body(
                    serde_json::json!({
                        "error": "invalid_grant", "error_description": "expired"
                    })
                    .to_string(),
                )
                .create_async()
                .await,
        );

        let oauth =
            AnthropicOAuth::with_client(http::shared_client_for_auth().expect("HTTP 客户端"))
                .with_endpoints(
                    AUTHORIZE_URL.to_owned(),
                    format!("{}/v1/oauth/token", server.url()),
                );
        let err = oauth.refresh("dead").await.expect_err("应当失败");
        assert!(matches!(err, AuthError::Denied { error, .. } if error == "invalid_grant"));
    }
}
