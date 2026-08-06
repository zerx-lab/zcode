//! 运行时模型发现结果的 `SQLite` 落盘缓存。
//!
//! ## pragma 顺序为什么固定
//!
//! `busy_timeout` 必须在任何取锁语句之前设置：SQLite 只在“进入忙等待循环”时读取
//! `busy_timeout` 的当前值，若在它之前先执行了会隐式加锁的语句（例如首次切到
//! `journal_mode=WAL` 要写 WAL 头），那条语句仍按默认 0 超时立即失败，多进程并发
//! 打开同一份缓存时会随机报 `database is locked`（移植自上游 oh-my-pi 的已知问题
//! `#2421`）。因此固定顺序：`busy_timeout=3000` → `journal_mode=WAL` →
//! `synchronous=NORMAL` → `secure_delete=ON`。
//!
//! `secure_delete=ON` 会让 `SQLite` 在删除/更新行时把旧页清零，而不是把内容留在
//! 空闲页里当垃圾数据，这有真实的写放大成本。但这张表存的是模型发现结果的缓存
//! ——一旦上层撤销或轮换了某个自定义 provider 的凭据，旧 `endpoint_fingerprint`
//! 对应的行必须真正从磁盘上消失，而不是仍能被磁盘取证工具捞出来（移植自上游
//! `#5780` 的取舍）。用这点写入开销换“凭据变更后磁盘不留痕迹”。
//!
//! ## headers 为什么根本不在这个模块的类型里出现
//!
//! 自定义 provider 可以用任意 header 名承载凭据（不只是 `Authorization`），任何
//! 基于名字的黑白名单过滤都可能漏掉一种约定。与其在写入路径上做过滤，不如从
//! 类型系统上根除：本模块公开的每一个函数都不接受、不存储、不返回任何 header
//! 内容。落盘的唯一“凭据相关”输入是调用方预先算好的不透明指纹字符串
//! （`endpoint_fingerprint`），缓存本身对它的构成一无所知。
//!
//! ## 失效通道只有一条
//!
//! 缓存主键是 `(provider_id, endpoint_fingerprint)`；一行是否仍然新鲜只看两个
//! 维度：`SCHEMA_VERSION`（表结构版本，[`ModelCache::open`]/[`ModelCache::in_memory`]
//! 建表后立即清掉不匹配的行）与 `static_fingerprint`（调用方对该 provider“形状”
//! 算出的哈希，[`ModelCache::load`] 里比对，不匹配则视为未命中并顺手删除该行）。
//! 不额外维护第三条失效路径——上游历史上并行长出过 schema version、fingerprint
//! version、手工 bump 的 cache-provider-id 三套并存的失效机制，其中 schema
//! version 的迁移语句还把旧版本值原地 `UPDATE` 成新版本号，悄悄吞掉了此后每一次
//! 本该发生的失效（这正是 `SCHEMA_VERSION` 的文档里强调“只能删行或加列，绝不能
//! 原地改写版本号”的原因）。

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, params};

use crate::spec::ModelSpec;

/// 覆盖默认缓存文件位置的环境变量。
pub const MODEL_CACHE_ENV: &str = "ZCODE_MODEL_CACHE";

/// 缓存文件相对用户主目录的默认位置，与 `zcode-ai` 的 `.zcode/auth.json` 同级。
const DEFAULT_RELATIVE_PATH: &str = ".zcode/models-cache.db";

/// 缓存表结构版本。**改结构就 +1**，并在这里追加一行失效理由；`open`/`in_memory`
/// 建表后会立即 `DELETE` 掉 `schema_version` 不等于当前值的行。
///
/// 迁移只允许两种操作：删除不匹配的行，或者新增列。绝不允许写
/// `UPDATE model_cache SET schema_version = <new>` 这类把旧版本原地升级成新版本
/// 的语句——那等于让下一次结构变更悄悄跳过失效，历史上游正因此吃过亏（见模块
/// 文档「失效通道只有一条」）。
///
/// - v1：首版。`(provider_id, endpoint_fingerprint)` 主键，`static_fingerprint`
///   做内容级失效判据。
const SCHEMA_VERSION: i64 = 1;

/// 运行时模型发现结果的 `SQLite` 落盘缓存。
///
/// 不缓存 header：类型上就没有接受 header 的入口，调用方传入的是预先算好的
/// 不透明指纹字符串（`endpoint_fingerprint` / `static_fingerprint`）。
pub struct ModelCache {
    conn: Connection,
}

impl std::fmt::Debug for ModelCache {
    /// 只打结构体名：`rusqlite::Connection` 没有 `Debug`，且句柄内部状态对调试无意义。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelCache").finish_non_exhaustive()
    }
}

