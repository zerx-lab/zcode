//! 凭据的持久化后端。
//!
//! 默认实现 [`FileCredentialStore`] 把所有提供商的凭据放在一个 JSON 文件里，并用
//! 同目录的锁文件做**跨进程**互斥。
//!
//! # 为什么必须有锁
//!
//! OAuth 刷新会轮换 refresh token（Codex 每次刷新都换）。多个 zcode 进程并发刷新
//! 时，"整文件读—改—写" 会丢更新：A 读到 `{anthropic, openai}`，B 也读到，A 写回、
//! B 再写回，A 的新 token 被覆盖，对应账号的 refresh token 就永久作废了。原子 rename
//! 只能防半写，防不住丢更新，所以读-改-写整段必须在排他锁内完成。
//!
//! 与之配套的是 [`CredentialStore::compare_and_swap`]：刷新前记下 refresh token，
//! 网络往返后在锁内比对，发现别的进程已经轮换过就采纳对方结果，丢弃自己的。

use std::collections::BTreeMap;
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::auth::credential::Credential;
use crate::error::AuthError;
use crate::types::ProviderId;

/// 覆盖凭据文件位置的环境变量。
pub const AUTH_FILE_ENV: &str = "ZCODE_AUTH_FILE";

/// 凭据文件相对用户主目录的默认位置。
const DEFAULT_RELATIVE_PATH: &str = ".zcode/auth.json";

/// 磁盘格式版本；结构不兼容变更时递增。
const FORMAT_VERSION: u32 = 1;

/// 凭据文件的顶层结构。
#[derive(Debug, Default, Serialize, Deserialize)]
struct CredentialFile {
    version: u32,
    #[serde(default)]
    credentials: BTreeMap<String, Credential>,
}

/// 一次 [`CredentialStore::compare_and_swap`] 的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SwapOutcome {
    /// 槽位仍是期望的那份 OAuth 凭据，写入成功。
    Stored(Credential),
    /// 槽位已被别的写者改成另一份凭据（并发刷新，或用户重新登录 / 换成 API key）。
    ///
    /// **没有写入**。调用方应改用返回的这份，而不是自己那份。
    Superseded(Credential),
    /// 槽位已被清空（并发登出）。**没有写入**，不得让旧凭据复活。
    Vacated,
}

impl SwapOutcome {
    /// 取出最终生效的凭据；槽位被清空时返回 `None`。
    #[must_use]
    pub fn into_credential(self) -> Option<Credential> {
        match self {
            Self::Stored(credential) | Self::Superseded(credential) => Some(credential),
            Self::Vacated => None,
        }
    }

    /// 本次调用是否真的落盘了 `next`。
    #[must_use]
    pub fn is_stored(&self) -> bool {
        matches!(self, Self::Stored(_))
    }
}

/// 凭据持久化后端。
///
/// 所有方法都是阻塞的，调用方负责放进 [`tokio::task::spawn_blocking`]。
pub trait CredentialStore: fmt::Debug + Send + Sync + 'static {
    /// 读一条凭据。
    fn load(&self, provider: ProviderId) -> Result<Option<Credential>, AuthError>;

    /// 写一条凭据，覆盖同 provider 的旧值。
    fn save(&self, provider: ProviderId, credential: Credential) -> Result<(), AuthError>;

    /// 删除一条凭据。
    fn remove(&self, provider: ProviderId) -> Result<(), AuthError>;

    /// 在跨进程互斥下比对并写入。
    ///
    /// 只有当槽位**仍是**一份 OAuth 凭据且其 `refresh` 等于 `expected_refresh` 时才写入。
    /// 槽位被清空（登出）返回 [`SwapOutcome::Vacated`]，被换成别的凭据（并发刷新、
    /// 重新登录、改用 API key）返回 [`SwapOutcome::Superseded`]，两者都**不写入**——
    /// 否则一次在途刷新就能让已登出/已替换的旧凭据复活。
    fn compare_and_swap(
        &self,
        provider: ProviderId,
        expected_refresh: &str,
        next: Credential,
    ) -> Result<SwapOutcome, AuthError>;
}

/// 基于单个 JSON 文件 + 锁文件的默认实现。
#[derive(Debug, Clone)]
pub struct FileCredentialStore {
    path: PathBuf,
    lock_path: PathBuf,
}

impl FileCredentialStore {
    /// 用指定路径建立存储；锁文件是同名加 `.lock`。
    #[must_use]
    pub fn at(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let mut lock_path = path.clone().into_os_string();
        lock_path.push(".lock");
        Self {
            path,
            lock_path: PathBuf::from(lock_path),
        }
    }

