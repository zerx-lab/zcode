//! OAuth 登录与令牌刷新。
//!
//! 三家的流程各不相同：
//!
//! | 提供商 | 流程 | 端口 |
//! | --- | --- | --- |
//! | Anthropic | authorization code + PKCE，JSON body | 固定 54545 |
//! | OpenAI Codex | authorization code + PKCE，表单 body；另有设备码 | 固定 1455 |
//! | xAI | RFC 8628 设备码，无 PKCE、无 `redirect_uri` | 不用回调 |
//!
//! 共性抽在 [`OAuthClient`]：登录拿一份 [`OAuthTokens`]，刷新也拿一份。

pub mod anthropic;
pub mod callback;
pub mod jwt;
pub mod openai_codex;
pub mod pkce;
pub mod xai;

use std::fmt;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use reqwest::{Client, StatusCode};

use crate::auth::credential::{OAuthTokens, now_ms};
use crate::auth::oauth::callback::{CallbackServer, parse_manual_code};
use crate::error::AuthError;
use crate::types::ProviderId;

/// token 请求的超时。
const TOKEN_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// 交互式登录时把 URL / 设备码递给用户的通道，同时提供手工粘贴兜底。
///
/// 库不假设宿主长什么样：CLI 可以写 stderr，TUI 必须自己实现（在 TUI 活动期间
/// 直接写 stdout/stderr 会破坏渲染）。
#[async_trait]
pub trait LoginPrompt: fmt::Debug + Send + Sync {
    /// 提示用户打开授权 URL。实现方可以顺手拉起浏览器。
    fn authorization_url(&self, provider: ProviderId, url: &str);

    /// 设备码流程：展示验证地址与用户码。
    fn device_code(&self, provider: ProviderId, verification_uri: &str, user_code: &str);

    /// 手工粘贴通道，与本机回调**竞速**。
    ///
    /// SSH / 容器里浏览器回连不到本机，回调永远不会到达；此时用户把授权后的
    /// 完整 URL 或 code 粘回来就能完成登录。默认实现永不返回，等价于"本宿主
    /// 不支持粘贴"，不会干扰回调那一支。
    async fn manual_code(&self, provider: ProviderId) -> String {
        let _ = provider;
        std::future::pending().await
    }
}

/// 默认提示：写进给定的 sink，并尝试拉起系统浏览器。
///
/// `sink` 由调用方给定，避免库代码擅自写 stdout/stderr。TUI 宿主请勿使用
/// [`BrowserPrompt::stderr`]。
#[derive(Debug)]
pub struct BrowserPrompt<W> {
    sink: Mutex<W>,
    open_browser: bool,
    read_stdin: bool,
}

impl<W: std::io::Write + Send> BrowserPrompt<W> {
    /// 指定输出目标。
    pub fn to(sink: W) -> Self {
        Self {
            sink: Mutex::new(sink),
            open_browser: true,
            read_stdin: false,
        }
    }

    /// 关闭自动拉起浏览器（无头环境、远程会话）。
    #[must_use]
    pub fn without_browser(mut self) -> Self {
        self.open_browser = false;
        self
    }

    /// 打开 stdin 粘贴兜底。
    ///
    /// 只适用于**独占 stdin** 的一次性 CLI 登录命令。注意：回调先完成时，那次
    /// 阻塞读会一直挂到用户敲回车为止，因此不要在长驻进程里开。
    #[must_use]
    pub fn with_stdin(mut self) -> Self {
        self.read_stdin = true;
        self
    }

    fn write_line(&self, line: &str) {
        let Ok(mut sink) = self.sink.lock() else {
            return;
        };
        if let Err(err) = writeln!(sink, "{line}") {
            tracing::debug!(error = %err, "登录提示写入失败");
        }
        if let Err(err) = sink.flush() {
            tracing::debug!(error = %err, "登录提示 flush 失败");
        }
    }
}

impl BrowserPrompt<std::io::Stderr> {
    /// 面向一次性 CLI 登录命令的默认实现。
    #[must_use]
    pub fn stderr() -> Self {
        Self::to(std::io::stderr())
    }
}

