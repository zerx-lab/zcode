//! 内置目录 `models.json` 的惰性加载、查询与成本计算。
//!
//! ## 两级惰性解析
//!
//! `models.json` 有 180 个 provider、6149 个 model、约 1.5 MB。上游 oh-my-pi
//! （`packages/catalog/src/models.ts:16-30`）按 provider 惰性构建，原因是它们
//! 曾经在首帧路径同步构建整张目录，实测耗时约 210 ms（见
//! `packages/coding-agent/CHANGELOG.md:3410`）——同步解析全部 provider 会直接
//! 拖慢每一次进程启动，而绝大多数会话只用到一到两个 provider。
//!
//! 本模块因此做两级惰性，而不是一次性 `serde_json::from_str::<CatalogFile>`：
//!
//! 1. **顶层索引**：把 `models.json` 解析成
//!    `HashMap<Box<str>, &'static RawValue>`（[`RAW_INDEX`]）。`RawValue`
//!    只记录每个 provider 子树在原始字符串里的起止范围，不构造任何
//!    `ProviderSpec`/`ModelSpec`；这一步只做一次，且只需要识别 JSON 对象的
//!    键和大括号配对，不需要理解 6149 个 model 各自的字段。
//! 2. **按 provider 解析**：某个 provider 第一次被 [`BundledCatalog::provider`]
//!    访问时，才把它的原始子串 `serde_json::from_str::<ProviderSpec>`，结果
//!    存进 [`PARSED`]（`RwLock<HashMap<Box<str>, Arc<ProviderSpec>>>`）。此后
//!    同一个 id 的每次访问都直接命中缓存、复用同一个 `Arc`。
//!
//! 代价：[`BundledCatalog::find_model_everywhere`] 需要跨 provider 搜索，会
//! 强制触发第二级的全量解析，退化成一次性解析——这是唯一放弃惰性优势的查询，
//! 文档在该函数上重复了这一点。
//!
//! ## 成本计算
//!
//! [`calculate_cost`] 在定价未知（`ModelSpec::cost == None`）时返回 `None`，
//! 绝不返回 `0.0`：账单归因宁可标记"未知"也不能悄悄把未知成本记成免费，这与
//! `spec.rs` 里 `ModelSpec::cost` 字段的约定一致。[`cache_write_cost`] 的
//! `OneHour` 档改用 `input * 2` 推导，而不取目录里的 `cost.cache_write`——见该
//! 函数上的文档。

use std::collections::HashMap;
use std::sync::{Arc, OnceLock, PoisonError, RwLock};

use serde::de::Error as _;
use serde_json::value::RawValue;

use crate::spec::{CostSpec, ModelSpec, ProviderSpec};

/// 内置 `models.json` 的原始文本，编译期嵌入二进制（生成物，见 `rule://zcode-architecture`）。
const BUNDLED_JSON: &str = include_str!("models.json");

/// 长上下文阶梯定价的分界点，单位 token。
///
/// 200K 是 Anthropic / Gemini 公开定价文档给出的固定阈值，指的是**这次请求占用的
/// 上下文 token 数**，与模型自身的容量上限（[`crate::spec::LimitSpec::context`]）
/// 是两回事：即便模型上限只有 128K，调用方传入的 `context_tokens` 理论上仍可能
/// 超过 200K（例如上游临时放宽），阶梯价照样按这个固定阈值切换。
const LONG_CONTEXT_THRESHOLD_TOKENS: u64 = 200_000;

/// 顶层索引：provider id -> 该 provider 在 `models.json` 里的原始子串。
type RawIndex = HashMap<Box<str>, &'static RawValue>;

/// 顶层索引的惰性单例；只解析一次。
static RAW_INDEX: OnceLock<Result<RawIndex, Box<str>>> = OnceLock::new();

