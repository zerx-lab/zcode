//! 凭据解析：登录、持久化、按需刷新。
//!
//! [`AuthStore`] 是提供商适配器唯一的鉴权入口。它负责：
//!
//! 1. 从存储或环境变量取出凭据；
//! 2. OAuth 令牌临近过期时刷新（[`credential::REFRESH_SKEW_MS`] 提前量）；
//! 3. 收敛并发刷新，三道防线叠着用：
//!    - **进程内**：per-provider 异步锁，同进程只有一个任务真的去刷；
//!    - **锁后重读**：拿到锁再读一次存储，别的进程刚刷完就直接采纳；
//!    - **失败后重读**：轮换型 refresh token 下，跨进程竞争的输家必然拿到
//!      `invalid_grant`——此时若存储里的 token 已经变了且可用，说明赢家已经写好，
//!      采纳它而不是把错误抛给调用方。
//!
//! 这里刻意**没有**跨进程租约：那需要在网络往返期间一直持有文件锁，会把
//! 执行器线程钉住。代价是极小概率下输家会白跑一次刷新请求。

pub mod credential;
pub mod oauth;
pub mod store;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use crate::auth::credential::{
    Access, AccessKind, ApiKeyCredential, Credential, OAuthCredential, now_ms,
};
use crate::auth::oauth::anthropic::AnthropicOAuth;
use crate::auth::oauth::openai_codex::OpenAiCodexOAuth;
use crate::auth::oauth::xai::XaiOAuth;
use crate::auth::oauth::{LoginPrompt, OAuthClient};
use crate::auth::store::{CredentialStore, FileCredentialStore, SwapOutcome};
use crate::error::AuthError;
use crate::types::ProviderId;

/// 凭据解析器。
#[derive(Debug)]
pub struct AuthStore {
    store: Arc<dyn CredentialStore>,
    clients: BTreeMap<ProviderId, Arc<dyn OAuthClient>>,
    refresh_locks: Mutex<BTreeMap<ProviderId, Arc<tokio::sync::Mutex<()>>>>,
}

impl AuthStore {
    /// 用默认文件存储与内置 OAuth 客户端构造。
    pub fn discover() -> Result<Self, AuthError> {
        let store = Arc::new(FileCredentialStore::discover()?);
        Self::new(store)
    }

    /// 用指定存储与内置 OAuth 客户端构造。
    pub fn new(store: Arc<dyn CredentialStore>) -> Result<Self, AuthError> {
        Ok(Self::bare(store)
            .register(Arc::new(AnthropicOAuth::new()?))
            .register(Arc::new(OpenAiCodexOAuth::new()?))
            .register(Arc::new(XaiOAuth::new()?)))
    }

    /// 用指定存储构造，不注册任何 OAuth 客户端。
    #[must_use]
    pub fn bare(store: Arc<dyn CredentialStore>) -> Self {
        Self {
            store,
            clients: BTreeMap::new(),
            refresh_locks: Mutex::new(BTreeMap::new()),
        }
    }

    /// 注册（或替换）一个提供商的 OAuth 客户端。
    #[must_use]
    pub fn register(mut self, client: Arc<dyn OAuthClient>) -> Self {
        drop(self.clients.insert(client.provider(), client));
        self
    }

    /// 解析出本次请求要用的鉴权材料，必要时先刷新令牌。
    pub async fn access(&self, provider: ProviderId) -> Result<Access, AuthError> {
        match self.load(provider).await? {
            Some(Credential::ApiKey(key)) => Ok(api_key_access(key.key)),
            Some(Credential::Oauth(oauth)) if oauth.is_fresh() => Ok(oauth_access(&oauth)),
            Some(Credential::Oauth(_stale)) => self.refresh(provider).await,
            // 只有在没有已存凭据时才看环境变量：显式登录过的账号优先级更高。
            None => env_access(provider).ok_or(AuthError::Missing(provider)),
        }
    }

    /// 跑一遍交互式登录并把结果落盘。
    pub async fn login(
        &self,
        provider: ProviderId,
        prompt: &dyn LoginPrompt,
    ) -> Result<Access, AuthError> {
        let client = self.client(provider)?;
        let tokens = client.login(prompt).await?;
        let credential = Credential::Oauth(tokens.into_credential(Some(now_ms())));
        self.save(provider, credential.clone()).await?;
        match credential {
            Credential::Oauth(oauth) => Ok(oauth_access(&oauth)),
            Credential::ApiKey(key) => Ok(api_key_access(key.key)),
        }
    }