#[async_trait]
impl<W: std::io::Write + Send + Sync + fmt::Debug> LoginPrompt for BrowserPrompt<W> {
    fn authorization_url(&self, provider: ProviderId, url: &str) {
        self.write_line(&format!("请在浏览器中完成 {provider} 授权：\n  {url}"));
        if self.open_browser
            && let Err(err) = webbrowser::open(url)
        {
            tracing::debug!(error = %err, "无法自动打开浏览器，请手动访问上面的链接");
        }
    }

    fn device_code(&self, provider: ProviderId, verification_uri: &str, user_code: &str) {
        self.write_line(&format!(
            "请在浏览器中完成 {provider} 授权：\n  {verification_uri}\n  验证码：{user_code}"
        ));
        if self.open_browser
            && let Err(err) = webbrowser::open(verification_uri)
        {
            tracing::debug!(error = %err, "无法自动打开浏览器，请手动访问上面的链接");
        }
    }

    async fn manual_code(&self, provider: ProviderId) -> String {
        if !self.read_stdin {
            return std::future::pending().await;
        }
        self.write_line(&format!(
            "浏览器无法回连本机时，把 {provider} 授权后的完整 URL 或 code 粘贴到这里并回车："
        ));
        loop {
            match tokio::task::spawn_blocking(read_stdin_line).await {
                Ok(Ok(Some(line))) if !line.trim().is_empty() => return line,
                // EOF：stdin 关了，退回"只等回调"。
                Ok(Ok(None)) => return std::future::pending().await,
                // 空行：用户按了回车但没粘东西，再读一次。
                Ok(Ok(Some(_blank))) => {}
                Ok(Err(err)) => {
                    tracing::warn!(error = %err, "读取粘贴输入失败");
                    return std::future::pending().await;
                }
                Err(err) => {
                    tracing::warn!(error = %err, "粘贴输入任务异常");
                    return std::future::pending().await;
                }
            }
        }
    }
}

/// 阻塞读一行 stdin；`Ok(None)` 表示 EOF。
fn read_stdin_line() -> std::io::Result<Option<String>> {
    use std::io::BufRead as _;
    let mut line = String::new();
    let read = std::io::stdin().lock().read_line(&mut line)?;
    if read == 0 { Ok(None) } else { Ok(Some(line)) }
}

/// 拿到手的授权码及其 `state`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AuthorizationCode {
    pub(crate) code: String,
    pub(crate) state: String,
}

/// 同时等待本机回调与手工粘贴，先到者胜。
///
/// SSH / 容器里浏览器回连不到本机，只等回调必然是干等到超时；反过来在本机有
/// 浏览器时粘贴那一支通常永远不会返回。两支竞速才能覆盖两种环境。
pub(crate) async fn await_authorization_code(
    provider: ProviderId,
    server: CallbackServer,
    prompt: &dyn LoginPrompt,
    timeout: Duration,
) -> Result<AuthorizationCode, AuthError> {
    let expected_state = server.state().to_owned();
    let callback = server.wait(timeout);
    tokio::pin!(callback);
    let mut manual = Box::pin(prompt.manual_code(provider));

    loop {
        tokio::select! {
            biased;
            result = &mut callback => {
                return result.map(|code| AuthorizationCode { code, state: expected_state });
            }
            pasted = &mut manual => {
                match parse_manual_code(&pasted) {
                    // 粘贴内容自带 state 时必须与本次流程一致——否则等于把回调
                    // 路径已经做过的 CSRF 校验从后门绕过去。
                    Some(manual) if manual.state.as_deref().is_none_or(|state| {
                        state == expected_state
                    }) => {
                        return Ok(AuthorizationCode { code: manual.code, state: expected_state });
                    }
                    Some(_mismatched) => {
                        tracing::warn!(%provider, "粘贴内容的 state 与本次流程不符，已忽略");
                    }
                    None => tracing::warn!(%provider, "粘贴内容里找不到 authorization code"),
                }
                // 丢掉这次输入，重新挂上粘贴通道，继续与回调竞速。
                manual = Box::pin(prompt.manual_code(provider));
            }
        }
    }
}