/// 第二级缓存：provider id -> 已解析的 `ProviderSpec`。只有实际被访问过的
/// provider 才会出现在这里。用 `RwLock` 而非 `Mutex`：并发只读查询（多个请求
/// 同时探测各自的 provider）不必互相排队，只有"第一次遇到某 provider 需要
/// 写入缓存"这一刻才短暂持写锁。
static PARSED: OnceLock<RwLock<HashMap<Box<str>, Arc<ProviderSpec>>>> = OnceLock::new();

/// 目录查询可能失败的原因。
#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    /// 单个 provider 的 JSON 子树反序列化为 [`ProviderSpec`] 失败。
    #[error("解析提供商 {provider} 失败: {source}")]
    Parse {
        /// 出错的 provider id。
        provider: Box<str>,
        /// 底层 `serde_json` 错误。
        source: serde_json::Error,
    },
    /// 顶层 JSON（provider id -> 原始子树 的映射）解析失败。
    ///
    /// 理论上不应该发生：`models.json` 由 `gen-models` 生成并随二进制嵌入，
    /// 但内容损坏（例如手工改坏、构建产物被截断）时仍需要一个可返回的错误，
    /// 而不是让整个进程 panic。
    #[error("解析目录根失败: {source}")]
    Root {
        /// 底层 `serde_json` 错误。
        source: serde_json::Error,
    },
}

/// 内置目录的句柄；所有查询都经由它。
///
/// 零大小类型，不持有任何状态——状态全部在模块级 `static` 里惰性构建，因此
/// `BundledCatalog` 可以按值随意传递、复制，没有任何运行时开销。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BundledCatalog;

impl BundledCatalog {
    /// 全部 provider id，字典序。
    ///
    /// 只需要顶层索引（第一级惰性），不会触发任何 provider 的完整解析。
    /// 顶层 JSON 本身损坏时记录错误日志并返回空列表——这个函数的签名不带
    /// `Result`，无法把错误传给调用方；需要感知这类失败请改用
    /// [`BundledCatalog::provider`]，它会返回 [`CatalogError::Root`]。
    #[must_use]
    pub fn provider_ids() -> Vec<&'static str> {
        match raw_index() {
            Ok(index) => {
                let mut ids: Vec<&'static str> = index.keys().map(Box::as_ref).collect();
                ids.sort_unstable();
                ids
            }
            Err(err) => {
                tracing::error!(error = %err, "内置目录顶层索引解析失败，provider_ids 返回空列表");
                Vec::new()
            }
        }
    }

    /// 按 id 查找 provider；不存在返回 `Ok(None)`。
    ///
    /// 第一次访问某个 id 才会解析它的 JSON 子树并存入缓存，此后的调用直接
    /// 复用同一个 `Arc`（克隆 `Arc` 只是引用计数 +1，不重新解析）。
    pub fn provider(id: &str) -> Result<Option<Arc<ProviderSpec>>, CatalogError> {
        if let Some(spec) = cached_provider(id) {
            return Ok(Some(spec));
        }
        let index = raw_index()?;
        let Some(raw) = index.get(id) else {
            return Ok(None);
        };
        let spec: ProviderSpec =
            serde_json::from_str(raw.get()).map_err(|source| CatalogError::Parse {
                provider: id.into(),
                source,
            })?;
        Ok(Some(insert_provider(id, spec)))
    }

    /// 按 provider id + model id 精确查找一个模型。
    ///
    /// provider 不存在，或 provider 存在但没有该 model id，都返回 `Ok(None)`；
    /// 只有 JSON 解析本身失败才返回 `Err`。
    pub fn model(provider_id: &str, model_id: &str) -> Result<Option<ModelRef>, CatalogError> {
        let Some(provider) = Self::provider(provider_id)? else {
            return Ok(None);
        };
        if !provider.models.contains_key(model_id) {
            return Ok(None);
        }
        Ok(Some(ModelRef {
            provider,
            model_id: model_id.into(),
        }))
    }

    /// 某个 provider 的全部 model id，字典序。provider 不存在时返回空列表。
    pub fn provider_model_ids(provider_id: &str) -> Result<Vec<Box<str>>, CatalogError> {
        let Some(provider) = Self::provider(provider_id)? else {
            return Ok(Vec::new());
        };
        // `ProviderSpec::models` 是 `BTreeMap`，键迭代天然按字典序。
        Ok(provider.models.keys().cloned().collect())
    }

    /// 全目录搜索同名 model id，返回全部命中——同一个 model id 可能被多家
    /// provider 各自托管（例如多个中转商都转发 `claude-opus-4-5`）。
    ///
    /// # 代价
    /// 会遍历并解析**全部** 180 个 provider，触发完整的第二级惰性解析，
    /// 是本模块里唯一放弃惰性优势的查询。已知 provider + model 时优先用
    /// [`BundledCatalog::model`]，只解析用得到的那一个。
    pub fn find_model_everywhere(model_id: &str) -> Result<Vec<ModelRef>, CatalogError> {
        let mut hits = Vec::new();
        for provider_id in Self::provider_ids() {
            let Some(provider) = Self::provider(provider_id)? else {
                continue;
            };
            if provider.models.contains_key(model_id) {
                hits.push(ModelRef {
                    provider: Arc::clone(&provider),
                    model_id: model_id.into(),
                });
            }
        }
        Ok(hits)
    }
}

