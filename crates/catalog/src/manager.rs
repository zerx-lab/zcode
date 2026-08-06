//! 静态目录与运行时发现结果的仲裁与新鲜度策略。
//!
//! # 分层边界：本模块不发 HTTP 请求
//!
//! `zcode_ai::http` 是全 workspace 唯一的 `reqwest` 客户端；`zcode-ai` 反过来依赖
//! `zcode-catalog`（要用目录里的模型元数据），所以 `zcode-catalog` 绝不能依赖
//! `zcode-ai`——那会形成循环依赖。因此本模块只做**解析、仲裁、新鲜度判定**：
//! 谁来发 `GET /v1/models`、把响应字节交进来，是调用方（`crates/coding-agent`）的职责。
//! 本模块把字节（[`parse_openai_models_response`]）或已落盘的缓存（[`crate::cache::CachedModels`]）
//! 与内置静态目录（[`crate::models::BundledCatalog`]）仲裁成最终模型表（[`resolve`]），
//! 并回答“现在该不该再去拉一次”（[`should_fetch`]）。
//!
//! 仲裁与新鲜度策略移植自 oh-my-pi `packages/catalog/src/model-manager.ts`，但两点不照搬：
//! - 不用它在只读数组上就地挂 `Symbol` 属性做指纹缓存的写法；本仓静态目录是编译期常量，
//!   指纹用 [`OnceLock`] 算一次即可（见 [`static_fingerprint`]）。
//! - 不照搬无出处的魔数：详见 [`CACHE_TTL`] 与 [`NON_AUTHORITATIVE_RETRY`] 各自的文档。

use std::collections::BTreeMap;
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

use serde::Deserialize;

use crate::cache::CachedModels;
use crate::models::{BundledCatalog, CatalogError};
use crate::spec::{LimitSpec, ModelSpec};

/// 静态目录的指纹：内置 `models.json` 的字节变了，指纹就变，
/// 可作为 `SQLite` 缓存失效的维度之一（与 `endpoint_fingerprint` 一起用，
/// 见 [`crate::cache::ModelCache::load`]）。
///
/// 编译期内容（[`include_str!`] 嵌入的 `models.json`）上只算一次哈希，
/// 用 [`OnceLock`] 缓住，往后调用直接返回。
#[must_use]
pub fn static_fingerprint() -> &'static str {
    static FINGERPRINT: OnceLock<String> = OnceLock::new();
    FINGERPRINT.get_or_init(|| {
        // `models.json` 与本文件同目录，`include_str!` 在编译期把它的当前字节内容
        // 冻结进二进制——这里对它取指纹，等价于对“这次构建所用的静态目录”取指纹。
        let raw = include_str!("models.json");
        // 用标准库自带的 SipHash（`DefaultHasher`）而非引入额外的哈希/摘要依赖：
        // 这个指纹只用作本地缓存失效的一个维度，不需要跨机器可比对，
        // 也不需要抗碰撞的密码学强度，够用即可。
        let mut hasher = std::hash::DefaultHasher::new();
        std::hash::Hash::hash(raw, &mut hasher);
        format!("{:016x}", std::hash::Hasher::finish(&hasher))
    })
}

/// 刷新策略：调用方想要多“新鲜”的模型表。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshStrategy {
    /// 只用内置静态目录，不看缓存、不请求。冷启动快路径（例如 `--help`、补全脚本）。
    StaticOnly,
    /// 静态目录 + 缓存；缓存过期才需要调用方去拉一次。日常交互路径。
    CachedFirst,
    /// 无条件让调用方去拉一次（例如用户手动执行“刷新模型列表”）。
    Online,
}

/// 一次解析的结果来自哪里。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolutionSource {
    /// 只用了内置静态目录（没有可用缓存，也没有本轮 discovery）。
    Static,
    /// 用了缓存里的上一轮 discovery 结果（本轮没有再发起 discovery）。
    Cache,
    /// 本轮发起过 discovery，结果已与静态目录 / 缓存仲裁合并。
    Merged,
}