    /// 按 `ZCODE_AUTH_FILE` 或 `~/.zcode/auth.json` 定位凭据文件。
    pub fn discover() -> Result<Self, AuthError> {
        if let Some(path) = std::env::var_os(AUTH_FILE_ENV).filter(|value| !value.is_empty()) {
            return Ok(Self::at(PathBuf::from(path)));
        }
        // 用 std 而非 `dirs`：后者依赖 MPL-2.0 的 option-ext，被 deny.toml 的
        // 许可白名单挡住；`std::env::home_dir` 自 1.85 起在 Windows 上行为也已修正。
        let home = std::env::home_dir().ok_or_else(|| {
            AuthError::Io(std::io::Error::other(
                "无法定位用户主目录，请设置 ZCODE_AUTH_FILE",
            ))
        })?;
        Ok(Self::at(home.join(DEFAULT_RELATIVE_PATH)))
    }

    /// 凭据文件路径。
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn with_lock<T>(
        &self,
        act: impl FnOnce(&mut CredentialFile) -> Result<T, AuthError>,
    ) -> Result<T, AuthError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
            harden_directory(parent);
        }
        let lock = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&self.lock_path)?;
        // std 的 `File::lock` 在 Unix 走 flock、在 Windows 走 LockFileEx，语义一致。
        lock.lock()?;
        let result = self.locked_section(act);
        // 解锁失败只影响后续争用，不该淹没业务结果。
        if let Err(err) = lock.unlock() {
            tracing::warn!(error = %err, "释放凭据锁失败");
        }
        result
    }

    fn locked_section<T>(
        &self,
        act: impl FnOnce(&mut CredentialFile) -> Result<T, AuthError>,
    ) -> Result<T, AuthError> {
        let mut file = self.read_file()?;
        let before = serde_json::to_string(&file.credentials).unwrap_or_default();
        let outcome = act(&mut file)?;
        let after = serde_json::to_string(&file.credentials).unwrap_or_default();
        if before != after {
            self.write_file(&file)?;
        }
        Ok(outcome)
    }

    fn read_file(&self) -> Result<CredentialFile, AuthError> {
        let mut raw = String::new();
        match File::open(&self.path) {
            Ok(mut handle) => {
                handle.read_to_string(&mut raw)?;
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(CredentialFile {
                    version: FORMAT_VERSION,
                    credentials: BTreeMap::new(),
                });
            }
            Err(err) => return Err(AuthError::Io(err)),
        }
        if raw.trim().is_empty() {
            return Ok(CredentialFile {
                version: FORMAT_VERSION,
                credentials: BTreeMap::new(),
            });
        }
        serde_json::from_str(&raw).map_err(AuthError::Corrupt)
    }

    fn write_file(&self, file: &CredentialFile) -> Result<(), AuthError> {
        let body = serde_json::to_vec_pretty(&CredentialFile {
            version: FORMAT_VERSION,
            credentials: file.credentials.clone(),
        })
        .map_err(AuthError::Corrupt)?;

        let mut temp_path = self.path.clone().into_os_string();
        temp_path.push(".tmp");
        let temp_path = PathBuf::from(temp_path);

        {
            let mut handle = create_private_file(&temp_path)?;
            handle.write_all(&body)?;
            handle.sync_all()?;
        }
        std::fs::rename(&temp_path, &self.path)?;
        Ok(())
    }
}

/// 判定一次 CAS 是否可以写入。
///
/// 返回 `None` 表示槽位仍是期望的那份 OAuth 凭据，可以写；返回 `Some(outcome)`
/// 表示必须放弃写入并把 `outcome` 交回调用方。
fn evaluate_swap(current: Option<&Credential>, expected_refresh: &str) -> Option<SwapOutcome> {
    match current {
        // 并发登出：写回去等于让已删除的凭据复活。
        None => Some(SwapOutcome::Vacated),
        Some(Credential::Oauth(stored)) if stored.refresh == expected_refresh => None,
        // 并发刷新、重新登录、或改用了 API key——一律以存储里的为准。
        Some(other) => Some(SwapOutcome::Superseded(other.clone())),
    }
}

#[cfg(unix)]
fn create_private_file(path: &Path) -> Result<File, AuthError> {
    use std::os::unix::fs::OpenOptionsExt as _;
    Ok(OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?)
}

#[cfg(not(unix))]
fn create_private_file(path: &Path) -> Result<File, AuthError> {
    // Windows 上 `%USERPROFILE%` 默认只有属主可写，没有等价于 0600 的可移植设置。
    Ok(OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)?)
}

#[cfg(unix)]
fn harden_directory(path: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    if let Err(err) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)) {
        tracing::warn!(path = %path.display(), error = %err, "无法收紧凭据目录权限");
    }
}

#[cfg(not(unix))]
fn harden_directory(_path: &Path) {}