/// 一家提供商的 OAuth 客户端。
#[async_trait]
pub trait OAuthClient: fmt::Debug + Send + Sync + 'static {
    /// 归属的提供商。
    fn provider(&self) -> ProviderId;

    /// 走完交互式登录，拿到首份令牌。
    async fn login(&self, prompt: &dyn LoginPrompt) -> Result<OAuthTokens, AuthError>;

    /// 用 refresh token 换新的访问令牌。
    async fn refresh(&self, refresh_token: &str) -> Result<OAuthTokens, AuthError>;
}

/// 一次 token 端点调用的原始结果。
#[derive(Debug)]
pub(crate) struct TokenExchange {
    pub(crate) status: StatusCode,
    pub(crate) body: serde_json::Value,
}

impl TokenExchange {
    /// 读取顶层字符串字段。
    pub(crate) fn string(&self, key: &str) -> Option<String> {
        self.body
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    }

    /// 读取 OAuth 标准错误码。
    pub(crate) fn oauth_error(&self) -> Option<String> {
        self.string("error")
    }

    /// 把非 2xx 响应转成错误。
    pub(crate) fn into_error(self, provider: ProviderId, operation: &str) -> AuthError {
        match self.oauth_error() {
            Some(error) => AuthError::Denied {
                provider,
                error,
                description: self.string("error_description"),
            },
            None => AuthError::RefreshFailed {
                provider,
                detail: format!("{operation} 返回 HTTP {}：{}", self.status, self.body),
            },
        }
    }
}

/// POST 一个 `application/x-www-form-urlencoded` 请求到 token 端点。
pub(crate) async fn post_form(
    client: &Client,
    url: &str,
    form: &[(&str, &str)],
) -> Result<TokenExchange, AuthError> {
    let response = client
        .post(url)
        .header(reqwest::header::ACCEPT, "application/json")
        .timeout(TOKEN_REQUEST_TIMEOUT)
        .form(form)
        .send()
        .await?;
    into_exchange(response).await
}