/// 一次 [`resolve`] 的结果。
#[derive(Debug, Clone, PartialEq)]
pub struct Resolution {
    /// 仲裁后的模型表，按 `id` 字典序排列，无重复。
    pub models: Vec<ModelSpec>,
    /// 这份结果的数据来源。
    pub source: ResolutionSource,
    /// 调用方是否应该去发一次 discovery 请求（等价于用当前入参重新调用一次
    /// [`should_fetch`]；这里直接给出，省得调用方自己再算一遍）。
    pub should_fetch: bool,
    /// 调用方把 `models` 存入缓存时，应该把 `authoritative` 参数设成什么。
    ///
    /// 本轮没有发起 discovery（`discovered` 传入 `None`）时，原样透传已有缓存的
    /// 权威性（没有更新的信息，不应该妄改判断）；既没有 discovery 也没有缓存
    /// （纯静态兜底）时为 `false`——没有任何“这一轮”的数据，谈不上权威。
    pub cache_authoritative: bool,
}

/// 静态目录内置的、上游未给出任何取值前提的经验值——本仓也未对其做过实测。
///
/// 沿用 oh-my-pi `model-manager.ts` 的 `DEFAULT_CACHE_TTL_MS = 2h`；已核实上游代码
/// 对这个数字没有任何注释、issue 引用或 benchmark。取用它只是因为“不比瞎猜差”，
/// 不代表本仓验证过 2 小时是合适的窗口。真要调它，先测再改这行注释。
pub const CACHE_TTL: Duration = Duration::from_hours(2);

/// 非权威缓存条目（discovery 失败、或本轮判定为“空结果不可信”）的重试窗口。
///
/// 前提明确：拉取失败过的 provider 如果仍按 [`CACHE_TTL`] 的长窗口等待，会让一次
/// 瞬时故障拖慢到下次冷启动才能恢复；5 分钟是“控制恢复延迟”与“不没事就重试脱机
/// provider”之间的折衷——只搬 TTL 不搬这个重试窗，会让离线 provider 每次启动都重试。
pub const NON_AUTHORITATIVE_RETRY: Duration = Duration::from_mins(5);

/// 新鲜度判定：给定已有缓存（如果有）与刷新策略，现在该不该去发一次 discovery。
///
/// 两级窗口：缓存条目 `authoritative` 时用 [`CACHE_TTL`]（长窗口，正常刷新节奏），
/// 否则用 [`NON_AUTHORITATIVE_RETRY`]（短窗口，尽快从失败/空响应里恢复）。
#[must_use]
pub fn should_fetch(
    cached: Option<&CachedModels>,
    strategy: RefreshStrategy,
    now: SystemTime,
) -> bool {
    match strategy {
        RefreshStrategy::StaticOnly => false,
        RefreshStrategy::Online => true,
        RefreshStrategy::CachedFirst => {
            let Some(cached) = cached else {
                // 从没缓存过，没有“过期”可言，直接判定需要拉。
                return true;
            };
            // `now < updated_at`（时钟回拨/漂移）没有现实中会稳定复现的场景；
            // 保守地当作“刚刚更新过”处理，不强行拉取。
            let elapsed = now
                .duration_since(cached.updated_at)
                .unwrap_or(Duration::ZERO);
            let window = if cached.authoritative {
                CACHE_TTL
            } else {
                NON_AUTHORITATIVE_RETRY
            };
            elapsed >= window
        }
    }
}

/// [`resolve`] 失败的原因。
#[derive(Debug, thiserror::Error)]
pub enum RefreshError {
    /// discovery 响应体不是合法 JSON，或形状对不上 OpenAI 兼容的 `/v1/models` 契约。
    #[error("解析模型发现响应失败: {source}")]
    Parse {
        /// 底层 JSON 解析错误。
        #[from]
        source: serde_json::Error,
    },
    /// 查询内置静态目录失败（只有顶层 JSON 损坏才会发生）。
    #[error("查询内置目录失败: {source}")]
    Catalog {
        /// 底层目录错误。
        #[from]
        source: CatalogError,
    },
}