/// 一个模型及其所属提供商的引用。持有 provider 的 `Arc`（廉价克隆），模型本身
/// 以引用暴露，不重复拷贝 [`ModelSpec`]。
#[derive(Debug, Clone)]
pub struct ModelRef {
    provider: Arc<ProviderSpec>,
    model_id: Box<str>,
}

impl ModelRef {
    /// 该模型所属的完整 provider 规格。
    #[must_use]
    pub fn provider(&self) -> &ProviderSpec {
        &self.provider
    }

    /// 该模型自身的规格。
    #[must_use]
    #[allow(clippy::expect_used)]
    // 不变量：ModelRef 只能通过 BundledCatalog::model / find_model_everywhere 构造，
    // 两处都先确认过 model_id ∈ provider.models 才会构造本值，因此这里一定命中。
    pub fn spec(&self) -> &ModelSpec {
        self.provider
            .models
            .get(self.model_id.as_ref())
            .expect("ModelRef 的 model_id 在构造时已验证存在于 provider.models")
    }

    /// 所属 provider 的 id。
    #[must_use]
    pub fn provider_id(&self) -> &str {
        &self.provider.id
    }

    /// 该模型的 id。
    #[must_use]
    pub fn model_id(&self) -> &str {
        &self.model_id
    }
}

/// 一次请求的 token 用量。四个字段互不重叠：`input` 已经扣除了
/// `cache_read` / `cache_write` 部分，不会重复计费。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TokenUsage {
    /// 未命中缓存的输入 token 数。
    pub input: u64,
    /// 输出 token 数。
    pub output: u64,
    /// 命中缓存读取的 token 数。
    pub cache_read: u64,
    /// 写入缓存的 token 数。
    pub cache_write: u64,
}

/// 缓存写入的保留时长档位，只影响 [`cache_write_cost`] 的取值路径。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheRetention {
    /// 5 分钟档：直接使用目录里的 `cost.cache_write`。
    FiveMinutes,
    /// 1 小时档：Anthropic 按 `input * 2` 这个模型无关的固定倍率计价。
    OneHour,
}