/// POST 一个 JSON 请求到 token 端点。
pub(crate) async fn post_json(
    client: &Client,
    url: &str,
    body: &serde_json::Value,
    extra_headers: &[(&'static str, &str)],
) -> Result<TokenExchange, AuthError> {
    let mut request = client.post(url).timeout(TOKEN_REQUEST_TIMEOUT).json(body);
    for (name, value) in extra_headers {
        request = request.header(*name, *value);
    }
    into_exchange(request.send().await?).await
}

async fn into_exchange(response: reqwest::Response) -> Result<TokenExchange, AuthError> {
    let status = response.status();
    let text = response.text().await?;
    let body = match serde_json::from_str(&text) {
        Ok(value) => value,
        // 非 JSON 响应体（网关 HTML 错误页等）原样保留，便于排查。
        Err(_not_json) => serde_json::Value::String(text),
    };
    Ok(TokenExchange { status, body })
}

/// 从 token 响应里取出 `access_token` / `refresh_token` / `expires_in`。
///
/// `fallback_refresh` 用于刷新场景：授权服务器不轮换时不会回传 refresh token，
/// 此时必须沿用旧的，否则凭据会在下一次刷新时作废。
///
/// `expiry_skew` 是写进 `expires` 前额外扣掉的提前量。Anthropic 与 xAI 都在
/// 客户端侧扣 5 分钟。
pub(crate) fn parse_tokens(
    provider: ProviderId,
    exchange: &TokenExchange,
    fallback_refresh: Option<&str>,
    expiry_skew: Duration,
) -> Result<OAuthTokens, AuthError> {
    let missing = |field: &str| AuthError::Protocol {
        provider,
        detail: format!("token 响应缺少 `{field}` 字段"),
    };
    let access = exchange
        .string("access_token")
        .ok_or_else(|| missing("access_token"))?;
    let refresh = exchange
        .string("refresh_token")
        .filter(|token| !token.is_empty())
        .or_else(|| fallback_refresh.map(str::to_owned))
        .ok_or_else(|| missing("refresh_token"))?;
    let expires_in = exchange
        .body
        .get("expires_in")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| missing("expires_in"))?;

    let lifetime_ms = expires_in.saturating_mul(1000);
    let skew_ms = u64::try_from(expiry_skew.as_millis()).unwrap_or(0);
    let expires = now_ms().saturating_add(lifetime_ms).saturating_sub(skew_ms);

    Ok(OAuthTokens {
        access,
        refresh,
        expires,
        account_id: None,
        email: None,
        plan: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exchange(body: serde_json::Value) -> TokenExchange {
        TokenExchange {
            status: StatusCode::OK,
            body,
        }
    }

    #[test]
    fn parses_a_complete_token_response() {
        let before = now_ms();
        let tokens = parse_tokens(
            ProviderId::Anthropic,
            &exchange(serde_json::json!({
                "access_token": "at", "refresh_token": "rt", "expires_in": 3600
            })),
            None,
            Duration::ZERO,
        )
        .expect("解析");
        assert_eq!(tokens.access, "at");
        assert_eq!(tokens.refresh, "rt");
        assert!(tokens.expires >= before + 3_600_000);
    }

    #[test]
    fn expiry_skew_is_subtracted() {
        let tokens = parse_tokens(
            ProviderId::Xai,
            &exchange(serde_json::json!({
                "access_token": "at", "refresh_token": "rt", "expires_in": 3600
            })),
            None,
            Duration::from_mins(5),
        )
        .expect("解析");
        let without_skew = now_ms() + 3_600_000;
        assert!(tokens.expires <= without_skew - 299_000);
    }

    #[test]
    fn absent_refresh_token_falls_back_to_the_stored_one() {
        let tokens = parse_tokens(
            ProviderId::Xai,
            &exchange(serde_json::json!({ "access_token": "at", "expires_in": 60 })),
            Some("old-refresh"),
            Duration::ZERO,
        )
        .expect("解析");
        assert_eq!(tokens.refresh, "old-refresh");
    }

    #[test]
    fn empty_refresh_token_also_falls_back() {
        let tokens = parse_tokens(
            ProviderId::Xai,
            &exchange(serde_json::json!({
                "access_token": "at", "refresh_token": "", "expires_in": 60
            })),
            Some("old-refresh"),
            Duration::ZERO,
        )
        .expect("解析");
        assert_eq!(tokens.refresh, "old-refresh");
    }

    #[test]
    fn missing_required_fields_are_protocol_errors() {
        let err = parse_tokens(
            ProviderId::OpenAiCodex,
            &exchange(serde_json::json!({ "refresh_token": "rt", "expires_in": 1 })),
            None,
            Duration::ZERO,
        )
        .expect_err("缺 access_token");
        assert!(matches!(err, AuthError::Protocol { .. }));

        let err = parse_tokens(
            ProviderId::OpenAiCodex,
            &exchange(serde_json::json!({ "access_token": "at", "refresh_token": "rt" })),
            None,
            Duration::ZERO,
        )
        .expect_err("缺 expires_in");
        assert!(matches!(err, AuthError::Protocol { .. }));
    }

    #[test]
    fn oauth_error_bodies_become_denied() {
        let failure = TokenExchange {
            status: StatusCode::BAD_REQUEST,
            body: serde_json::json!({
                "error": "invalid_grant", "error_description": "refresh token 已失效"
            }),
        };
        match failure.into_error(ProviderId::Anthropic, "刷新") {
            AuthError::Denied {
                error, description, ..
            } => {
                assert_eq!(error, "invalid_grant");
                assert_eq!(description.as_deref(), Some("refresh token 已失效"));
            }
            other => panic!("期望 Denied，实际 {other:?}"),
        }
    }

    #[test]
    fn opaque_failures_become_refresh_failed() {
        let failure = TokenExchange {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            body: serde_json::Value::String("gateway down".to_owned()),
        };
        assert!(matches!(
            failure.into_error(ProviderId::Anthropic, "刷新"),
            AuthError::RefreshFailed { .. }
        ));
    }
}
