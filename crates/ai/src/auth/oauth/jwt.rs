//! 只读 JWT payload 解码。
//!
//! **不做签名校验**：token 是我们刚从授权服务器经 TLS 取回来的，这里只是把
//! 服务端已经放进去的 claim（账号 id、邮箱、套餐、过期时刻）读出来，不做任何
//! 信任决策。校验签名需要 JWKS 轮询，收益为零。

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD_NO_PAD, URL_SAFE_NO_PAD};

/// 解出 JWT 的 payload JSON。
///
/// 兼容 base64url 与标准 base64 两种编码（部分服务端不做 URL-safe 替换）。
#[must_use]
pub fn decode_payload(token: &str) -> Option<serde_json::Value> {
    let mut parts = token.split('.');
    let _header = parts.next()?;
    let payload = parts.next()?;
    parts.next()?;

    let bytes = URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .or_else(|_| STANDARD_NO_PAD.decode(payload.trim_end_matches('=')))
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// 读取顶层字符串 claim。
#[must_use]
pub fn string_claim(payload: &serde_json::Value, key: &str) -> Option<String> {
    payload
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

/// 读取命名空间对象下的字符串 claim（OpenAI 用 `https://api.openai.com/auth` 这种 URI 作 key）。
#[must_use]
pub fn nested_string_claim(
    payload: &serde_json::Value,
    namespace: &str,
    key: &str,
) -> Option<String> {
    payload
        .get(namespace)?
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

/// 读取 `exp`（Unix 秒）并换算成 Unix 毫秒。
#[must_use]
pub fn expires_at_ms(payload: &serde_json::Value) -> Option<u64> {
    let seconds = payload.get("exp")?.as_u64()?;
    seconds.checked_mul(1000)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode(payload: &serde_json::Value) -> String {
        let body = URL_SAFE_NO_PAD.encode(serde_json::to_vec(payload).expect("序列化"));
        format!("header.{body}.signature")
    }

    #[test]
    fn reads_openai_namespaced_account_id() {
        let token = encode(&serde_json::json!({
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "acct-123",
                "chatgpt_plan_type": "pro"
            },
            "https://api.openai.com/profile": { "email": "Me@Example.com" }
        }));
        let payload = decode_payload(&token).expect("解码");
        assert_eq!(
            nested_string_claim(
                &payload,
                "https://api.openai.com/auth",
                "chatgpt_account_id"
            ),
            Some("acct-123".to_owned())
        );
        assert_eq!(
            nested_string_claim(&payload, "https://api.openai.com/profile", "email"),
            Some("Me@Example.com".to_owned())
        );
    }

    #[test]
    fn reads_subject_and_expiry() {
        let token = encode(&serde_json::json!({ "sub": "user-9", "exp": 1_700_000_000_u64 }));
        let payload = decode_payload(&token).expect("解码");
        assert_eq!(string_claim(&payload, "sub"), Some("user-9".to_owned()));
        assert_eq!(expires_at_ms(&payload), Some(1_700_000_000_000));
    }

    #[test]
    fn accepts_standard_base64_payloads() {
        // `~~~` 的 base64 是 `fn5+`，其中 `+` 只在标准字母表里合法。
        let raw = serde_json::to_vec(&serde_json::json!({ "sub": "~~~" })).expect("序列化");
        let token = format!("h.{}.s", STANDARD_NO_PAD.encode(&raw));
        let payload = decode_payload(&token).expect("解码");
        assert_eq!(string_claim(&payload, "sub"), Some("~~~".to_owned()));
    }

    #[test]
    fn rejects_malformed_tokens() {
        assert!(decode_payload("").is_none());
        assert!(decode_payload("only.two").is_none());
        assert!(decode_payload("a.!!!not-base64!!!.c").is_none());
        assert!(decode_payload(&format!("h.{}.s", URL_SAFE_NO_PAD.encode("not json"))).is_none());
    }

    #[test]
    fn missing_claims_yield_none_rather_than_defaults() {
        let payload = decode_payload(&encode(&serde_json::json!({}))).expect("解码");
        assert_eq!(string_claim(&payload, "sub"), None);
        assert_eq!(nested_string_claim(&payload, "ns", "k"), None);
        assert_eq!(expires_at_ms(&payload), None);
    }
}