/// 计算一次请求的美元成本。
///
/// 定价未知（`spec.cost == None`）时返回 `None`，绝不返回 `0.0`：账单归因
/// 宁可标记"未知"也不能悄悄把未知成本记成免费。这条规则同样适用到单个分量：
/// `cache_read` / `cache_write` 单价缺失时，只要对应的 token 用量为 0，缺失的
/// 单价乘以 0 本来就是 0，不影响结果；但只要用量 > 0 而单价未知，就没法算出
/// 真实成本，整个函数返回 `None`，而不是悄悄按 0 计——那会让已经发生的缓存开销
/// 在账单里凭空消失。
///
/// `context_tokens` 超过 [`LONG_CONTEXT_THRESHOLD_TOKENS`] 且该模型提供了
/// `cost.context_over_200k` 时，`input`/`output`/`cache_read`/`cache_write`
/// 四项单价整体切换为阶梯单价；阶梯里缺失的缓存单价（上游常常只给阶梯的
/// `input`/`output`，不重复给缓存价）回退到基础单价，回退后仍缺失则按上一段
/// 的规则处理。
#[must_use]
pub fn calculate_cost(spec: &ModelSpec, usage: &TokenUsage, context_tokens: u64) -> Option<f64> {
    const PER_MILLION: f64 = 1_000_000.0;

    let cost = spec.cost.as_ref()?;
    let pricing = effective_pricing(cost, context_tokens);

    // 单价已知（`f64`）的分量：input/output 在 `cost: Some(_)` 时必然存在。
    let known_component = |tokens: u64, price: f64| tokens_to_f64(tokens) / PER_MILLION * price;
    // 单价可能未知（`Option<f64>`）的分量：0 token 时缺价无所谓（结果就是 0）；
    // 非 0 token 时缺价直接让整次计算失败。
    let optional_component = |tokens: u64, price: Option<f64>| -> Option<f64> {
        if tokens == 0 {
            Some(0.0)
        } else {
            price.map(|p| known_component(tokens, p))
        }
    };

    let input_cost = known_component(usage.input, pricing.input);
    let output_cost = known_component(usage.output, pricing.output);
    let cache_read_cost = optional_component(usage.cache_read, pricing.cache_read)?;
    let cache_write_cost = optional_component(usage.cache_write, pricing.cache_write)?;
    Some(input_cost + output_cost + cache_read_cost + cache_write_cost)
}

/// 缓存写入单价（单位 $/M token，不是某次请求的总成本）。
///
/// `OneHour` 档由 `input * 2` 推导，**不取**目录里的 `cost.cache_write`：
/// 那个倍率是 Anthropic 公布的模型无关常量，而目录里存的 `cache_write` 对应
/// 的是 5 分钟档，且数值会随上游快照漂移，两者不能混用。
#[must_use]
pub fn cache_write_cost(spec: &ModelSpec, retention: CacheRetention) -> Option<f64> {
    let cost = spec.cost.as_ref()?;
    match retention {
        CacheRetention::FiveMinutes => cost.cache_write,
        CacheRetention::OneHour => Some(cost.input * 2.0),
    }
}

/// 某个 `context_tokens` 值下实际生效的四项单价（$/M token）。`cache_read` /
/// `cache_write` 是 `Option`：`None` 表示上游确实没给这项定价，调用方
/// （[`calculate_cost`]）要自己决定"没有对应用量就不算"还是"有用量但没价就失败"，
/// 这个结构本身不替调用方做判断、更不会悄悄填 0。
struct EffectivePricing {
    input: f64,
    output: f64,
    cache_read: Option<f64>,
    cache_write: Option<f64>,
}

/// 解析出 [`calculate_cost`] 实际要用的四项单价：默认走基础价，超过阶梯阈值
/// 且提供了阶梯价时整体切到阶梯价；阶梯缺失的缓存价回退到基础价，回退后仍然
/// 缺失就保持 `None`（交给 [`calculate_cost`] 按用量决定是否致命）。
fn effective_pricing(cost: &CostSpec, context_tokens: u64) -> EffectivePricing {
    if context_tokens > LONG_CONTEXT_THRESHOLD_TOKENS
        && let Some(tier) = cost.context_over_200k.as_ref()
    {
        return EffectivePricing {
            input: tier.input,
            output: tier.output,
            cache_read: tier.cache_read.or(cost.cache_read),
            cache_write: tier.cache_write.or(cost.cache_write),
        };
    }
    EffectivePricing {
        input: cost.input,
        output: cost.output,
        cache_read: cost.cache_read,
        cache_write: cost.cache_write,
    }
}

