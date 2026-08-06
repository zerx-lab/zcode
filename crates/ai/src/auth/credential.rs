//! 凭据的内存与磁盘表示。

use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// OAuth 凭据在到期前多久就视为需要刷新。
///
/// 对齐 oh-my-pi `OAUTH_REFRESH_SKEW_MS = 60_000`：留够一次网络往返，
/// 避免 token 在请求飞行途中失效。
pub const REFRESH_SKEW_MS: u64 = 60_000;

/// 当前 Unix 毫秒时间戳。
#[must_use]
pub fn now_ms() -> u64 {
    let since_epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    u64::try_from(since_epoch.as_millis()).unwrap_or(u64::MAX)
}

/// 一份 OAuth 凭据。
///
/// `expires` 是 Unix **毫秒**，与 oh-my-pi 的 `OAuthCredentials.expires` 一致。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OAuthCredential {
    /// 访问令牌。
    pub access: String,
    /// 刷新令牌。
    pub refresh: String,
    /// 访问令牌到期时刻（Unix 毫秒）。
    pub expires: u64,
    /// 提供商侧账号 id（Codex 的 `chatgpt_account_id`、xAI 的 `sub`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    /// 账号邮箱。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// 订阅档位（Codex 的 `chatgpt_plan_type` 等）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    /// 交互式授权完成的时刻（Unix 毫秒）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authorized_at: Option<u64>,
}

impl OAuthCredential {
    /// 是否还能直接用——留出 [`REFRESH_SKEW_MS`] 的提前量。
    #[must_use]
    pub fn is_fresh(&self) -> bool {
        now_ms().saturating_add(REFRESH_SKEW_MS) < self.expires
    }
}

/// 一份 API key 凭据。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiKeyCredential {
    /// 密钥原文。
    pub key: String,
}

/// 存储里的一条凭据。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Credential {
    /// 静态 API key。
    ApiKey(ApiKeyCredential),
    /// OAuth 令牌对。
    Oauth(OAuthCredential),
}

/// 授权服务器返回的令牌，尚未落盘。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthTokens {
    /// 访问令牌。
    pub access: String,
    /// 刷新令牌。
    pub refresh: String,
    /// 到期时刻（Unix 毫秒）。
    pub expires: u64,
    /// 账号 id。
    pub account_id: Option<String>,
    /// 邮箱。
    pub email: Option<String>,
    /// 订阅档位。
    pub plan: Option<String>,
}

impl OAuthTokens {
    /// 落成可持久化的凭据；`authorized_at` 只在交互式登录时写入。
    #[must_use]
    pub fn into_credential(self, authorized_at: Option<u64>) -> OAuthCredential {
        OAuthCredential {
            access: self.access,
            refresh: self.refresh,
            expires: self.expires,
            account_id: self.account_id,
            email: self.email,
            plan: self.plan,
            authorized_at,
        }
    }
}

/// 凭据来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessKind {
    /// 静态 API key（存储或环境变量）。
    ApiKey,
    /// OAuth 访问令牌。
    OAuth,
}

/// 一次请求实际要用的鉴权材料。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Access {
    /// Bearer / `x-api-key` 的值。
    pub token: String,
    /// 来源，决定提供商适配器走哪条鉴权分支。
    pub kind: AccessKind,
    /// 账号 id，Codex 要把它放进 `chatgpt-account-id` 头。
    pub account_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_json_is_internally_tagged() {
        let cred = Credential::ApiKey(ApiKeyCredential {
            key: "sk-1".to_owned(),
        });
        let json = serde_json::to_string(&cred).unwrap_or_default();
        assert_eq!(json, r#"{"type":"api_key","key":"sk-1"}"#);
    }

    #[test]
    fn optional_oauth_fields_are_omitted_when_absent() {
        let cred = Credential::Oauth(OAuthCredential {
            access: "a".to_owned(),
            refresh: "r".to_owned(),
            expires: 5,
            account_id: None,
            email: None,
            plan: None,
            authorized_at: None,
        });
        let json = serde_json::to_string(&cred).unwrap_or_default();
        assert_eq!(
            json,
            r#"{"type":"oauth","access":"a","refresh":"r","expires":5}"#
        );
    }

    #[test]
    fn stored_credential_roundtrips() {
        let cred = Credential::Oauth(OAuthCredential {
            access: "a".to_owned(),
            refresh: "r".to_owned(),
            expires: 42,
            account_id: Some("acct".to_owned()),
            email: Some("me@example.com".to_owned()),
            plan: Some("pro".to_owned()),
            authorized_at: Some(7),
        });
        let json = serde_json::to_string(&cred).unwrap_or_default();
        let back: Credential = serde_json::from_str(&json)
            .unwrap_or(Credential::ApiKey(ApiKeyCredential { key: String::new() }));
        assert_eq!(back, cred);
    }

    #[test]
    fn token_within_the_refresh_skew_is_not_fresh() {
        let almost = OAuthCredential {
            access: "a".to_owned(),
            refresh: "r".to_owned(),
            expires: now_ms() + REFRESH_SKEW_MS / 2,
            account_id: None,
            email: None,
            plan: None,
            authorized_at: None,
        };
        assert!(!almost.is_fresh());

        let comfortable = OAuthCredential {
            expires: now_ms() + REFRESH_SKEW_MS * 10,
            ..almost
        };
        assert!(comfortable.is_fresh());
    }
}