/// 把静态目录、缓存、（可选的）本轮 discovery 结果仲裁成一份模型表。
///
/// # 仲裁规则
/// 1. 静态目录里已有的模型：能力位（`input`/`output`/`reasoning`/`tool_call`）与
///    `limit` 可被 discovery 覆盖，但 `cost` 只在静态侧为 `None` 时才接受外部值——
///    定价一旦由静态目录给出就是权威来源，不能被猜测性的外部数据覆盖。
/// 2. discovery 只给了模型 id（没有报告任何模态，即 `input`/`output` 均为空，
///    这正是 OpenAI 兼容 `/v1/models` 的真实形状）：已在静态目录里 → 直接用静态
///    条目（没有更好的信息可覆盖，规则 1 的“可覆盖”自然退化为“不覆盖”）；不在 →
///    造一个最小条目。
/// 3. discovery 返回空列表时具有双重语义：本轮权威（“这个 provider 真的一个模型
///    都没”，因此 `models` 就是空的），但绝不能让调用方把这次结果当“缓存权威”
///    存下去，否则会压掉恢复瞬时空响应的短重试窗——`Resolution::cache_authoritative`
///    在这种情况下固定为 `false`。
/// 4. 结果按 `id` 字典序排列，同 id 只保留最后出现的一条。
///
/// discovery（或缓存里上一轮的结果）定义了“这个 provider 现在有哪些模型 id”的
/// 完整集合：不在这个集合里的静态条目不会被强行拼回结果——静态目录只负责给
/// discovery 已经确认存在的 id 补字段，不负责发明 discovery 没提到的模型。
pub fn resolve(
    provider_id: &str,
    cached: Option<CachedModels>,
    discovered: Option<Vec<ModelSpec>>,
    strategy: RefreshStrategy,
    now: SystemTime,
) -> Result<Resolution, RefreshError> {
    let static_models = load_static_models(provider_id)?;
    Ok(resolve_with_static(
        static_models,
        cached,
        discovered,
        strategy,
        now,
    ))
}

/// [`resolve`] 的纯函数核心：静态模型表由调用方给定，不再触碰 [`BundledCatalog`]。
///
/// 拆出这一层是为了让仲裁规则可以脱离内置 `models.json` 的真实内容单独测试
/// （`models.json` 由生成器维护，内容会随上游快照变化，测试不该依赖它此刻长什么样）。
fn resolve_with_static(
    static_models: BTreeMap<Box<str>, ModelSpec>,
    cached: Option<CachedModels>,
    mut discovered: Option<Vec<ModelSpec>>,
    strategy: RefreshStrategy,
    now: SystemTime,
) -> Resolution {
    let discovered = discovered.take();
    if strategy == RefreshStrategy::StaticOnly {
        // “只用内置静态目录，不看缓存、不请求”——即便调用方误传了 cached/discovered，
        // 这里也要显式忽略，行为必须与文档承诺的一致。
        return Resolution {
            models: static_models.into_values().collect(),
            source: ResolutionSource::Static,
            should_fetch: false,
            cache_authoritative: false,
        };
    }

    let should_fetch_flag = if discovered.is_some() {
        // 本轮已经有 discovery 结果了，没必要再让调用方立刻拉一次。
        false
    } else {
        should_fetch(cached.as_ref(), strategy, now)
    };

    let (external, source, cache_authoritative) = match (&discovered, cached) {
        (Some(models), _) => {
            // 规则 3：空列表本轮权威，但不作为缓存权威——由调用方决定要不要存、
            // 存的话把 authoritative 设成这里给出的 false。
            let authoritative = !models.is_empty();
            (
                Some(models.clone()),
                ResolutionSource::Merged,
                authoritative,
            )
        }
        (None, Some(cached)) => {
            let authoritative = cached.authoritative;
            (Some(cached.models), ResolutionSource::Cache, authoritative)
        }
        (None, None) => (None, ResolutionSource::Static, false),
    };

    let models = match external {
        Some(items) => merge(&static_models, &items),
        None => static_models.into_values().collect(),
    };

    Resolution {
        models,
        source,
        should_fetch: should_fetch_flag,
        cache_authoritative,
    }
}

/// 从内置静态目录取出某 provider 的模型表；provider 不存在时返回空表
/// （空表意味着规则 2 的“不在静态目录里”分支对该 provider 的所有模型都成立）。
fn load_static_models(provider_id: &str) -> Result<BTreeMap<Box<str>, ModelSpec>, RefreshError> {
    match BundledCatalog::provider(provider_id)? {
        Some(provider) => Ok(provider.models.clone()),
        None => Ok(BTreeMap::new()),
    }
}

/// 用 `discovered` 定义的 id 集合仲裁出最终模型表：按 id 排序、去重
/// （规则 4；`BTreeMap` 的键唯一性天然去重，插入顺序决定同 id 时谁生效——
/// 后出现的条目覆盖先出现的）。
fn merge(
    static_models: &BTreeMap<Box<str>, ModelSpec>,
    discovered: &[ModelSpec],
) -> Vec<ModelSpec> {
    let mut merged: BTreeMap<Box<str>, ModelSpec> = BTreeMap::new();
    for item in discovered {
        let resolved = match static_models.get(&item.id) {
            Some(static_entry) => merge_known(static_entry, item),
            None => build_minimal(item),
        };
        merged.insert(resolved.id.clone(), resolved);
    }
    merged.into_values().collect()
}