/// 把 token 计数转换为 `f64`，不使用 `as`。
///
/// `u32 -> f64` 是精确转换（`u32::MAX` 远小于 `f64` 尾数能精确表示的 2^53
/// 上限），单次请求超出 `u32` 范围的用量（43 亿+ token，现实中不会出现）按
/// `u32::MAX` 分段累加，而不是截断成不精确的值或者 panic。
fn tokens_to_f64(mut tokens: u64) -> f64 {
    let mut total = 0.0_f64;
    while tokens > 0 {
        let chunk = u32::try_from(tokens).unwrap_or(u32::MAX);
        total += f64::from(chunk);
        tokens -= u64::from(chunk);
    }
    total
}

/// 顶层索引：解析一次并缓存，之后的调用直接复用同一个 `&'static RawIndex`。
fn raw_index() -> Result<&'static RawIndex, CatalogError> {
    let result = RAW_INDEX.get_or_init(|| {
        serde_json::from_str::<RawIndex>(BUNDLED_JSON)
            .map_err(|err| err.to_string().into_boxed_str())
    });
    match result {
        Ok(index) => Ok(index),
        // `serde_json::Error` 不是 `Clone`，无法把第一次的错误缓存下来重复
        // 返回同一个实例；改为缓存错误文本，每次调用时用它重新构造一个
        // 语义等价的 `Error`。
        Err(msg) => Err(CatalogError::Root {
            source: serde_json::Error::custom(msg.as_ref()),
        }),
    }
}

/// 拿到（或初始化）第二级缓存。
fn parsed_cache() -> &'static RwLock<HashMap<Box<str>, Arc<ProviderSpec>>> {
    PARSED.get_or_init(|| RwLock::new(HashMap::new()))
}

/// 读二级缓存；命中则克隆 `Arc`（只是引用计数 +1）。
fn cached_provider(id: &str) -> Option<Arc<ProviderSpec>> {
    let cache = parsed_cache()
        .read()
        .unwrap_or_else(PoisonError::into_inner);
    cache.get(id).map(Arc::clone)
}