    /// 保存一份 API key。
    pub async fn set_api_key(
        &self,
        provider: ProviderId,
        key: impl Into<String>,
    ) -> Result<(), AuthError> {
        self.save(
            provider,
            Credential::ApiKey(ApiKeyCredential { key: key.into() }),
        )
        .await
    }

    /// 删除某提供商的凭据。
    pub async fn logout(&self, provider: ProviderId) -> Result<(), AuthError> {
        let store = Arc::clone(&self.store);
        blocking(move || store.remove(provider)).await
    }

    fn client(&self, provider: ProviderId) -> Result<Arc<dyn OAuthClient>, AuthError> {
        self.clients
            .get(&provider)
            .map(Arc::clone)
            .ok_or(AuthError::RefreshFailed {
                provider,
                detail: "没有为该提供商注册 OAuth 客户端".to_owned(),
            })
    }

    async fn load(&self, provider: ProviderId) -> Result<Option<Credential>, AuthError> {
        let store = Arc::clone(&self.store);
        blocking(move || store.load(provider)).await
    }

    async fn save(&self, provider: ProviderId, credential: Credential) -> Result<(), AuthError> {
        let store = Arc::clone(&self.store);
        blocking(move || store.save(provider, credential)).await
    }

    fn refresh_lock(&self, provider: ProviderId) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self
            .refresh_locks
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        Arc::clone(locks.entry(provider).or_default())
    }

    /// 刷新一份临近过期的 OAuth 凭据。
    async fn refresh(&self, provider: ProviderId) -> Result<Access, AuthError> {
        let lock = self.refresh_lock(provider);
        let _guard = lock.lock().await;

        // 排队期间别人可能已经刷过了；无论如何都要以重读到的凭据为准，
        // 拿调用方传进来的旧快照去刷是必错的。
        let current = match self.load(provider).await? {
            Some(Credential::Oauth(fresh)) if fresh.is_fresh() => {
                return Ok(oauth_access(&fresh));
            }
            Some(Credential::ApiKey(key)) => return Ok(api_key_access(key.key)),
            None => return Err(AuthError::Missing(provider)),
            Some(Credential::Oauth(stale)) => stale,
        };

        let client = self.client(provider)?;
        let tokens = match client.refresh(&current.refresh).await {
            Ok(tokens) => tokens,
            Err(err) => return self.adopt_peer_rotation(provider, &current, err).await,
        };
        // `authorized_at` 记的是交互式授权时刻，刷新不该改写它。
        let next = Credential::Oauth(tokens.into_credential(current.authorized_at));

        let store = Arc::clone(&self.store);
        let expected = current.refresh.clone();
        let outcome = blocking(move || store.compare_and_swap(provider, &expected, next)).await?;

        match outcome {
            SwapOutcome::Stored(Credential::Oauth(saved)) => Ok(oauth_access(&saved)),
            // 别的进程抢先轮换：用它写下的凭据，别拿自己那份去覆盖。
            SwapOutcome::Superseded(Credential::Oauth(peer)) => {
                tracing::debug!(%provider, "另一个进程已经刷新过，采纳其结果");
                Ok(oauth_access(&peer))
            }
            SwapOutcome::Superseded(Credential::ApiKey(key))
            | SwapOutcome::Stored(Credential::ApiKey(key)) => Ok(api_key_access(key.key)),
            // 刷新途中被登出：不能让旧凭据复活。
            SwapOutcome::Vacated => Err(AuthError::Missing(provider)),
        }
    }

    /// 刷新失败后的最后一道防线。
    ///
    /// 轮换型 refresh token 下，两个进程拿同一个 token 并发刷新时输家必定失败，
    /// 而失败发生在 CAS 之前，CAS 兜不住。此时只要存储里的 token 已经换成别的
    /// 且仍可用，就说明赢家已经写好了，直接采纳。否则如实抛出原始错误。
    async fn adopt_peer_rotation(
        &self,
        provider: ProviderId,
        observed: &OAuthCredential,
        err: AuthError,
    ) -> Result<Access, AuthError> {
        match self.load(provider).await? {
            Some(Credential::Oauth(peer))
                if peer.refresh != observed.refresh && peer.is_fresh() =>
            {
                tracing::debug!(%provider, "本进程刷新失败，但对端已轮换出可用凭据，采纳之");
                Ok(oauth_access(&peer))
            }
            Some(Credential::ApiKey(key)) => Ok(api_key_access(key.key)),
            _ => Err(err),
        }
    }
}

fn api_key_access(token: String) -> Access {
    Access {
        token,
        kind: AccessKind::ApiKey,
        account_id: None,
    }
}

fn oauth_access(credential: &OAuthCredential) -> Access {
    Access {
        token: credential.access.clone(),
        kind: AccessKind::OAuth,
        account_id: credential.account_id.clone(),
    }
}