/// 一次成功的 [`ModelCache::load`] 命中。
#[derive(Debug, Clone, PartialEq)]
pub struct CachedModels {
    /// 提供商 id。
    pub provider_id: Box<str>,
    /// 该提供商在这次抓取中得到的模型列表。
    pub models: Vec<ModelSpec>,
    /// 本行写入时刻。
    pub updated_at: SystemTime,
    /// 本轮拉取是否权威（真的拿到了远端应答）。非权威条目应使用更短的重试窗。
    pub authoritative: bool,
}

/// [`ModelCache`] 操作失败的原因。
#[derive(Debug, thiserror::Error)]
pub enum ModelCacheError {
    /// 底层 `SQLite` 操作失败。
    #[error("SQLite 操作失败: {source}")]
    Sqlite {
        /// 底层错误。
        #[from]
        source: rusqlite::Error,
    },
    /// 缓存行的 `models` 列序列化/反序列化失败。
    #[error("模型缓存的 JSON (反)序列化失败: {source}")]
    Json {
        /// 底层错误。
        #[from]
        source: serde_json::Error,
    },
    /// 缓存文件所在目录无法创建或访问。
    #[error("无法访问缓存文件 {path}: {source}")]
    Io {
        /// 出问题的路径。
        path: PathBuf,
        /// 底层错误。
        source: std::io::Error,
    },
}

impl ModelCache {
    /// 打开或创建缓存库；父目录不存在则创建。
    pub fn open(path: &Path) -> Result<Self, ModelCacheError> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent).map_err(|source| ModelCacheError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let conn = Connection::open(path)?;
        Self::from_connection(conn)
    }

    /// 纯内存库，测试与“用户禁用落盘”场景用。
    pub fn in_memory() -> Result<Self, ModelCacheError> {
        let conn = Connection::open_in_memory()?;
        Self::from_connection(conn)
    }

    /// 默认路径：`$ZCODE_MODEL_CACHE`，否则用户主目录下的
    /// `.zcode/models-cache.db`（与 `zcode-ai` 的 `.zcode/auth.json` 约定一致）。
    /// 找不到主目录且未设置环境变量时返回 `None`，由调用方决定退化为
    /// [`Self::in_memory`] 还是直接放弃缓存。
    #[must_use]
    pub fn default_path() -> Option<PathBuf> {
        if let Some(path) = std::env::var_os(MODEL_CACHE_ENV).filter(|value| !value.is_empty()) {
            return Some(PathBuf::from(path));
        }
        let home = std::env::home_dir()?;
        Some(home.join(DEFAULT_RELATIVE_PATH))
    }

    /// 建表、清理陈旧 schema 版本；`open`/`in_memory` 共用。
    fn from_connection(conn: Connection) -> Result<Self, ModelCacheError> {
        // pragma 顺序固定：busy_timeout 必须排在任何会隐式取锁的语句之前，
        // 完整理由见模块文档「pragma 顺序为什么固定」。
        conn.execute_batch(
            "PRAGMA busy_timeout = 3000;
             PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA secure_delete = ON;
             CREATE TABLE IF NOT EXISTS model_cache (
                 provider_id TEXT NOT NULL,
                 endpoint_fingerprint TEXT NOT NULL,
                 schema_version INTEGER NOT NULL,
                 static_fingerprint TEXT NOT NULL,
                 updated_at INTEGER NOT NULL,
                 authoritative INTEGER NOT NULL,
                 models TEXT NOT NULL,
                 PRIMARY KEY (provider_id, endpoint_fingerprint)
             );",
        )?;
        conn.execute(
            "DELETE FROM model_cache WHERE schema_version <> ?1",
            params![SCHEMA_VERSION],
        )?;
        Ok(Self { conn })
    }

    /// 读取一条缓存。`static_fingerprint` 与落盘值不一致（provider 的形状变了）
    /// 时视为未命中，顺手删除该行，返回 `Ok(None)`。
    pub fn load(
        &self,
        provider_id: &str,
        endpoint_fingerprint: &str,
        static_fingerprint: &str,
    ) -> Result<Option<CachedModels>, ModelCacheError> {
        type Row = (String, String, i64, i64, String);
        let row: Option<Row> = self
            .conn
            .query_row(
                "SELECT provider_id, static_fingerprint, updated_at, authoritative, models
                 FROM model_cache
                 WHERE provider_id = ?1 AND endpoint_fingerprint = ?2",
                params![provider_id, endpoint_fingerprint],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .optional()?;

        let Some((stored_provider_id, stored_fingerprint, updated_at, authoritative, models_json)) =
            row
        else {
            return Ok(None);
        };

        if stored_fingerprint != static_fingerprint {
            self.invalidate(provider_id, endpoint_fingerprint)?;
            return Ok(None);
        }

        let models: Vec<ModelSpec> = serde_json::from_str(&models_json)?;
        Ok(Some(CachedModels {
            provider_id: stored_provider_id.into_boxed_str(),
            models,
            updated_at: from_unix_seconds(updated_at),
            authoritative: authoritative != 0,
        }))
    }

    /// 写入一条缓存；`(provider_id, endpoint_fingerprint)` 主键冲突时整行覆盖
    /// （而非报错），因为重复抓取同一 endpoint 是正常的刷新场景。
    pub fn store(
        &self,
        provider_id: &str,
        endpoint_fingerprint: &str,
        static_fingerprint: &str,
        models: &[ModelSpec],
        authoritative: bool,
    ) -> Result<(), ModelCacheError> {
        let models_json = serde_json::to_string(models)?;
        self.conn.execute(
            "INSERT INTO model_cache
                 (provider_id, endpoint_fingerprint, schema_version, static_fingerprint,
                  updated_at, authoritative, models)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT (provider_id, endpoint_fingerprint) DO UPDATE SET
                 schema_version = excluded.schema_version,
                 static_fingerprint = excluded.static_fingerprint,
                 updated_at = excluded.updated_at,
                 authoritative = excluded.authoritative,
                 models = excluded.models",
            params![
                provider_id,
                endpoint_fingerprint,
                SCHEMA_VERSION,
                static_fingerprint,
                to_unix_seconds(SystemTime::now()),
                i64::from(authoritative),
                models_json,
            ],
        )?;
        Ok(())
    }

    /// 删掉一个条目；条目本就不存在也视为成功。
    pub fn invalidate(
        &self,
        provider_id: &str,
        endpoint_fingerprint: &str,
    ) -> Result<(), ModelCacheError> {
        self.conn.execute(
            "DELETE FROM model_cache WHERE provider_id = ?1 AND endpoint_fingerprint = ?2",
            params![provider_id, endpoint_fingerprint],
        )?;
        Ok(())
    }

    /// 清空全表。
    pub fn clear(&self) -> Result<(), ModelCacheError> {
        self.conn.execute("DELETE FROM model_cache", [])?;
        Ok(())
    }
}

