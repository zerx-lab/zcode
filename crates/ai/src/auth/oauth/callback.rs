//! OAuth 授权码回调用的本地环回 HTTP 服务器。
//!
//! 只处理一件事：接住浏览器重定向回来的 `?code=&state=`，回一张静态页面，把
//! authorization code 交回登录流程。因此不引 HTTP 框架，直接在 `TcpListener`
//! 上解析请求行。

use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};

use crate::error::AuthError;
use crate::types::ProviderId;

/// 等待用户完成授权的默认时限，与 oh-my-pi `DEFAULT_TIMEOUT` 一致。
pub const DEFAULT_TIMEOUT: Duration = Duration::from_mins(5);

/// 单个请求最多读多少字节——回调 URL 再长也远小于此。
const MAX_REQUEST_BYTES: usize = 16 * 1024;

const SUCCESS_HTML: &str = include_str!("callback_success.html");
const FAILURE_HTML: &str = include_str!("callback_failure.html");
const NOT_FOUND_HTML: &str = "<!doctype html><meta charset=\"utf-8\"><h1>404</h1>";

/// 端口占用时的处理方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortPolicy {
    /// `redirect_uri` 里写死了端口，占用即失败。
    Fixed,
    /// 端口只是偏好，占用时退到系统分配的随机端口。
    PreferredThenRandom,
}

/// 已绑定端口、等待回调的本地服务器。
#[derive(Debug)]
pub struct CallbackServer {
    provider: ProviderId,
    listener: TcpListener,
    redirect_uri: String,
    path: String,
    state: String,
}

impl CallbackServer {
    /// 绑定环回地址。
    ///
    /// `path` 必须以 `/` 开头；`state` 是本次流程的 CSRF token，回调里对不上就
    /// 拒绝，并且**不**结束等待——否则任何人往本地端口发一条伪造回调就能打断
    /// 真实登录。
    pub async fn bind(
        provider: ProviderId,
        port: u16,
        path: &str,
        policy: PortPolicy,
        state: String,
    ) -> Result<Self, AuthError> {
        let listener = match TcpListener::bind((Ipv4Addr::LOCALHOST, port)).await {
            Ok(listener) => listener,
            Err(err) if policy == PortPolicy::PreferredThenRandom => {
                tracing::debug!(port, error = %err, "首选回调端口不可用，改用随机端口");
                TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?
            }
            Err(err) => {
                return Err(AuthError::Protocol {
                    provider,
                    detail: format!(
                        "回调端口 {port} 被占用，而 {provider} 的 redirect_uri 写死了该端口：{err}"
                    ),
                });
            }
        };
        let local: SocketAddr = listener.local_addr()?;
        let redirect_uri = format!("http://localhost:{}{path}", local.port());
        Ok(Self {
            provider,
            listener,
            redirect_uri,
            path: path.to_owned(),
            state,
        })
    }

    /// 本次流程要交给授权服务器的 `redirect_uri`。
    #[must_use]
    pub fn redirect_uri(&self) -> &str {
        &self.redirect_uri
    }

    /// 本次流程的 `state`。
    #[must_use]
    pub fn state(&self) -> &str {
        &self.state
    }

    /// 等待回调，返回 authorization code。
    pub async fn wait(self, timeout: Duration) -> Result<String, AuthError> {
        let provider = self.provider;
        tokio::time::timeout(timeout, self.accept_loop())
            .await
            .map_err(|_elapsed| AuthError::Timeout { provider })?
    }

    async fn accept_loop(self) -> Result<String, AuthError> {
        loop {
            let (mut stream, _peer) = self.listener.accept().await?;
            let Some(target) = read_request_target(&mut stream).await? else {
                respond(&mut stream, 400, NOT_FOUND_HTML).await;
                continue;
            };
            match self.classify(&target) {
                Outcome::Ignore => respond(&mut stream, 404, NOT_FOUND_HTML).await,
                Outcome::StateMismatch => {
                    tracing::warn!(provider = %self.provider, "回调 state 不匹配，已忽略");
                    respond(
                        &mut stream,
                        400,
                        &failure_page("state 不匹配，疑似伪造回调"),
                    )
                    .await;
                }
                Outcome::Denied { error, description } => {
                    let detail = description.clone().unwrap_or_else(|| error.clone());
                    respond(&mut stream, 400, &failure_page(&detail)).await;
                    return Err(AuthError::Denied {
                        provider: self.provider,
                        error,
                        description,
                    });
                }
                Outcome::Code(code) => {
                    respond(&mut stream, 200, SUCCESS_HTML).await;
                    return Ok(code);
                }
            }
        }
    }