fn env_access(provider: ProviderId) -> Option<Access> {
    provider
        .bearer_env()
        .iter()
        .find_map(|name| std::env::var(name).ok())
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .map(api_key_access)
}

/// 把阻塞的存储操作挪出 async 执行器线程。
async fn blocking<T, F>(work: F) -> Result<T, AuthError>
where
    F: FnOnce() -> Result<T, AuthError> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|err| AuthError::Io(std::io::Error::other(err)))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::credential::{OAuthTokens, REFRESH_SKEW_MS};
    use crate::auth::store::MemoryCredentialStore;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[derive(Debug)]
    struct CountingOAuth {
        provider: ProviderId,
        calls: AtomicU32,
    }

    impl CountingOAuth {
        fn new(provider: ProviderId) -> Arc<Self> {
            Arc::new(Self {
                provider,
                calls: AtomicU32::new(0),
            })
        }
    }

    #[async_trait]
    impl OAuthClient for CountingOAuth {
        fn provider(&self) -> ProviderId {
            self.provider
        }

        async fn login(&self, _prompt: &dyn LoginPrompt) -> Result<OAuthTokens, AuthError> {
            Ok(OAuthTokens {
                access: "logged-in".to_owned(),
                refresh: "rt".to_owned(),
                expires: now_ms() + 3_600_000,
                account_id: Some("acct".to_owned()),
                email: None,
                plan: None,
            })
        }

        async fn refresh(&self, refresh_token: &str) -> Result<OAuthTokens, AuthError> {
            let round = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            // 让出一次，制造并发窗口。
            tokio::task::yield_now().await;
            Ok(OAuthTokens {
                access: format!("access-{round}"),
                refresh: format!("{refresh_token}-{round}"),
                expires: now_ms() + 3_600_000,
                account_id: Some("acct".to_owned()),
                email: None,
                plan: None,
            })
        }
    }

    /// 模拟跨进程竞争的输家：上游那一刻赢家已经轮换完并写好了存储，
    /// 我们这一支拿到 `invalid_grant`。
    #[derive(Debug)]
    struct LosingRacerOAuth {
        store: Arc<MemoryCredentialStore>,
    }

    #[async_trait]
    impl OAuthClient for LosingRacerOAuth {
        fn provider(&self) -> ProviderId {
            ProviderId::Anthropic
        }

        async fn login(&self, _prompt: &dyn LoginPrompt) -> Result<OAuthTokens, AuthError> {
            Err(AuthError::Missing(ProviderId::Anthropic))
        }

        async fn refresh(&self, _refresh_token: &str) -> Result<OAuthTokens, AuthError> {
            self.store
                .save(
                    ProviderId::Anthropic,
                    oauth_credential("winner-rt", now_ms() + 10 * REFRESH_SKEW_MS),
                )
                .expect("赢家写入");
            Err(AuthError::Denied {
                provider: ProviderId::Anthropic,
                error: "invalid_grant".to_owned(),
                description: None,
            })
        }
    }

    #[derive(Debug)]
    struct FailingOAuth;

    #[async_trait]
    impl OAuthClient for FailingOAuth {
        fn provider(&self) -> ProviderId {
            ProviderId::Anthropic
        }

        async fn login(&self, _prompt: &dyn LoginPrompt) -> Result<OAuthTokens, AuthError> {
            Err(AuthError::Denied {
                provider: ProviderId::Anthropic,
                error: "access_denied".to_owned(),
                description: None,
            })
        }

        async fn refresh(&self, _refresh_token: &str) -> Result<OAuthTokens, AuthError> {
            Err(AuthError::Denied {
                provider: ProviderId::Anthropic,
                error: "invalid_grant".to_owned(),
                description: None,
            })
        }
    }

    #[derive(Debug)]
    struct SilentPrompt;

    #[async_trait]
    impl LoginPrompt for SilentPrompt {
        fn authorization_url(&self, _provider: ProviderId, _url: &str) {}
        fn device_code(&self, _provider: ProviderId, _uri: &str, _code: &str) {}
    }

    /// `as` 在本仓库是禁用的（见 workspace lints），trait 对象转换走这两个辅助函数。
    fn erase_store(store: &Arc<MemoryCredentialStore>) -> Arc<dyn CredentialStore> {
        let concrete: Arc<MemoryCredentialStore> = Arc::clone(store);
        concrete
    }

    fn erase_client<T: OAuthClient>(client: &Arc<T>) -> Arc<dyn OAuthClient> {
        let concrete: Arc<T> = Arc::clone(client);
        concrete
    }

    fn oauth_credential(refresh: &str, expires: u64) -> Credential {
        Credential::Oauth(OAuthCredential {
            access: format!("access-for-{refresh}"),
            refresh: refresh.to_owned(),
            expires,
            account_id: Some("acct".to_owned()),
            email: None,
            plan: None,
            authorized_at: Some(1),
        })
    }

    fn store_with(entries: &[(ProviderId, Credential)]) -> Arc<MemoryCredentialStore> {
        let store = Arc::new(MemoryCredentialStore::new());
        for (provider, credential) in entries {
            store.save(*provider, credential.clone()).expect("预置凭据");
        }
        store
    }

    #[tokio::test]
    async fn api_key_credentials_are_returned_verbatim() {
        let store = store_with(&[(
            ProviderId::OpenAi,
            Credential::ApiKey(ApiKeyCredential {
                key: "sk-test".to_owned(),
            }),
        )]);
        let auth = AuthStore::bare(store);

        let access = auth.access(ProviderId::OpenAi).await.expect("解析凭据");
        assert_eq!(access.token, "sk-test");
        assert_eq!(access.kind, AccessKind::ApiKey);
    }

    #[tokio::test]
    async fn fresh_oauth_tokens_are_used_without_refreshing() {
        let store = store_with(&[(
            ProviderId::Anthropic,
            oauth_credential("rt", now_ms() + 10 * REFRESH_SKEW_MS),
        )]);
        let client = CountingOAuth::new(ProviderId::Anthropic);
        let auth = AuthStore::bare(store).register(erase_client(&client));

        let access = auth.access(ProviderId::Anthropic).await.expect("解析凭据");
        assert_eq!(access.token, "access-for-rt");
        assert_eq!(access.kind, AccessKind::OAuth);
        assert_eq!(access.account_id.as_deref(), Some("acct"));
        assert_eq!(client.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn tokens_inside_the_skew_window_are_refreshed_and_persisted() {
        let store = store_with(&[(
            ProviderId::Anthropic,
            oauth_credential("rt", now_ms() + REFRESH_SKEW_MS / 2),
        )]);
        let client = CountingOAuth::new(ProviderId::Anthropic);
        let auth = AuthStore::bare(erase_store(&store)).register(erase_client(&client));

        let access = auth.access(ProviderId::Anthropic).await.expect("刷新");
        assert_eq!(access.token, "access-1");
        assert_eq!(client.calls.load(Ordering::SeqCst), 1);

        match store.load(ProviderId::Anthropic).expect("load") {
            Some(Credential::Oauth(saved)) => {
                assert_eq!(saved.refresh, "rt-1");
                // 刷新不该改写交互式授权时刻。
                assert_eq!(saved.authorized_at, Some(1));
            }
            other => panic!("期望 OAuth 凭据，实际 {other:?}"),
        }
    }

    #[tokio::test]
    async fn concurrent_callers_refresh_only_once() {
        let store = store_with(&[(
            ProviderId::OpenAiCodex,
            oauth_credential("rt", now_ms() + REFRESH_SKEW_MS / 2),
        )]);
        let client = CountingOAuth::new(ProviderId::OpenAiCodex);
        let auth = Arc::new(AuthStore::bare(store).register(erase_client(&client)));

        let mut handles = Vec::new();
        for _ in 0..8_u8 {
            let auth = Arc::clone(&auth);
            handles.push(tokio::spawn(async move {
                auth.access(ProviderId::OpenAiCodex).await
            }));
        }
        for handle in handles {
            let access = handle.await.expect("join").expect("解析凭据");
            assert_eq!(access.token, "access-1");
        }
        assert_eq!(client.calls.load(Ordering::SeqCst), 1, "刷新被重复触发");
    }

    #[tokio::test]
    async fn a_peer_refresh_that_already_landed_is_used_as_is() {
        let store = Arc::new(MemoryCredentialStore::new());
        // 拿到锁时存储里已经是别的进程刷出来的新凭据。
        store
            .save(
                ProviderId::Anthropic,
                oauth_credential("rotated-by-peer", now_ms() + 10 * REFRESH_SKEW_MS),
            )
            .expect("peer 写入");

        let client = CountingOAuth::new(ProviderId::Anthropic);
        let auth = AuthStore::bare(erase_store(&store)).register(erase_client(&client));

        let access = auth.refresh(ProviderId::Anthropic).await.expect("解析凭据");
        assert_eq!(access.token, "access-for-rotated-by-peer");
        assert_eq!(client.calls.load(Ordering::SeqCst), 0, "不该再刷一次");
    }

    #[tokio::test]
    async fn refresh_uses_the_token_read_under_the_lock_not_a_stale_snapshot() {
        let store = store_with(&[(
            ProviderId::Anthropic,
            oauth_credential("current-rt", now_ms() + REFRESH_SKEW_MS / 2),
        )]);
        let client = CountingOAuth::new(ProviderId::Anthropic);
        let auth = AuthStore::bare(erase_store(&store)).register(erase_client(&client));

        drop(auth.refresh(ProviderId::Anthropic).await.expect("刷新"));
        match store.load(ProviderId::Anthropic).expect("load") {
            // `CountingOAuth` 把传入的 refresh token 拼进新值，据此断言用的是哪一个。
            Some(Credential::Oauth(saved)) => assert_eq!(saved.refresh, "current-rt-1"),
            other => panic!("期望 OAuth 凭据，实际 {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_losing_racer_adopts_the_winner_instead_of_reporting_invalid_grant() {
        let store = Arc::new(MemoryCredentialStore::new());
        store
            .save(
                ProviderId::Anthropic,
                oauth_credential("shared-rt", now_ms()),
            )
            .expect("预置凭据");
        // 上游刷新期间赢家把轮换结果写了进来，我们这一支随后拿到 invalid_grant。
        let client = Arc::new(LosingRacerOAuth {
            store: Arc::clone(&store),
        });
        let auth = AuthStore::bare(erase_store(&store)).register(erase_client(&client));

        let access = auth
            .refresh(ProviderId::Anthropic)
            .await
            .expect("应当采纳赢家结果");
        assert_eq!(access.token, "access-for-winner-rt");
    }

    #[tokio::test]
    async fn logout_during_refresh_does_not_resurrect_the_credential() {
        let store = Arc::new(MemoryCredentialStore::new());
        let client = CountingOAuth::new(ProviderId::Anthropic);
        let auth = AuthStore::bare(erase_store(&store)).register(erase_client(&client));

        // 槽位空着就等于已登出。
        let err = auth
            .refresh(ProviderId::Anthropic)
            .await
            .expect_err("应当报缺凭据");
        assert!(matches!(err, AuthError::Missing(ProviderId::Anthropic)));
        assert_eq!(store.load(ProviderId::Anthropic).expect("load"), None);
    }

    #[tokio::test]
    async fn refresh_failures_propagate_instead_of_silently_using_a_dead_token() {
        let store = store_with(&[(ProviderId::Anthropic, oauth_credential("dead", now_ms()))]);
        let auth = AuthStore::bare(store).register(Arc::new(FailingOAuth));

        let err = auth
            .access(ProviderId::Anthropic)
            .await
            .expect_err("应当失败");
        assert!(matches!(err, AuthError::Denied { error, .. } if error == "invalid_grant"));
    }

    #[tokio::test]
    async fn missing_credentials_report_which_provider_needs_login() {
        let auth = AuthStore::bare(Arc::new(MemoryCredentialStore::new()));
        let err = auth
            .access(ProviderId::OpenAiCodex)
            .await
            .expect_err("应当失败");
        assert!(matches!(err, AuthError::Missing(ProviderId::OpenAiCodex)));
    }

    #[tokio::test]
    async fn login_persists_the_credential_and_stamps_authorized_at() {
        let store = Arc::new(MemoryCredentialStore::new());
        let auth =
            AuthStore::bare(erase_store(&store)).register(CountingOAuth::new(ProviderId::XaiOAuth));

        let access = auth
            .login(ProviderId::XaiOAuth, &SilentPrompt)
            .await
            .expect("登录");
        assert_eq!(access.token, "logged-in");
        match store.load(ProviderId::XaiOAuth).expect("load") {
            Some(Credential::Oauth(saved)) => assert!(saved.authorized_at.is_some()),
            other => panic!("期望 OAuth 凭据，实际 {other:?}"),
        }
    }

    #[tokio::test]
    async fn logout_removes_the_credential() {
        let store = store_with(&[(
            ProviderId::Xai,
            Credential::ApiKey(ApiKeyCredential {
                key: "k".to_owned(),
            }),
        )]);
        let auth = AuthStore::bare(erase_store(&store));

        auth.logout(ProviderId::Xai).await.expect("登出");
        assert_eq!(store.load(ProviderId::Xai).expect("load"), None);
    }

    #[tokio::test]
    async fn login_without_a_registered_client_is_an_explicit_error() {
        let auth = AuthStore::bare(Arc::new(MemoryCredentialStore::new()));
        let err = auth
            .login(ProviderId::Anthropic, &SilentPrompt)
            .await
            .expect_err("应当失败");
        assert!(matches!(err, AuthError::RefreshFailed { .. }));
    }
}