/// 把 [`SystemTime`] 转成落盘用的 Unix 秒数。早于 epoch 的时间钳到 `0`，晚于
/// `i64::MAX` 秒的时间钳到 `i64::MAX`——两者在实际系统时钟下都不可达，钳制只是
/// 为了在类型上排除 `as` 截断，而不是真的预期触发。
fn to_unix_seconds(time: SystemTime) -> i64 {
    let secs = time
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs();
    i64::try_from(secs).unwrap_or(i64::MAX)
}

/// 把落盘的 Unix 秒数还原为 [`SystemTime`]；负数（本模块从不写入负数，防御性
/// 处理外部/历史数据）钳到 `0`。
fn from_unix_seconds(secs: i64) -> SystemTime {
    let secs = u64::try_from(secs).unwrap_or(0);
    UNIX_EPOCH + Duration::from_secs(secs)
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::spec::{LimitSpec, Modality, ModelStatus};

    fn sample_model(id: &str) -> ModelSpec {
        ModelSpec {
            id: id.into(),
            name: id.into(),
            cost: None,
            limit: LimitSpec {
                context: Some(1_000),
                output: None,
                input: None,
            },
            input: Box::from([Modality::Text]),
            output: Box::from([Modality::Text]),
            reasoning: false,
            tool_call: true,
            status: Some(ModelStatus::Beta),
        }
    }

    #[test]
    fn store_then_load_round_trips() {
        let cache = ModelCache::in_memory().unwrap();
        let models = vec![sample_model("a"), sample_model("b")];
        cache
            .store("openai", "fp-endpoint", "fp-static", &models, true)
            .unwrap();

        let loaded = cache
            .load("openai", "fp-endpoint", "fp-static")
            .unwrap()
            .unwrap();
        assert_eq!(loaded.provider_id.as_ref(), "openai");
        assert_eq!(loaded.models, models);
        assert!(loaded.authoritative);
    }

    #[test]
    fn load_misses_on_unknown_key() {
        let cache = ModelCache::in_memory().unwrap();
        assert!(
            cache
                .load("openai", "fp-endpoint", "fp-static")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn stale_static_fingerprint_misses_and_deletes_row() {
        let cache = ModelCache::in_memory().unwrap();
        let models = vec![sample_model("a")];
        cache
            .store("openai", "fp-endpoint", "fp-static-old", &models, true)
            .unwrap();

        // 形状变了：static_fingerprint 不再匹配，应该未命中。
        assert!(
            cache
                .load("openai", "fp-endpoint", "fp-static-new")
                .unwrap()
                .is_none()
        );

        // 且该行已被顺手删除：换回旧指纹也读不到了。
        assert!(
            cache
                .load("openai", "fp-endpoint", "fp-static-old")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn distinct_endpoint_fingerprints_do_not_overwrite_each_other() {
        let cache = ModelCache::in_memory().unwrap();
        let models_a = vec![sample_model("a")];
        let models_b = vec![sample_model("b")];
        cache
            .store("custom", "endpoint-1", "fp", &models_a, true)
            .unwrap();
        cache
            .store("custom", "endpoint-2", "fp", &models_b, true)
            .unwrap();

        let loaded_a = cache.load("custom", "endpoint-1", "fp").unwrap().unwrap();
        let loaded_b = cache.load("custom", "endpoint-2", "fp").unwrap().unwrap();
        assert_eq!(loaded_a.models, models_a);
        assert_eq!(loaded_b.models, models_b);
    }

    #[test]
    fn repeated_store_on_same_key_overwrites_without_error() {
        let cache = ModelCache::in_memory().unwrap();
        cache
            .store(
                "openai",
                "fp-endpoint",
                "fp-static",
                &[sample_model("a")],
                true,
            )
            .unwrap();
        cache
            .store(
                "openai",
                "fp-endpoint",
                "fp-static",
                &[sample_model("b")],
                false,
            )
            .unwrap();

        let loaded = cache
            .load("openai", "fp-endpoint", "fp-static")
            .unwrap()
            .unwrap();
        assert_eq!(loaded.models, vec![sample_model("b")]);
        assert!(!loaded.authoritative);
    }

    #[test]
    fn invalidate_removes_only_the_targeted_row() {
        let cache = ModelCache::in_memory().unwrap();
        cache
            .store("openai", "endpoint-1", "fp", &[sample_model("a")], true)
            .unwrap();
        cache
            .store("openai", "endpoint-2", "fp", &[sample_model("b")], true)
            .unwrap();

        cache.invalidate("openai", "endpoint-1").unwrap();

        assert!(cache.load("openai", "endpoint-1", "fp").unwrap().is_none());
        assert!(cache.load("openai", "endpoint-2", "fp").unwrap().is_some());
    }

    #[test]
    fn invalidate_on_missing_row_is_not_an_error() {
        let cache = ModelCache::in_memory().unwrap();
        cache.invalidate("nobody", "nowhere").unwrap();
    }

    #[test]
    fn clear_empties_every_provider() {
        let cache = ModelCache::in_memory().unwrap();
        cache
            .store("openai", "endpoint-1", "fp", &[sample_model("a")], true)
            .unwrap();
        cache
            .store("anthropic", "endpoint-2", "fp", &[sample_model("b")], true)
            .unwrap();

        cache.clear().unwrap();

        assert!(cache.load("openai", "endpoint-1", "fp").unwrap().is_none());
        assert!(
            cache
                .load("anthropic", "endpoint-2", "fp")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn open_persists_across_reopen_on_disk() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("nested").join("models-cache.db");

        {
            let cache = ModelCache::open(&path).unwrap();
            cache
                .store(
                    "openai",
                    "fp-endpoint",
                    "fp-static",
                    &[sample_model("a")],
                    true,
                )
                .unwrap();
        }

        let reopened = ModelCache::open(&path).unwrap();
        let loaded = reopened
            .load("openai", "fp-endpoint", "fp-static")
            .unwrap()
            .unwrap();
        assert_eq!(loaded.models, vec![sample_model("a")]);
    }

    #[test]
    fn open_rejects_unwritable_parent_as_io_error() {
        // 用一个已存在的普通文件当“父目录”，`create_dir_all` 必然失败,
        // 断言错误被映射成 `ModelCacheError::Io` 而不是从 SQLite 侧冒出来的怪错误。
        let dir = TempDir::new().unwrap();
        let blocker = dir.path().join("blocker");
        std::fs::write(&blocker, b"not a directory").unwrap();
        let db_path = blocker.join("models-cache.db");

        let result = ModelCache::open(&db_path);
        assert!(matches!(result, Err(ModelCacheError::Io { .. })));
    }

    #[test]
    fn unix_timestamp_round_trips_through_store_and_load() {
        let cache = ModelCache::in_memory().unwrap();
        let before = SystemTime::now();
        cache
            .store(
                "openai",
                "fp-endpoint",
                "fp-static",
                &[sample_model("a")],
                true,
            )
            .unwrap();
        let after = SystemTime::now();

        let loaded = cache
            .load("openai", "fp-endpoint", "fp-static")
            .unwrap()
            .unwrap();
        // 秒级精度：允许 updated_at 落在 [before, after] 这一秒边界附近。
        assert!(loaded.updated_at + Duration::from_secs(1) >= before);
        assert!(loaded.updated_at <= after + Duration::from_secs(1));
    }
}