/// 规则 1 + 规则 2（已在静态目录里的分支）：discovery 报告的模型 id 已存在于
/// 静态目录，把两者仲裁成一条 [`ModelSpec`]。
fn merge_known(static_entry: &ModelSpec, discovered: &ModelSpec) -> ModelSpec {
    // OpenAI 兼容 `/v1/models` 从不报告模态，此时 discovered.input/output 恒为空，
    // 视作“discovery 没有能力数据”——能力位与 limit 都保留静态目录的值。
    // 这正是规则 2「已在静态目录里 → 用静态条目」的由来：不是特判，是“无覆盖数据
    // 时覆盖操作自然是恒等”的自然结果。`limit` 单独按字段用 `Option::or`
    // 处理是因为它本身就是 `Option`，不需要这个总开关也能正确表达“未知”。
    let has_capability_report = !discovered.input.is_empty() || !discovered.output.is_empty();

    ModelSpec {
        id: static_entry.id.clone(),
        name: static_entry.name.clone(),
        // 规则 1：cost 只在静态侧缺失时才接受外部值。
        cost: static_entry.cost.or(discovered.cost),
        limit: LimitSpec {
            context: discovered.limit.context.or(static_entry.limit.context),
            output: discovered.limit.output.or(static_entry.limit.output),
            input: discovered.limit.input.or(static_entry.limit.input),
        },
        input: if has_capability_report {
            discovered.input.clone()
        } else {
            static_entry.input.clone()
        },
        output: if has_capability_report {
            discovered.output.clone()
        } else {
            static_entry.output.clone()
        },
        reasoning: if has_capability_report {
            discovered.reasoning
        } else {
            static_entry.reasoning
        },
        tool_call: if has_capability_report {
            discovered.tool_call
        } else {
            static_entry.tool_call
        },
        // 生命周期状态（alpha/beta/deprecated）只有静态目录在维护，discovery 端点
        // 不会报告这个信息。
        status: static_entry.status,
    }
}

/// 规则 2（不在静态目录里的分支）：discovery 报告了一个静态目录完全不认识的
/// 模型 id，造一条最小可用条目。
fn build_minimal(discovered: &ModelSpec) -> ModelSpec {
    ModelSpec {
        id: discovered.id.clone(),
        name: discovered.name.clone(),
        cost: None,
        limit: LimitSpec::default(),
        input: discovered.input.clone(),
        output: discovered.output.clone(),
        reasoning: false,
        // 默认 true 而不是 false：假定不支持工具调用会让整个 agent 循环对这个新
        // 模型直接不可用（工具调用是 agent 主循环的前提），而假定支持的代价只是
        // 一次上游 400——前者是死路，后者是可恢复的单次失败，两害相权取其轻。
        tool_call: true,
        status: None,
    }
}

/// OpenAI 兼容 `GET /v1/models` 响应体的最小反序列化形状。
///
/// 真实实现里字段远不止这些（`object`、`created`、`owned_by` 等），但仲裁逻辑
/// 只关心 `id`——多余字段交给 `serde` 默认忽略。
#[derive(Debug, Deserialize)]
struct ModelsListResponse {
    data: Vec<ModelListEntry>,
}

#[derive(Debug, Deserialize)]
struct ModelListEntry {
    id: Box<str>,
}

