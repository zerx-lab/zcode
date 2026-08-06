//! xAI（SuperGrok）OAuth。
//!
//! 与另外两家都不同：RFC 8628 **设备码**流程，没有 PKCE、没有 `redirect_uri`、
//! 不起本地回调服务器。token 端点也不是写死的常量，而是每次从 OIDC discovery
//! 文档里读 `token_endpoint`——因此必须校验它仍指向 `*.x.ai`，否则一个被篡改的
//! discovery 响应就能把 refresh token 送到别处。

use std::time::Duration;

use async_trait::async_trait;
use reqwest::Client;

use crate::auth::credential::OAuthTokens;
use crate::auth::oauth::{LoginPrompt, OAuthClient, jwt, parse_tokens, post_form};
use crate::error::AuthError;
use crate::http;
use crate::types::ProviderId;

/// OIDC discovery 文档地址。
pub const DISCOVERY_URL: &str = "https://auth.x.ai/.well-known/openid-configuration";

/// 设备码申请端点。
pub const DEVICE_CODE_URL: &str = "https://auth.x.ai/oauth2/device/code";

/// 用户信息端点。
pub const USERINFO_URL: &str = "https://auth.x.ai/oauth2/userinfo";

/// Grok CLI 的 OAuth client id。
pub const CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";

/// 申请的 scope。
pub const SCOPES: &str = "openid profile email offline_access grok-cli:access api:access";

/// 设备码 grant 类型。
const DEVICE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";

/// 客户端侧额外扣掉的过期提前量。
const EXPIRY_SKEW: Duration = Duration::from_mins(5);

/// discovery / userinfo 请求超时。
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(15);

/// 服务端未给出 `interval` 时的默认轮询间隔。
const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(5);

/// `slow_down` 时每次追加的间隔。
const SLOW_DOWN_STEP: Duration = Duration::from_secs(5);

/// 轮询上限。
const MAX_POLLS: u32 = 180;

/// xAI OAuth 客户端。
#[derive(Debug, Clone)]
pub struct XaiOAuth {
    client: Client,
    discovery_url: String,
    device_code_url: String,
    userinfo_url: String,
    /// 允许 token / 验证地址落在哪个域下；测试指向 mock 时放开。
    enforce_x_ai_host: bool,
    /// 收到 `slow_down` 时每次追加的间隔；测试里调零以免真的睡过去。
    slow_down_step: Duration,
}

impl XaiOAuth {
    /// 用共享 HTTP 客户端构造。
    pub fn new() -> Result<Self, AuthError> {
        Ok(Self::with_client(http::shared_client_for_auth()?))
    }

    /// 用指定 HTTP 客户端构造。
    #[must_use]
    pub fn with_client(client: Client) -> Self {
        Self {
            client,
            discovery_url: DISCOVERY_URL.to_owned(),
            device_code_url: DEVICE_CODE_URL.to_owned(),
            userinfo_url: USERINFO_URL.to_owned(),
            enforce_x_ai_host: true,
            slow_down_step: SLOW_DOWN_STEP,
        }
    }