/// 把新解析出的 provider 写入二级缓存。并发场景下先到先得：如果另一个线程
/// 已经抢先插入了同一个 id，直接复用它已经放进去的 `Arc`，丢弃本线程刚解析
/// 出来的那份，保证同一个 id 全程只有一个 `Arc`（同一块堆内存）在流通。
fn insert_provider(id: &str, spec: ProviderSpec) -> Arc<ProviderSpec> {
    let mut cache = parsed_cache()
        .write()
        .unwrap_or_else(PoisonError::into_inner);
    Arc::clone(cache.entry(id.into()).or_insert_with(|| Arc::new(spec)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anthropic_provider_has_claude_opus_4_5() {
        let provider = BundledCatalog::provider("anthropic")
            .unwrap()
            .expect("bundled models.json 应当包含 anthropic provider");
        assert_eq!(provider.id.as_ref(), "anthropic");
        assert!(provider.models.contains_key("claude-opus-4-5"));
    }

    #[test]
    fn missing_provider_returns_none() {
        let result = BundledCatalog::provider("this-provider-does-not-exist-in-catalog").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn missing_model_on_existing_provider_returns_none() {
        let result =
            BundledCatalog::model("anthropic", "this-model-does-not-exist-anywhere").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn provider_lookup_is_lazily_cached_to_the_same_arc() {
        let first = BundledCatalog::provider("openai")
            .unwrap()
            .expect("openai provider 应当存在");
        let second = BundledCatalog::provider("openai")
            .unwrap()
            .expect("第二次查询应当命中缓存");
        assert!(
            Arc::ptr_eq(&first, &second),
            "两次查询同一个 provider 应当复用同一个 Arc，而不是重新解析"
        );
    }

    #[test]
    fn provider_ids_are_sorted_and_contain_known_providers() {
        let ids = BundledCatalog::provider_ids();
        assert!(ids.contains(&"anthropic"));
        assert!(ids.contains(&"openai"));
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        assert_eq!(ids, sorted, "provider_ids 必须已经是字典序");
    }

    #[test]
    fn provider_model_ids_lists_models_in_order() {
        let ids = BundledCatalog::provider_model_ids("anthropic").unwrap();
        assert!(ids.iter().any(|id| id.as_ref() == "claude-opus-4-5"));
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        assert_eq!(ids, sorted, "provider_model_ids 必须已经是字典序");
    }

    #[test]
    fn provider_model_ids_on_missing_provider_is_empty() {
        let ids = BundledCatalog::provider_model_ids("this-provider-does-not-exist").unwrap();
        assert!(ids.is_empty());
    }

    #[test]
    fn model_ref_exposes_provider_and_model_ids() {
        let model_ref = BundledCatalog::model("anthropic", "claude-opus-4-5")
            .unwrap()
            .expect("claude-opus-4-5 应当存在于 anthropic");
        assert_eq!(model_ref.provider_id(), "anthropic");
        assert_eq!(model_ref.model_id(), "claude-opus-4-5");
        assert_eq!(model_ref.spec().id.as_ref(), "claude-opus-4-5");
        assert_eq!(model_ref.provider().id.as_ref(), "anthropic");
    }

    #[test]
    fn find_model_everywhere_hits_multiple_providers() {
        // claude-opus-4-5 被 anthropic 官方以及至少一家中转商托管。
        let hits = BundledCatalog::find_model_everywhere("claude-opus-4-5").unwrap();
        assert!(
            hits.iter().any(|hit| hit.provider_id() == "anthropic"),
            "至少应当命中 anthropic 官方"
        );
        assert!(
            hits.len() >= 2,
            "claude-opus-4-5 预期被多家 provider 托管，实际命中 {}",
            hits.len()
        );
    }

    #[test]
    fn find_model_everywhere_on_unknown_model_is_empty() {
        let hits = BundledCatalog::find_model_everywhere("no-such-model-anywhere").unwrap();
        assert!(hits.is_empty());
    }

    #[test]
    fn calculate_cost_is_none_when_pricing_unknown() {
        let model_ref = BundledCatalog::model("anyapi", "anthropic/claude-haiku-4-5")
            .unwrap()
            .expect("anyapi/anthropic/claude-haiku-4-5 应当存在且定价未知");
        assert!(
            model_ref.spec().cost.is_none(),
            "本用例依赖该模型没有定价数据"
        );
        let usage = TokenUsage {
            input: 1000,
            output: 500,
            cache_read: 0,
            cache_write: 0,
        };
        assert_eq!(calculate_cost(model_ref.spec(), &usage, 1000), None);
    }

    #[test]
    fn calculate_cost_uses_base_pricing_below_threshold() {
        let model_ref = BundledCatalog::model("anthropic", "claude-opus-4-5")
            .unwrap()
            .expect("claude-opus-4-5 应当存在");
        let usage = TokenUsage {
            input: 1_000_000,
            output: 1_000_000,
            cache_read: 0,
            cache_write: 0,
        };
        let cost = calculate_cost(model_ref.spec(), &usage, 1_000)
            .expect("claude-opus-4-5 定价已知，不应为 None");
        // input 5 $/M + output 25 $/M，各消耗 1M token。
        assert!(
            (cost - 30.0).abs() < 1e-9,
            "预期成本 30.0 美元，实际 {cost}"
        );
    }

    #[test]
    fn calculate_cost_switches_to_tier_pricing_above_threshold() {
        let model_ref = BundledCatalog::model("302ai", "claude-opus-4-7")
            .unwrap()
            .expect("302ai/claude-opus-4-7 应当存在且带阶梯定价");
        let cost = model_ref.spec().cost.as_ref().expect("该模型应当有定价");
        assert!(
            cost.context_over_200k.is_some(),
            "本用例依赖该模型带阶梯定价"
        );

        let usage = TokenUsage {
            input: 1_000_000,
            output: 0,
            cache_read: 0,
            cache_write: 0,
        };
        let below =
            calculate_cost(model_ref.spec(), &usage, 100_000).expect("阈值以下应有基础定价");
        let above =
            calculate_cost(model_ref.spec(), &usage, 300_000).expect("阈值以上应有阶梯定价");
        // 基础 input 5 $/M，阶梯 input 10 $/M；同样的用量阶梯价应当翻倍。
        assert!(
            (below - 5.0).abs() < 1e-9,
            "阈值以下预期 5.0 美元，实际 {below}"
        );
        assert!(
            (above - 10.0).abs() < 1e-9,
            "阈值以上预期 10.0 美元，实际 {above}"
        );
    }

    #[test]
    fn calculate_cost_is_none_when_cache_price_missing_but_used() {
        let model_ref = BundledCatalog::model("302ai", "MiniMax-M1")
            .unwrap()
            .expect("302ai/MiniMax-M1 应当存在且没有 cache_read 单价");
        let cost = model_ref.spec().cost.as_ref().expect("该模型应当有定价");
        assert!(
            cost.cache_read.is_none(),
            "本用例依赖该模型缺失 cache_read 单价"
        );

        // cache_read 用量 > 0 但单价未知：不能悄悄按 0 计，必须整体失败。
        let used = TokenUsage {
            input: 0,
            output: 0,
            cache_read: 1000,
            cache_write: 0,
        };
        assert_eq!(calculate_cost(model_ref.spec(), &used, 1000), None);

        // cache_read 用量 == 0：缺失的单价乘以 0 无所谓，照样能算出结果。
        let unused = TokenUsage {
            input: 1_000_000,
            output: 0,
            cache_read: 0,
            cache_write: 0,
        };
        let result = calculate_cost(model_ref.spec(), &unused, 1000)
            .expect("cache_read 用量为 0 时缺失单价不应影响计算");
        assert!(
            (result - cost.input).abs() < 1e-9,
            "预期成本等于 input 单价，实际 {result}"
        );
    }

    #[test]
    fn cache_write_cost_one_hour_is_double_input_price() {
        let model_ref = BundledCatalog::model("anthropic", "claude-opus-4-5")
            .unwrap()
            .expect("claude-opus-4-5 应当存在");
        let spec = model_ref.spec();
        let input_price = spec.cost.as_ref().expect("应有定价").input;
        let one_hour = cache_write_cost(spec, CacheRetention::OneHour)
            .expect("定价已知时 OneHour 档不应为 None");
        assert!((one_hour - input_price * 2.0).abs() < 1e-9);
    }

    #[test]
    fn cache_write_cost_five_minutes_uses_catalog_value_directly() {
        let model_ref = BundledCatalog::model("anthropic", "claude-opus-4-5")
            .unwrap()
            .expect("claude-opus-4-5 应当存在");
        let spec = model_ref.spec();
        let expected = spec.cost.as_ref().expect("应有定价").cache_write;
        assert_eq!(
            cache_write_cost(spec, CacheRetention::FiveMinutes),
            expected
        );
    }

    #[test]
    fn cache_write_cost_is_none_when_pricing_unknown() {
        let model_ref = BundledCatalog::model("anyapi", "anthropic/claude-haiku-4-5")
            .unwrap()
            .expect("该模型应当没有定价数据");
        assert_eq!(
            cache_write_cost(model_ref.spec(), CacheRetention::OneHour),
            None
        );
    }

    #[test]
    fn tokens_to_f64_matches_exact_integer_value() {
        assert!((tokens_to_f64(0) - 0.0).abs() < f64::EPSILON);
        assert!((tokens_to_f64(1_234_567) - 1_234_567.0).abs() < f64::EPSILON);
        // 跨越 u32::MAX 的分段累加路径：u32::MAX + 5 无法直接用 `u32::try_from`
        // 一步转换，必须走两段累加；期望值用两次已知精确的 `f64::from(u32)`
        // 相加得到，不依赖被测函数本身的逻辑。
        let huge = u64::from(u32::MAX) + 5;
        let expected = f64::from(u32::MAX) + 5.0;
        assert!((tokens_to_f64(huge) - expected).abs() < 1.0);
    }
}