    fn classify(&self, target: &str) -> Outcome {
        // 请求行里只有路径，补一个 authority 才能交给 URL 解析器。
        let Ok(url) = url::Url::parse(&format!("http://localhost{target}")) else {
            return Outcome::Ignore;
        };
        if url.path() != self.path {
            return Outcome::Ignore;
        }
        let mut code = None;
        let mut state = None;
        let mut error = None;
        let mut description = None;
        for (key, value) in url.query_pairs() {
            match key.as_ref() {
                "code" => code = Some(value.into_owned()),
                "state" => state = Some(value.into_owned()),
                "error" => error = Some(value.into_owned()),
                "error_description" => description = Some(value.into_owned()),
                _ => {}
            }
        }
        if state.as_deref() != Some(self.state.as_str()) {
            return Outcome::StateMismatch;
        }
        if let Some(error) = error {
            return Outcome::Denied { error, description };
        }
        code.map_or(
            Outcome::Denied {
                error: "invalid_response".to_owned(),
                description: Some("回调里既没有 code 也没有 error".to_owned()),
            },
            Outcome::Code,
        )
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    Ignore,
    StateMismatch,
    Denied {
        error: String,
        description: Option<String>,
    },
    Code(String),
}

/// 用户手工粘回来的授权结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManualCode {
    /// authorization code。
    pub code: String,
    /// 一并粘回来的 `state`，可能没有。
    pub state: Option<String>,
}

/// 解析手工粘贴的内容。
///
/// 用户可能粘回四种东西，都要能认：
///
/// 1. 完整重定向 URL：`http://localhost:54545/callback?code=A&state=B`
/// 2. 光查询串：`?code=A&state=B` 或 `code=A&state=B`
/// 3. Claude Code 授权页展示的 `code#state`
/// 4. 光 code
#[must_use]
pub fn parse_manual_code(input: &str) -> Option<ManualCode> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(parsed) = parse_query_like(trimmed) {
        return Some(parsed);
    }
    match trimmed.split_once('#') {
        Some((code, state)) if !code.is_empty() && !state.is_empty() => Some(ManualCode {
            code: code.to_owned(),
            state: Some(state.to_owned()),
        }),
        _ => Some(ManualCode {
            code: trimmed.to_owned(),
            state: None,
        }),
    }
}

/// 尝试把输入当 URL 或查询串解析出 `code` / `state`。
fn parse_query_like(input: &str) -> Option<ManualCode> {
    let query = if input.contains("://") {
        url::Url::parse(input).ok()?.query()?.to_owned()
    } else if input.contains("code=") {
        input.trim_start_matches('?').to_owned()
    } else {
        return None;
    };

    let mut code = None;
    let mut state = None;
    for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
        match key.as_ref() {
            "code" => code = Some(value.into_owned()),
            "state" => state = Some(value.into_owned()),
            _ => {}
        }
    }
    code.filter(|value| !value.is_empty())
        .map(|code| ManualCode { code, state })
}

/// 读出请求行里的 request-target（`GET <target> HTTP/1.1`）。
async fn read_request_target(stream: &mut TcpStream) -> Result<Option<String>, AuthError> {
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 1024];
    loop {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(chunk.get(..read).unwrap_or_default());
        if find_subslice(&buffer, b"\r\n").is_some() || buffer.len() >= MAX_REQUEST_BYTES {
            break;
        }
    }
    let end = find_subslice(&buffer, b"\r\n").unwrap_or(buffer.len());
    let Some(line) = buffer
        .get(..end)
        .and_then(|raw| std::str::from_utf8(raw).ok())
    else {
        return Ok(None);
    };
    let mut parts = line.split(' ');
    let method = parts.next().unwrap_or_default();
    let target = parts.next().unwrap_or_default();
    if method != "GET" || target.is_empty() {
        return Ok(None);
    }
    Ok(Some(target.to_owned()))
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn failure_page(detail: &str) -> String {
    FAILURE_HTML.replace("{{detail}}", &html_escape(detail))
}

fn html_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            other => out.push(other),
        }
    }
    out
}