    /// 从 discovery 文档解析 `token_endpoint` 并校验其归属。
    async fn token_endpoint(&self) -> Result<String, AuthError> {
        let response = self
            .client
            .get(&self.discovery_url)
            .header(reqwest::header::ACCEPT, "application/json")
            .timeout(DISCOVERY_TIMEOUT)
            .send()
            .await?;
        let document: serde_json::Value = response.json().await?;
        let endpoint = document
            .get("token_endpoint")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| AuthError::Protocol {
                provider: ProviderId::XaiOAuth,
                detail: "OIDC discovery 文档里没有 token_endpoint".to_owned(),
            })?;
        self.ensure_trusted(endpoint)?;
        Ok(endpoint.to_owned())
    }

    /// discovery 文档是可被中间人改写的，token 端点必须仍在 xAI 名下。
    fn ensure_trusted(&self, endpoint: &str) -> Result<(), AuthError> {
        if !self.enforce_x_ai_host {
            return Ok(());
        }
        let reject = |detail: String| AuthError::Protocol {
            provider: ProviderId::XaiOAuth,
            detail,
        };
        let url = url::Url::parse(endpoint)
            .map_err(|err| reject(format!("端点 `{endpoint}` 不是合法 URL：{err}")))?;
        if url.scheme() != "https" {
            return Err(reject(format!("端点 `{endpoint}` 不是 https")));
        }
        let host = url
            .host_str()
            .ok_or_else(|| reject(format!("端点 `{endpoint}` 没有主机名")))?;
        if host == "x.ai" || host.ends_with(".x.ai") {
            Ok(())
        } else {
            Err(reject(format!(
                "端点 `{endpoint}` 不在 x.ai 域下，拒绝把令牌发过去"
            )))
        }
    }

    async fn request_device_code(&self) -> Result<DeviceAuthorization, AuthError> {
        let exchange = post_form(
            &self.client,
            &self.device_code_url,
            &[("client_id", CLIENT_ID), ("scope", SCOPES)],
        )
        .await?;
        if !exchange.status.is_success() {
            return Err(exchange.into_error(ProviderId::XaiOAuth, "设备码申请"));
        }
        let missing = |field: &str| AuthError::Protocol {
            provider: ProviderId::XaiOAuth,
            detail: format!("设备码响应缺少 `{field}` 字段"),
        };
        let device_code = exchange
            .string("device_code")
            .ok_or_else(|| missing("device_code"))?;
        let user_code = exchange
            .string("user_code")
            .ok_or_else(|| missing("user_code"))?;
        let verification_uri = exchange
            .string("verification_uri_complete")
            .or_else(|| exchange.string("verification_uri"))
            .ok_or_else(|| missing("verification_uri"))?;
        self.ensure_trusted(&verification_uri)?;
        let interval = exchange
            .body
            .get("interval")
            .and_then(serde_json::Value::as_u64)
            .map_or(DEFAULT_POLL_INTERVAL, Duration::from_secs);

        Ok(DeviceAuthorization {
            device_code,
            user_code,
            verification_uri,
            interval,
        })
    }

    /// 用 access token 拉 `sub` / `email`，失败不影响登录本身。
    async fn enrich_identity(&self, tokens: &mut OAuthTokens) {
        if let Some(payload) = jwt::decode_payload(&tokens.access) {
            tokens.account_id = jwt::string_claim(&payload, "sub");
        }
        let response = self
            .client
            .get(&self.userinfo_url)
            .bearer_auth(&tokens.access)
            .header(reqwest::header::ACCEPT, "application/json")
            .timeout(DISCOVERY_TIMEOUT)
            .send()
            .await;
        let Ok(response) = response else { return };
        if !response.status().is_success() {
            return;
        }
        let Ok(profile) = response.json::<serde_json::Value>().await else {
            return;
        };
        if let Some(sub) = profile.get("sub").and_then(serde_json::Value::as_str) {
            tokens.account_id = Some(sub.to_owned());
        }
        tokens.email = profile
            .get("email")
            .and_then(serde_json::Value::as_str)
            .map(|email| email.trim().to_lowercase())
            .filter(|email| !email.is_empty());
    }
}

/// 设备码申请的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
struct DeviceAuthorization {
    device_code: String,
    user_code: String,
    verification_uri: String,
    interval: Duration,
}

#[async_trait]
impl OAuthClient for XaiOAuth {
    fn provider(&self) -> ProviderId {
        ProviderId::XaiOAuth
    }

    async fn login(&self, prompt: &dyn LoginPrompt) -> Result<OAuthTokens, AuthError> {
        let token_endpoint = self.token_endpoint().await?;
        let device = self.request_device_code().await?;
        prompt.device_code(
            ProviderId::XaiOAuth,
            &device.verification_uri,
            &device.user_code,
        );

        let mut interval = device.interval;
        for _attempt in 0..MAX_POLLS {
            tokio::time::sleep(interval).await;
            let exchange = post_form(
                &self.client,
                &token_endpoint,
                &[
                    ("grant_type", DEVICE_GRANT),
                    ("client_id", CLIENT_ID),
                    ("device_code", &device.device_code),
                ],
            )
            .await?;

            if exchange.status.is_success() {
                let mut tokens = parse_tokens(ProviderId::XaiOAuth, &exchange, None, EXPIRY_SKEW)?;
                self.enrich_identity(&mut tokens).await;
                return Ok(tokens);
            }
            match exchange.oauth_error().as_deref() {
                // 用户还没在浏览器里确认，按原节奏继续等。
                Some("authorization_pending") => {}
                Some("slow_down") => interval = interval.saturating_add(self.slow_down_step),
                _ => return Err(exchange.into_error(ProviderId::XaiOAuth, "设备码轮询")),
            }
        }
        Err(AuthError::Timeout {
            provider: ProviderId::XaiOAuth,
        })
    }