/// 解析 OpenAI 兼容的 `GET /v1/models` 应答。
///
/// 返回的 [`ModelSpec`] **定价一律为 `None`**、**模态一律为空**：远端端点不会告诉
/// 你价格或支持哪些输入/输出模态，猜一个不如承认未知——上游反复强调能力位可以
/// 跨来源继承，定价绝不可以跨 provider 猜测。这些“空白”条目在 [`resolve`] 里
/// 与静态目录仲裁时，已知 id 会被静态目录的字段填满（见 `merge_known`）。
pub fn parse_openai_models_response(body: &[u8]) -> Result<Vec<ModelSpec>, RefreshError> {
    let parsed: ModelsListResponse = serde_json::from_slice(body)?;

    let mut by_id: BTreeMap<Box<str>, ModelSpec> = BTreeMap::new();
    for entry in parsed.data {
        let spec = ModelSpec {
            name: entry.id.clone(),
            id: entry.id,
            cost: None,
            limit: LimitSpec::default(),
            input: Box::default(),
            output: Box::default(),
            reasoning: false,
            tool_call: true,
            status: None,
        };
        by_id.insert(spec.id.clone(), spec);
    }
    Ok(by_id.into_values().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::{CostSpec, Modality};

    fn cached(models: Vec<ModelSpec>, updated_at: SystemTime, authoritative: bool) -> CachedModels {
        CachedModels {
            provider_id: "acme".into(),
            models,
            updated_at,
            authoritative,
        }
    }

    fn model(id: &str) -> ModelSpec {
        ModelSpec {
            id: id.into(),
            name: id.into(),
            cost: None,
            limit: LimitSpec::default(),
            input: Box::default(),
            output: Box::default(),
            reasoning: false,
            tool_call: true,
            status: None,
        }
    }

    // ── static_fingerprint ──────────────────────────────────────────────

    #[test]
    fn static_fingerprint_is_stable_and_nonempty() {
        let a = static_fingerprint();
        let b = static_fingerprint();
        assert_eq!(a, b);
        assert!(!a.is_empty());
    }

    // ── should_fetch ────────────────────────────────────────────────────

    #[test]
    fn should_fetch_static_only_never_fetches() {
        let now = SystemTime::now();
        // 缓存严重过期也不该触发拉取——StaticOnly 承诺“不看缓存”。
        let stale = cached(vec![], now - CACHE_TTL - Duration::from_secs(1), true);
        assert!(!should_fetch(
            Some(&stale),
            RefreshStrategy::StaticOnly,
            now
        ));
        assert!(!should_fetch(None, RefreshStrategy::StaticOnly, now));
    }

    #[test]
    fn should_fetch_online_always_fetches() {
        let now = SystemTime::now();
        let fresh = cached(vec![], now, true);
        assert!(should_fetch(Some(&fresh), RefreshStrategy::Online, now));
        assert!(should_fetch(None, RefreshStrategy::Online, now));
    }

    #[test]
    fn should_fetch_cached_first_no_cache_means_fetch() {
        let now = SystemTime::now();
        assert!(should_fetch(None, RefreshStrategy::CachedFirst, now));
    }

    #[test]
    fn should_fetch_cached_first_authoritative_ttl_window() {
        let now = SystemTime::now();
        let within = cached(
            vec![],
            now.checked_sub(
                CACHE_TTL
                    .checked_sub(Duration::from_secs(1))
                    .unwrap_or(Duration::ZERO),
            )
            .unwrap_or(now),
            true,
        );
        assert!(!should_fetch(
            Some(&within),
            RefreshStrategy::CachedFirst,
            now
        ));

        let expired = cached(vec![], now - (CACHE_TTL + Duration::from_secs(1)), true);
        assert!(should_fetch(
            Some(&expired),
            RefreshStrategy::CachedFirst,
            now
        ));
    }

    #[test]
    fn should_fetch_cached_first_non_authoritative_short_window() {
        let now = SystemTime::now();
        // 非权威条目在 TTL 窗口内、但已超过短重试窗——仍应尽快重试，不等 2 小时。
        let within_retry = cached(
            vec![],
            now.checked_sub(
                NON_AUTHORITATIVE_RETRY
                    .checked_sub(Duration::from_secs(1))
                    .unwrap_or(Duration::ZERO),
            )
            .unwrap_or(now),
            false,
        );
        assert!(!should_fetch(
            Some(&within_retry),
            RefreshStrategy::CachedFirst,
            now
        ));

        let past_retry = cached(
            vec![],
            now - (NON_AUTHORITATIVE_RETRY + Duration::from_secs(1)),
            false,
        );
        assert!(should_fetch(
            Some(&past_retry),
            RefreshStrategy::CachedFirst,
            now
        ));
    }

    // ── 仲裁规则 1：能力位/上限可覆盖，cost 只在静态侧缺失时接受外部值 ──────

    #[test]
    fn rule1_capability_and_limit_overridden_but_existing_cost_kept() {
        let mut static_models = BTreeMap::new();
        static_models.insert(
            Box::from("m1"),
            ModelSpec {
                id: "m1".into(),
                name: "Model One".into(),
                cost: Some(CostSpec {
                    input: 1.0,
                    output: 2.0,
                    cache_read: None,
                    cache_write: None,
                    context_over_200k: None,
                }),
                limit: LimitSpec {
                    context: Some(100_000),
                    output: Some(4_096),
                    input: None,
                },
                input: Box::from([Modality::Text]),
                output: Box::from([Modality::Text]),
                reasoning: false,
                tool_call: false,
                status: None,
            },
        );

        let discovered = ModelSpec {
            id: "m1".into(),
            name: "ignored, discovery 名字不采信".into(),
            cost: Some(CostSpec {
                input: 99.0,
                output: 99.0,
                cache_read: None,
                cache_write: None,
                context_over_200k: None,
            }),
            limit: LimitSpec {
                context: Some(200_000),
                output: None,
                input: Some(1_000),
            },
            input: Box::from([Modality::Text, Modality::Image]),
            output: Box::from([Modality::Text]),
            reasoning: true,
            tool_call: true,
            status: None,
        };

        let resolution = resolve_with_static(
            static_models,
            None,
            Some(vec![discovered]),
            RefreshStrategy::CachedFirst,
            SystemTime::now(),
        );

        assert_eq!(resolution.models.len(), 1);
        let merged = &resolution.models[0];
        assert!(merged.reasoning);
        assert!(merged.tool_call);
        assert_eq!(merged.input.as_ref(), [Modality::Text, Modality::Image]);
        assert_eq!(merged.limit.context, Some(200_000));
        assert_eq!(merged.limit.output, Some(4_096)); // discovered 没给，保留静态值
        assert_eq!(merged.limit.input, Some(1_000));
        // 静态侧已有定价，discovery 的定价被丢弃。
        let cost = merged
            .cost
            .expect("静态侧已给出 cost，合并结果不应变成 None");
        assert!((cost.input - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn rule1_cost_accepted_only_when_static_side_missing() {
        let mut static_models = BTreeMap::new();
        let mut base = model("m1");
        base.cost = None;
        static_models.insert(Box::from("m1"), base);

        let mut discovered = model("m1");
        discovered.cost = Some(CostSpec {
            input: 5.0,
            output: 6.0,
            cache_read: None,
            cache_write: None,
            context_over_200k: None,
        });

        let resolution = resolve_with_static(
            static_models,
            None,
            Some(vec![discovered]),
            RefreshStrategy::CachedFirst,
            SystemTime::now(),
        );

        let cost = resolution.models[0]
            .cost
            .expect("静态侧缺失定价时应接受外部值");
        assert!((cost.input - 5.0).abs() < f64::EPSILON);
    }

    // ── 仲裁规则 2：discovery 只给 id ────────────────────────────────────

    #[test]
    fn rule2_known_id_without_capability_report_keeps_static_entry_unchanged() {
        let mut static_models = BTreeMap::new();
        let static_entry = ModelSpec {
            id: "m1".into(),
            name: "Model One".into(),
            cost: Some(CostSpec {
                input: 1.0,
                output: 2.0,
                cache_read: None,
                cache_write: None,
                context_over_200k: None,
            }),
            limit: LimitSpec {
                context: Some(100_000),
                output: Some(4_096),
                input: Some(8_192),
            },
            input: Box::from([Modality::Text]),
            output: Box::from([Modality::Text]),
            reasoning: true,
            tool_call: false,
            status: None,
        };
        static_models.insert(Box::from("m1"), static_entry.clone());

        // `parse_openai_models_response` 产出的正是这种“只有 id”的占位条目。
        let discovered = model("m1");

        let resolution = resolve_with_static(
            static_models,
            None,
            Some(vec![discovered]),
            RefreshStrategy::CachedFirst,
            SystemTime::now(),
        );

        assert_eq!(resolution.models, vec![static_entry]);
    }

    #[test]
    fn rule2_unknown_id_builds_minimal_entry() {
        let static_models: BTreeMap<Box<str>, ModelSpec> = BTreeMap::new();
        let discovered = model("brand-new");

        let resolution = resolve_with_static(
            static_models,
            None,
            Some(vec![discovered]),
            RefreshStrategy::CachedFirst,
            SystemTime::now(),
        );

        assert_eq!(resolution.models.len(), 1);
        let built = &resolution.models[0];
        assert_eq!(built.id.as_ref(), "brand-new");
        assert_eq!(built.cost, None);
        assert_eq!(built.limit, LimitSpec::default());
        assert!(!built.reasoning);
        assert!(built.tool_call);
    }

    // ── 仲裁规则 3：空 discovery 本轮权威，但不算缓存权威 ──────────────────

    #[test]
    fn rule3_empty_discovery_is_round_authoritative_but_not_cache_authoritative() {
        let mut static_models = BTreeMap::new();
        static_models.insert(Box::from("m1"), model("m1"));

        let resolution = resolve_with_static(
            static_models,
            None,
            Some(vec![]),
            RefreshStrategy::CachedFirst,
            SystemTime::now(),
        );

        // 本轮权威：真的信了“这个 provider 现在没有模型”，不拿静态目录兜底填充。
        assert!(resolution.models.is_empty());
        assert_eq!(resolution.source, ResolutionSource::Merged);
        // 但缓存层不能把这次结果当权威存下去，否则会压掉短重试窗。
        assert!(!resolution.cache_authoritative);
    }

    #[test]
    fn rule3_nonempty_discovery_is_cache_authoritative() {
        let static_models: BTreeMap<Box<str>, ModelSpec> = BTreeMap::new();
        let resolution = resolve_with_static(
            static_models,
            None,
            Some(vec![model("m1")]),
            RefreshStrategy::CachedFirst,
            SystemTime::now(),
        );
        assert!(resolution.cache_authoritative);
    }

    // ── 仲裁规则 4：按 id 排序、去重 ────────────────────────────────────

    #[test]
    fn rule4_results_are_sorted_by_id_and_deduplicated() {
        let static_models: BTreeMap<Box<str>, ModelSpec> = BTreeMap::new();
        let mut dup_first = model("b");
        dup_first.input = Box::from([Modality::Text]);
        let mut dup_second = model("b");
        dup_second.input = Box::from([Modality::Text, Modality::Image]); // 后出现的应该生效

        let discovered = vec![model("c"), dup_first, model("a"), dup_second];

        let resolution = resolve_with_static(
            static_models,
            None,
            Some(discovered),
            RefreshStrategy::CachedFirst,
            SystemTime::now(),
        );

        let ids: Vec<&str> = resolution.models.iter().map(|m| m.id.as_ref()).collect();
        assert_eq!(ids, ["a", "b", "c"]);
        let b = resolution
            .models
            .iter()
            .find(|m| m.id.as_ref() == "b")
            .expect("b 应存在");
        assert_eq!(
            b.input.as_ref(),
            [Modality::Text, Modality::Image],
            "同 id 重复出现时，后一条应覆盖前一条"
        );
    }

    // ── resolve()：cached-only 路径与 cache_authoritative 透传 ─────────────

    #[test]
    fn resolve_uses_cache_when_no_discovery_ran_and_propagates_authoritative() {
        let static_models: BTreeMap<Box<str>, ModelSpec> = BTreeMap::new();
        let now = SystemTime::now();
        let cached_entry = cached(vec![model("m1")], now, false);

        let resolution = resolve_with_static(
            static_models,
            Some(cached_entry),
            None,
            RefreshStrategy::CachedFirst,
            now,
        );

        assert_eq!(resolution.source, ResolutionSource::Cache);
        assert!(!resolution.cache_authoritative);
        assert_eq!(resolution.models.len(), 1);
    }

    // ── parse_openai_models_response ───────────────────────────────────

    #[test]
    fn parse_openai_models_response_extracts_ids_sorted_and_deduplicated() {
        let body = br#"{"object":"list","data":[{"id":"model-b","object":"model"},{"id":"model-a"},{"id":"model-a"}]}"#;
        let models = parse_openai_models_response(body).expect("合法响应应解析成功");
        let ids: Vec<&str> = models.iter().map(|m| m.id.as_ref()).collect();
        assert_eq!(ids, ["model-a", "model-b"]);
        for spec in &models {
            assert_eq!(spec.cost, None);
            assert_eq!(spec.limit, LimitSpec::default());
            assert!(spec.input.is_empty());
            assert!(spec.output.is_empty());
            assert!(!spec.reasoning);
            assert!(spec.tool_call);
        }
    }

    #[test]
    fn parse_openai_models_response_rejects_malformed_json() {
        let err = parse_openai_models_response(b"not json").expect_err("非法 JSON 应返回错误");
        assert!(matches!(err, RefreshError::Parse { .. }));
    }
}