impl CredentialStore for FileCredentialStore {
    fn load(&self, provider: ProviderId) -> Result<Option<Credential>, AuthError> {
        self.with_lock(|file| Ok(file.credentials.get(provider.as_str()).cloned()))
    }

    fn save(&self, provider: ProviderId, credential: Credential) -> Result<(), AuthError> {
        self.with_lock(|file| {
            drop(
                file.credentials
                    .insert(provider.as_str().to_owned(), credential),
            );
            Ok(())
        })
    }

    fn remove(&self, provider: ProviderId) -> Result<(), AuthError> {
        self.with_lock(|file| {
            drop(file.credentials.remove(provider.as_str()));
            Ok(())
        })
    }

    fn compare_and_swap(
        &self,
        provider: ProviderId,
        expected_refresh: &str,
        next: Credential,
    ) -> Result<SwapOutcome, AuthError> {
        self.with_lock(|file| {
            let current = file.credentials.get(provider.as_str());
            if let Some(outcome) = evaluate_swap(current, expected_refresh) {
                return Ok(outcome);
            }
            drop(
                file.credentials
                    .insert(provider.as_str().to_owned(), next.clone()),
            );
            Ok(SwapOutcome::Stored(next))
        })
    }
}

/// 进程内存储，仅用于测试与嵌入式场景。
#[derive(Debug, Default)]
pub struct MemoryCredentialStore {
    entries: std::sync::Mutex<BTreeMap<String, Credential>>,
}