    async fn refresh(&self, refresh_token: &str) -> Result<OAuthTokens, AuthError> {
        let token_endpoint = self.token_endpoint().await?;
        let exchange = post_form(
            &self.client,
            &token_endpoint,
            &[
                ("grant_type", "refresh_token"),
                ("client_id", CLIENT_ID),
                ("refresh_token", refresh_token),
            ],
        )
        .await?;
        if !exchange.status.is_success() {
            return Err(exchange.into_error(ProviderId::XaiOAuth, "刷新"));
        }
        // xAI 不一定轮换 refresh token，缺省时必须沿用旧的。
        let mut tokens = parse_tokens(
            ProviderId::XaiOAuth,
            &exchange,
            Some(refresh_token),
            EXPIRY_SKEW,
        )?;
        self.enrich_identity(&mut tokens).await;
        Ok(tokens)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn xai_at(server: &mockito::ServerGuard) -> XaiOAuth {
        let mut oauth = XaiOAuth::with_client(http::shared_client_for_auth().expect("HTTP 客户端"));
        oauth.discovery_url = format!("{}/.well-known/openid-configuration", server.url());
        oauth.device_code_url = format!("{}/oauth2/device/code", server.url());
        oauth.userinfo_url = format!("{}/oauth2/userinfo", server.url());
        oauth.enforce_x_ai_host = false;
        // 测试只验证 pending / slow_down 的分支走向，不验证退避节奏。
        oauth.slow_down_step = Duration::ZERO;
        oauth
    }

    #[test]
    fn only_x_ai_hosts_are_accepted_as_token_endpoints() {
        let oauth = XaiOAuth::with_client(http::shared_client_for_auth().expect("HTTP 客户端"));
        assert!(
            oauth
                .ensure_trusted("https://auth.x.ai/oauth2/token")
                .is_ok()
        );
        assert!(oauth.ensure_trusted("https://x.ai/token").is_ok());
        // 后缀伪装：`evilx.ai` 不是 `.x.ai` 的子域。
        assert!(oauth.ensure_trusted("https://evilx.ai/token").is_err());
        assert!(
            oauth
                .ensure_trusted("https://auth.x.ai.attacker.com/token")
                .is_err()
        );
        assert!(oauth.ensure_trusted("http://auth.x.ai/token").is_err());
        assert!(oauth.ensure_trusted("not a url").is_err());
    }

    #[tokio::test]
    async fn discovery_without_token_endpoint_is_a_protocol_error() {
        let mut server = mockito::Server::new_async().await;
        drop(
            server
                .mock("GET", "/.well-known/openid-configuration")
                .with_status(200)
                .with_body("{}")
                .create_async()
                .await,
        );
        let err = xai_at(&server)
            .token_endpoint()
            .await
            .expect_err("应当失败");
        assert!(matches!(err, AuthError::Protocol { .. }));
    }

    #[tokio::test]
    async fn device_flow_handles_pending_then_slow_down_then_success() {
        let mut server = mockito::Server::new_async().await;
        let token_url = format!("{}/oauth2/token", server.url());
        drop(
            server
                .mock("GET", "/.well-known/openid-configuration")
                .with_status(200)
                .with_body(serde_json::json!({ "token_endpoint": token_url }).to_string())
                .create_async()
                .await,
        );
        let device = server
            .mock("POST", "/oauth2/device/code")
            .match_body(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded("client_id".into(), CLIENT_ID.into()),
                mockito::Matcher::UrlEncoded("scope".into(), SCOPES.into()),
            ]))
            .with_status(200)
            .with_body(
                serde_json::json!({
                    "device_code": "dc", "user_code": "WXYZ",
                    "verification_uri_complete": "https://auth.x.ai/device?code=WXYZ",
                    "interval": 0
                })
                .to_string(),
            )
            .create_async()
            .await;
        let pending = server
            .mock("POST", "/oauth2/token")
            .with_status(400)
            .with_body(serde_json::json!({ "error": "authorization_pending" }).to_string())
            .create_async()
            .await;
        let slow = server
            .mock("POST", "/oauth2/token")
            .with_status(400)
            .with_body(serde_json::json!({ "error": "slow_down" }).to_string())
            .create_async()
            .await;
        let granted = server
            .mock("POST", "/oauth2/token")
            .match_body(mockito::Matcher::UrlEncoded(
                "grant_type".into(),
                DEVICE_GRANT.into(),
            ))
            .with_status(200)
            .with_body(
                serde_json::json!({
                    "access_token": "at", "refresh_token": "rt", "expires_in": 3600
                })
                .to_string(),
            )
            .create_async()
            .await;
        drop(
            server
                .mock("GET", "/oauth2/userinfo")
                .with_status(200)
                .with_body(serde_json::json!({ "sub": "user-1", "email": " Me@X.AI " }).to_string())
                .create_async()
                .await,
        );

