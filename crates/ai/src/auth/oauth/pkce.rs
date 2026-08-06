//! PKCE（RFC 7636）与 CSRF state 生成。
//!
//! 这两个值都是安全关键：verifier 不可预测才能防授权码拦截，state 不可预测才能
//! 防 CSRF。取不到系统随机数时**必须**中止登录，不存在可接受的降级值——固定或
//! 空的 verifier 会让 PKCE 形同虚设，空 state 等于没有 CSRF 绑定。

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest as _, Sha256};

use crate::error::AuthError;

/// verifier 的随机字节数。
///
/// 与 oh-my-pi `pkce.ts` 一致取 96 字节 → base64url 后 128 字符，正好是
/// RFC 7636 允许的上限。
const VERIFIER_BYTES: usize = 96;

/// CSRF `state` 的随机字节数；base64url 后 22 字符。
const STATE_BYTES: usize = 16;

/// 一对 PKCE 参数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pkce {
    /// `code_verifier`，token 交换时原样回传。
    pub verifier: String,
    /// `code_challenge`，= base64url(SHA-256(verifier))，无 padding。
    pub challenge: String,
}

impl Pkce {
    /// 由现成的 verifier 推导 challenge。
    #[must_use]
    pub fn from_verifier(verifier: String) -> Self {
        let digest = Sha256::digest(verifier.as_bytes());
        let challenge = URL_SAFE_NO_PAD.encode(digest);
        Self {
            verifier,
            challenge,
        }
    }

    /// 生成一对新的 PKCE 参数。
    ///
    /// 系统熵源不可用时返回 [`AuthError::Entropy`]，调用方必须放弃本次登录。
    pub fn generate() -> Result<Self, AuthError> {
        Ok(Self::from_verifier(random_base64url(VERIFIER_BYTES)?))
    }

    /// `code_challenge_method` 的取值。
    #[must_use]
    pub fn method(&self) -> &'static str {
        "S256"
    }
}

/// 生成一个 CSRF `state`。
///
/// 系统熵源不可用时返回 [`AuthError::Entropy`]。
pub fn random_state() -> Result<String, AuthError> {
    random_base64url(STATE_BYTES)
}

fn random_base64url(len: usize) -> Result<String, AuthError> {
    let mut bytes = vec![0_u8; len];
    fill_random(&mut bytes)?;
    Ok(URL_SAFE_NO_PAD.encode(&bytes))
}

#[cfg(not(test))]
fn fill_random(buffer: &mut [u8]) -> Result<(), AuthError> {
    getrandom::fill(buffer).map_err(|err| AuthError::Entropy(err.to_string()))
}

#[cfg(test)]
thread_local! {
    static ENTROPY_FAILS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
fn fill_random(buffer: &mut [u8]) -> Result<(), AuthError> {
    if ENTROPY_FAILS.with(std::cell::Cell::get) {
        return Err(AuthError::Entropy("测试注入的熵源故障".to_owned()));
    }
    getrandom::fill(buffer).map_err(|err| AuthError::Entropy(err.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct BrokenEntropy;

    impl BrokenEntropy {
        fn install() -> Self {
            ENTROPY_FAILS.with(|flag| flag.set(true));
            Self
        }
    }

    impl Drop for BrokenEntropy {
        fn drop(&mut self) {
            ENTROPY_FAILS.with(|flag| flag.set(false));
        }
    }

    #[test]
    fn challenge_matches_rfc7636_appendix_b_vector() {
        // RFC 7636 附录 B 的官方测试向量。
        let pkce = Pkce::from_verifier("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk".to_owned());
        assert_eq!(
            pkce.challenge,
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        assert_eq!(pkce.method(), "S256");
    }

    #[test]
    fn generated_verifier_fits_rfc7636_length_limits() {
        let pkce = Pkce::generate().expect("生成 PKCE");
        assert_eq!(pkce.verifier.len(), 128);
        assert!((43..=128).contains(&pkce.verifier.len()));
        assert_eq!(pkce.challenge.len(), 43);
    }

    #[test]
    fn generated_values_are_url_safe_and_unpadded() {
        let pkce = Pkce::generate().expect("生成 PKCE");
        let ok = |s: &str| {
            s.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        };
        assert!(ok(&pkce.verifier), "{}", pkce.verifier);
        assert!(ok(&pkce.challenge), "{}", pkce.challenge);
        assert!(ok(&random_state().expect("生成 state")));
    }

    #[test]
    fn successive_generations_differ() {
        let first = Pkce::generate().expect("生成 PKCE");
        let second = Pkce::generate().expect("生成 PKCE");
        assert_ne!(first.verifier, second.verifier);
        assert_ne!(
            random_state().expect("state"),
            random_state().expect("state")
        );
    }

    #[test]
    fn entropy_failure_aborts_instead_of_degrading() {
        let _guard = BrokenEntropy::install();
        assert!(matches!(Pkce::generate(), Err(AuthError::Entropy(_))));
        assert!(matches!(random_state(), Err(AuthError::Entropy(_))));
    }
}