/// 写一条最小的 HTTP/1.1 响应；写失败只影响那张页面，不该中断登录流程。
async fn respond(stream: &mut TcpStream, status: u16, body: &str) {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        _ => "Not Found",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    if let Err(err) = stream.write_all(head.as_bytes()).await {
        tracing::debug!(error = %err, "回调响应头写入失败");
        return;
    }
    if let Err(err) = stream.write_all(body.as_bytes()).await {
        tracing::debug!(error = %err, "回调响应体写入失败");
        return;
    }
    if let Err(err) = stream.flush().await {
        tracing::debug!(error = %err, "回调响应 flush 失败");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn server(policy: PortPolicy, port: u16) -> CallbackServer {
        CallbackServer::bind(
            ProviderId::Anthropic,
            port,
            "/callback",
            policy,
            "st4te".to_owned(),
        )
        .await
        .expect("绑定回调端口")
    }

    async fn get(redirect_uri: &str, target: &str) -> String {
        let authority = redirect_uri.trim_start_matches("http://");
        let host = authority.split('/').next().unwrap_or(authority);
        let mut stream = TcpStream::connect(host).await.expect("连接回调服务器");
        stream
            .write_all(format!("GET {target} HTTP/1.1\r\nHost: {host}\r\n\r\n").as_bytes())
            .await
            .expect("发送请求");
        let mut response = String::new();
        stream
            .read_to_string(&mut response)
            .await
            .expect("读取响应");
        response
    }

    #[tokio::test]
    async fn redirect_uri_reflects_the_bound_port() {
        let server = server(PortPolicy::PreferredThenRandom, 0).await;
        assert!(server.redirect_uri().starts_with("http://localhost:"));
        assert!(server.redirect_uri().ends_with("/callback"));
        assert_eq!(server.state(), "st4te");
    }

    #[tokio::test]
    async fn delivers_the_code_when_state_matches() {
        let server = server(PortPolicy::PreferredThenRandom, 0).await;
        let uri = server.redirect_uri().to_owned();
        let waiter = tokio::spawn(server.wait(DEFAULT_TIMEOUT));

        let response = get(&uri, "/callback?code=abc123&state=st4te").await;
        assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");
        assert_eq!(waiter.await.expect("join").expect("code"), "abc123");
    }

    #[tokio::test]
    async fn ignores_unrelated_paths_and_keeps_waiting() {
        let server = server(PortPolicy::PreferredThenRandom, 0).await;
        let uri = server.redirect_uri().to_owned();
        let waiter = tokio::spawn(server.wait(DEFAULT_TIMEOUT));

        assert!(get(&uri, "/favicon.ico").await.starts_with("HTTP/1.1 404"));
        let response = get(&uri, "/callback?code=late&state=st4te").await;
        assert!(response.starts_with("HTTP/1.1 200 OK"));
        assert_eq!(waiter.await.expect("join").expect("code"), "late");
    }

    #[tokio::test]
    async fn forged_state_does_not_resolve_the_flow() {
        let server = server(PortPolicy::PreferredThenRandom, 0).await;
        let uri = server.redirect_uri().to_owned();
        let waiter = tokio::spawn(server.wait(DEFAULT_TIMEOUT));

        let response = get(&uri, "/callback?code=evil&state=wrong").await;
        assert!(response.starts_with("HTTP/1.1 400"), "{response}");
        // 伪造回调不能终止等待：真实回调随后仍应被接住。
        let response = get(&uri, "/callback?code=real&state=st4te").await;
        assert!(response.starts_with("HTTP/1.1 200 OK"));
        assert_eq!(waiter.await.expect("join").expect("code"), "real");
    }

    #[tokio::test]
    async fn provider_denial_surfaces_as_denied() {
        let server = server(PortPolicy::PreferredThenRandom, 0).await;
        let uri = server.redirect_uri().to_owned();
        let waiter = tokio::spawn(server.wait(DEFAULT_TIMEOUT));

        drop(
            get(
                &uri,
                "/callback?error=access_denied&error_description=nope&state=st4te",
            )
            .await,
        );
        let err = waiter.await.expect("join").expect_err("应当报错");
        match err {
            AuthError::Denied {
                error, description, ..
            } => {
                assert_eq!(error, "access_denied");
                assert_eq!(description.as_deref(), Some("nope"));
            }
            other => panic!("期望 Denied，实际 {other:?}"),
        }
    }

    #[tokio::test]
    async fn times_out_when_nobody_calls_back() {
        let server = server(PortPolicy::PreferredThenRandom, 0).await;
        let err = server
            .wait(Duration::from_millis(50))
            .await
            .expect_err("应当超时");
        assert!(matches!(err, AuthError::Timeout { .. }));
    }

    #[tokio::test]
    async fn fixed_port_policy_refuses_to_silently_move() {
        let occupied = server(PortPolicy::PreferredThenRandom, 0).await;
        let port = occupied.listener.local_addr().expect("本地地址").port();

        let err = CallbackServer::bind(
            ProviderId::OpenAiCodex,
            port,
            "/auth/callback",
            PortPolicy::Fixed,
            "s".to_owned(),
        )
        .await
        .expect_err("端口已占用应当失败");
        assert!(matches!(err, AuthError::Protocol { .. }));
    }

    #[test]
    fn html_escaping_neutralizes_injected_markup() {
        let page = failure_page("<script>alert('x')</script>");
        assert!(!page.contains("<script>"));
        assert!(page.contains("&lt;script&gt;"));
    }
}