        let prompt = RecordingPrompt::default();
        let tokens = xai_at(&server).login(&prompt).await.expect("登录成功");

        device.assert_async().await;
        pending.assert_async().await;
        slow.assert_async().await;
        granted.assert_async().await;
        assert_eq!(tokens.access, "at");
        assert_eq!(tokens.account_id.as_deref(), Some("user-1"));
        assert_eq!(tokens.email.as_deref(), Some("me@x.ai"));
        assert_eq!(
            prompt
                .device
                .lock()
                .ok()
                .and_then(|seen| seen.first().cloned()),
            Some((
                "https://auth.x.ai/device?code=WXYZ".to_owned(),
                "WXYZ".to_owned()
            ))
        );
    }

    #[tokio::test]
    async fn refresh_reuses_the_old_token_and_applies_the_expiry_skew() {
        let mut server = mockito::Server::new_async().await;
        let token_url = format!("{}/oauth2/token", server.url());
        drop(
            server
                .mock("GET", "/.well-known/openid-configuration")
                .with_status(200)
                .with_body(serde_json::json!({ "token_endpoint": token_url }).to_string())
                .create_async()
                .await,
        );
        drop(
            server
                .mock("POST", "/oauth2/token")
                .match_body(mockito::Matcher::UrlEncoded(
                    "grant_type".into(),
                    "refresh_token".into(),
                ))
                .with_status(200)
                .with_body(
                    serde_json::json!({ "access_token": "at2", "expires_in": 3600 }).to_string(),
                )
                .create_async()
                .await,
        );
        drop(
            server
                .mock("GET", "/oauth2/userinfo")
                .with_status(401)
                .create_async()
                .await,
        );

        let tokens = xai_at(&server).refresh("old-rt").await.expect("刷新成功");
        assert_eq!(tokens.access, "at2");
        assert_eq!(tokens.refresh, "old-rt");
        let unskewed = crate::auth::credential::now_ms() + 3_600_000;
        assert!(tokens.expires <= unskewed - 299_000, "5 分钟提前量没生效");
    }

    #[tokio::test]
    async fn unexpected_oauth_error_stops_polling() {
        let mut server = mockito::Server::new_async().await;
        let token_url = format!("{}/oauth2/token", server.url());
        drop(
            server
                .mock("GET", "/.well-known/openid-configuration")
                .with_status(200)
                .with_body(serde_json::json!({ "token_endpoint": token_url }).to_string())
                .create_async()
                .await,
        );
        drop(
            server
                .mock("POST", "/oauth2/device/code")
                .with_status(200)
                .with_body(
                    serde_json::json!({
                        "device_code": "dc", "user_code": "WXYZ",
                        "verification_uri": "https://auth.x.ai/device", "interval": 0
                    })
                    .to_string(),
                )
                .create_async()
                .await,
        );
        drop(
            server
                .mock("POST", "/oauth2/token")
                .with_status(400)
                .with_body(serde_json::json!({ "error": "expired_token" }).to_string())
                .create_async()
                .await,
        );

        let err = xai_at(&server)
            .login(&RecordingPrompt::default())
            .await
            .expect_err("应当失败");
        assert!(matches!(err, AuthError::Denied { error, .. } if error == "expired_token"));
    }
}