impl MemoryCredentialStore {
    /// 新建空存储。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn entries(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, Credential>> {
        // 本类型只在锁内做 map 操作，不会 panic，因此中毒锁可以安全接管。
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl CredentialStore for MemoryCredentialStore {
    fn load(&self, provider: ProviderId) -> Result<Option<Credential>, AuthError> {
        Ok(self.entries().get(provider.as_str()).cloned())
    }

    fn save(&self, provider: ProviderId, credential: Credential) -> Result<(), AuthError> {
        drop(
            self.entries()
                .insert(provider.as_str().to_owned(), credential),
        );
        Ok(())
    }

    fn remove(&self, provider: ProviderId) -> Result<(), AuthError> {
        drop(self.entries().remove(provider.as_str()));
        Ok(())
    }

    fn compare_and_swap(
        &self,
        provider: ProviderId,
        expected_refresh: &str,
        next: Credential,
    ) -> Result<SwapOutcome, AuthError> {
        let mut entries = self.entries();
        if let Some(outcome) = evaluate_swap(entries.get(provider.as_str()), expected_refresh) {
            return Ok(outcome);
        }
        drop(entries.insert(provider.as_str().to_owned(), next.clone()));
        Ok(SwapOutcome::Stored(next))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::credential::{ApiKeyCredential, OAuthCredential};

    fn oauth(refresh: &str) -> Credential {
        Credential::Oauth(OAuthCredential {
            access: format!("access-for-{refresh}"),
            refresh: refresh.to_owned(),
            expires: 1_000,
            account_id: None,
            email: None,
            plan: None,
            authorized_at: None,
        })
    }

    fn temp_store() -> (tempfile::TempDir, FileCredentialStore) {
        let dir = tempfile::tempdir().expect("创建临时目录");
        let store = FileCredentialStore::at(dir.path().join("auth.json"));
        (dir, store)
    }

    #[test]
    fn missing_file_reads_as_empty() {
        let (_dir, store) = temp_store();
        assert_eq!(store.load(ProviderId::Anthropic).expect("load"), None);
    }

    #[test]
    fn save_then_load_roundtrips_across_store_handles() {
        let (dir, store) = temp_store();
        store.save(ProviderId::OpenAi, oauth("r1")).expect("save");

        let reopened = FileCredentialStore::at(dir.path().join("auth.json"));
        assert_eq!(
            reopened.load(ProviderId::OpenAi).expect("load"),
            Some(oauth("r1"))
        );
    }

    #[test]
    fn providers_do_not_clobber_each_other() {
        let (_dir, store) = temp_store();
        store
            .save(ProviderId::OpenAi, oauth("r-openai"))
            .expect("save");
        store
            .save(
                ProviderId::Anthropic,
                Credential::ApiKey(ApiKeyCredential { key: "k".into() }),
            )
            .expect("save");

        assert_eq!(
            store.load(ProviderId::OpenAi).expect("load"),
            Some(oauth("r-openai"))
        );
        assert!(matches!(
            store.load(ProviderId::Anthropic).expect("load"),
            Some(Credential::ApiKey(_))
        ));
    }

    #[test]
    fn remove_deletes_only_the_named_provider() {
        let (_dir, store) = temp_store();
        store.save(ProviderId::OpenAi, oauth("a")).expect("save");
        store.save(ProviderId::Xai, oauth("b")).expect("save");

        store.remove(ProviderId::OpenAi).expect("remove");
        assert_eq!(store.load(ProviderId::OpenAi).expect("load"), None);
        assert_eq!(store.load(ProviderId::Xai).expect("load"), Some(oauth("b")));
    }

    #[test]
    fn swap_writes_when_the_refresh_token_still_matches() {
        let (_dir, store) = temp_store();
        store
            .save(ProviderId::OpenAiCodex, oauth("r1"))
            .expect("save");

        let outcome = store
            .compare_and_swap(ProviderId::OpenAiCodex, "r1", oauth("r2"))
            .expect("swap");
        assert_eq!(outcome, SwapOutcome::Stored(oauth("r2")));
        assert_eq!(
            store.load(ProviderId::OpenAiCodex).expect("load"),
            Some(oauth("r2"))
        );
    }

    #[test]
    fn swap_yields_to_a_peer_that_already_rotated() {
        let (_dir, store) = temp_store();
        store
            .save(ProviderId::OpenAiCodex, oauth("rotated-by-peer"))
            .expect("save");

        let outcome = store
            .compare_and_swap(ProviderId::OpenAiCodex, "stale", oauth("mine"))
            .expect("swap");
        assert_eq!(outcome, SwapOutcome::Superseded(oauth("rotated-by-peer")));
        // 关键不变量：对方的写入没有被覆盖。
        assert_eq!(
            store.load(ProviderId::OpenAiCodex).expect("load"),
            Some(oauth("rotated-by-peer"))
        );
    }

    #[test]
    fn swap_into_an_empty_slot_aborts_instead_of_resurrecting() {
        let (_dir, store) = temp_store();
        let outcome = store
            .compare_and_swap(ProviderId::Xai, "whatever", oauth("fresh"))
            .expect("swap");
        assert_eq!(outcome, SwapOutcome::Vacated);
        assert_eq!(store.load(ProviderId::Xai).expect("load"), None);
    }

    #[test]
    fn swap_aborts_when_the_slot_became_an_api_key() {
        let (_dir, store) = temp_store();
        let key = Credential::ApiKey(ApiKeyCredential {
            key: "sk-new".to_owned(),
        });
        store
            .save(ProviderId::Anthropic, key.clone())
            .expect("save");

        let outcome = store
            .compare_and_swap(ProviderId::Anthropic, "r1", oauth("stale"))
            .expect("swap");
        assert_eq!(outcome, SwapOutcome::Superseded(key.clone()));
        assert_eq!(store.load(ProviderId::Anthropic).expect("load"), Some(key));
    }

    #[test]
    fn corrupt_file_surfaces_as_corrupt_not_as_empty() {
        let (dir, store) = temp_store();
        std::fs::write(dir.path().join("auth.json"), "{ not json").expect("write");
        assert!(matches!(
            store.load(ProviderId::Xai),
            Err(AuthError::Corrupt(_))
        ));
    }

    #[test]
    fn concurrent_writers_do_not_lose_updates() {
        let (dir, _store) = temp_store();
        let path = dir.path().join("auth.json");
        let providers = [
            ProviderId::Anthropic,
            ProviderId::OpenAi,
            ProviderId::OpenAiCodex,
            ProviderId::Xai,
        ];

        std::thread::scope(|scope| {
            for provider in providers {
                let path = path.clone();
                drop(scope.spawn(move || {
                    let store = FileCredentialStore::at(path);
                    for round in 0..20 {
                        store
                            .save(provider, oauth(&format!("{provider}-{round}")))
                            .expect("save");
                    }
                }));
            }
        });

        let store = FileCredentialStore::at(path);
        for provider in providers {
            assert_eq!(
                store.load(provider).expect("load"),
                Some(oauth(&format!("{provider}-19"))),
                "{provider} 的最后一次写入被别的线程覆盖了"
            );
        }
    }

    #[test]
    fn memory_store_mirrors_file_store_swap_semantics() {
        let store = MemoryCredentialStore::new();
        store.save(ProviderId::Xai, oauth("r1")).expect("save");
        assert_eq!(
            store
                .compare_and_swap(ProviderId::Xai, "r1", oauth("r2"))
                .expect("swap"),
            SwapOutcome::Stored(oauth("r2"))
        );
        assert_eq!(
            store
                .compare_and_swap(ProviderId::Xai, "r1", oauth("r3"))
                .expect("swap"),
            SwapOutcome::Superseded(oauth("r2"))
        );
        store.remove(ProviderId::Xai).expect("remove");
        assert_eq!(
            store
                .compare_and_swap(ProviderId::Xai, "r2", oauth("r4"))
                .expect("swap"),
            SwapOutcome::Vacated
        );
    }
}
