//! 模型解析与装配：配置 + 内置目录 + 本地凭据 -> [`ResolvedModel`] / `Arc<dyn Provider>`。
//!
//! # 优先级链
//!
//! 最终使用的模型 id 按下列顺序取第一个 `Some`：
//!
//! 1. `--model`（[`resolve_model`] 的 `override_id` 参数，CLI flag）
//! 2. `ZCODE_MODEL` 环境变量
//! 3. `config.model.id`（项目/全局 TOML）
//! 4. 自动选择（见下）——以上都没给出时，按本地已配置的凭据挑一条线路，
//!    再取该线路目录里的默认模型
//!
//! # 自动选择顺序
//!
//! 没有任何显式模型请求时，按下列固定顺序取第一个"本地已配置凭据"的线路：
//!
//! `Anthropic -> OpenAiCodex -> OpenAi -> XaiOAuth -> Xai`
//!
//! 顺序来源与理由（`fn resolve_model` 一节要求写清楚，不能凭空排）：
//!
//! - 家族间的相对顺序沿用 jcode
//!   `crates/jcode-provider-core/src/selection.rs:44-65`（`auto_default_provider`）
//!   里硬编码的级联 `if/else`：候选集换成本仓五条线路，家族顺序保留它
//!   "Claude 优先于 OpenAI" 的取舍——两个 harness 都把 Anthropic 系模型当作
//!   编码任务的首选。jcode 的表里没有 xAI（它压根不支持这条线路），因此排在
//!   已有的两个家族之后，不算对 jcode 顺序的偏离。
//! - 同一家族内部，订阅制 OAuth 线路排在按 token 计费的 API key 线路之前
//!   （`OpenAiCodex` 先于 `OpenAi`，`XaiOAuth` 先于 `Xai`）：能查到 OAuth 凭据
//!   意味着用户已经为该订阅付了固定费用，默认用它比默认走额外计费的 API key
//!   更不容易造成意外账单。jcode 的 `OpenAI` 只有一条线路、没有这层区分，
//!   这条子顺序是本仓按同一套"减少意外开销"取舍精神补上的，不是照抄。
//! - 级联到底都没有可用凭据时，[`resolve_model`] 返回 [`ModelError`]（携带全部
//!   五条 `zcode auth login <provider>` 命令），不像 jcode 那样兜底回落到某个
//!   固定线路——jcode 的兜底是因为它后续还有请求期的鉴权失败处理兜底，本仓的
//!   [`resolve_model`] 是纯本地解析，没有下一层兜底可言，静默选一个必然没有
//!   凭据的线路只会把可诊断的错误推迟到请求期。
//!
//! # 为什么不在这里联网
//!
//! 本模块只读两样纯本地数据：编译期嵌入的内置 `models.json`
//! （[`zcode_catalog::models::BundledCatalog`]，零 I/O）与手写的静态描述符表
//! （[`zcode_catalog::descriptors`]），绝不发起模型发现请求。
//! `plans/runtime-boundary/implementation.md:93-94` 要求"秒开"：模型解析发生在
//! 建会话之前的启动热路径上，一旦在这里等一次网络往返，每次进程启动都会被拖慢。
//! 运行时模型发现（[`zcode_catalog::manager`]）与其 `SQLite` 落盘缓存
//! （[`zcode_catalog::cache`]）留给 session 建好之后的刷新流程去做，那时已经跑在
//! 事件循环里，一次异步拉取不会挡住首帧输出。
//!
//! 这也是本模块不读 [`zcode_catalog::cache::ModelCache`] 的原因：那张缓存表存的是
//! 运行时 discovery 补充进来的模型 id，只对"用户自定义 OpenAI 兼容端点"这类
//! `discovery_only` 提供商（`ollama` / `lmstudio` / `vllm`，见
//! `zcode_catalog::descriptors` 模块文档）有意义——它们的模型列表压根不在内置
//! `models.json` 里，只能靠运行时探测。本仓当前五条线路的模型集合完全由内置
//! `models.json` 覆盖，缓存里不会有这里用得上的内容；真要接入那类端点时，
//! 调用方需要先对端点发一次网络请求换来 `endpoint_fingerprint`，那已经不是
//! "解析已知配置"该做的事，不属于本模块职责。
//!
//! # 模糊匹配
//!
//! `--model` 允许简写（如 `sonnet`）：先精确匹配 id；未命中再退化为大小写不敏感的
//! 子串匹配。命中多个候选时报错并列出全部候选（[`ModelError`] 绝不静默取第一个）；
//! 命中零个候选时，退化到"去掉分隔符后再子串匹配"给一批"你是不是想输入"建议
//! （处理 `gpt5` 之类因为原 id 带 `-`/`.` 而匹配不上的输入）。
//!
//! 没有引入编辑距离算法：`crates/agent/src/tool/registry.rs:194-234` 那份
//! `levenshtein` 是 `zcode-agent` 的私有项，不能跨 crate 复用；前缀/包含匹配这一级
//! 已经覆盖了常见简写场景，验收标准也认定这一级足够，因此不再另起一份等价实现。
//!
//! # Catalog 导入边界
//!
//! 模型、思考阶梯、描述符等**值**一律来自 `zcode_catalog::<module>`；只有
//! [`zcode_ai::Effort`] / [`zcode_ai::Thinking`] / [`zcode_ai::ProviderId`] 这几个
//! **类型**经由 `zcode_ai` 的 re-export 使用（见 `rule://zcode-architecture` 的
//! catalog 导入边界）。

use std::collections::HashSet;
use std::sync::Arc;

use zcode_ai::auth::store::{CredentialStore, FileCredentialStore};
use zcode_ai::provider::anthropic::AnthropicProvider;
use zcode_ai::provider::openai_codex;
use zcode_ai::provider::openai_responses::{ResponsesConfig, ResponsesProvider};
use zcode_ai::provider::xai;
use zcode_ai::{AuthStore, Effort, Provider, ProviderId, Thinking};
use zcode_catalog::descriptors;
use zcode_catalog::models::BundledCatalog;
use zcode_catalog::spec::ModelSpec;
use zcode_catalog::thinking::{ThinkingControlMode, resolve_model_thinking};

use crate::config::Config;

/// 自动选择候选集，按优先级从高到低排列。理由见模块文档「自动选择顺序」。
const PRIORITY: [ProviderId; 5] = [
    ProviderId::Anthropic,
    ProviderId::OpenAiCodex,
    ProviderId::OpenAi,
    ProviderId::XaiOAuth,
    ProviderId::Xai,
];

/// 内置目录未给出 `limit.context` 时的保守兜底值。
///
/// 本仓没有对这个数字做过实测，也没有找到对应的上游依据——纯粹是"明显不会比
/// 模型真实上限更大"的保守选择：128K 低于当前（2026）本仓五条线路里绝大多数
/// 模型的实际上限，用它做压缩/预算判断阈值时，宁可提前触发压缩，也不要在未知
/// 的真实上限之上继续往请求里堆内容而被上游拒绝。
const FALLBACK_CONTEXT_WINDOW: u64 = 128_000;

/// 零候选时"你是不是想输入"建议的条数上限。
///
/// 没有上游依据，纯粹是本仓选定的经验值：错误消息里列出全部候选没有意义，
/// 5 条足够覆盖真实的拼写/简写失误，同时不至于刷屏。
const MAX_SUGGESTIONS: usize = 5;

/// 已解析、可直接用于建 provider 与发请求的模型选择。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedModel {
    /// 线上模型 id，直接可用于 `zcode_ai::CompletionRequest::new`。
    pub(crate) id: String,
    /// 归属的提供商线路。
    pub(crate) provider: ProviderId,
    /// 上下文窗口容量（token）。内置目录未给出时用 [`FALLBACK_CONTEXT_WINDOW`]。
    pub(crate) context_window: u64,
    /// 本次会话要下发的思考配置；模型不支持或未请求时为 `Thinking::Disabled`。
    pub(crate) thinking: Thinking,
}

/// 模型解析 / 装配失败的原因。面向用户的中文说明，可操作的错误都带具体命令。
#[derive(Debug, thiserror::Error)]
pub(crate) enum ModelError {
    /// `config.model.provider` 不是可识别的 provider 标识串。
    #[error(
        "未知的 provider 覆盖值 \"{0}\"，可选值：anthropic / openai / openai-codex / xai / xai-oauth"
    )]
    UnknownProviderOverride(String),
    /// `config.model.thinking` 不是四档取值之一。
    #[error("未知的思考档位 \"{0}\"，可选值：off / low / medium / high")]
    UnknownThinking(String),
    /// 模糊匹配命中多个候选，拒绝静默选择。
    #[error("{0}")]
    AmbiguousModel(String),
    /// 模糊匹配没有命中任何候选。
    #[error("{0}")]
    UnknownModel(String),
    /// 目标模型对应的线路本地没有配置凭据。
    #[error("{0}")]
    MissingCredentials(String),
    /// 目录里没有登记该 provider 的默认模型（正常配置下不应触发，属于目录一致性问题）。
    #[error("provider \"{0}\" 在内置目录里没有默认模型")]
    NoDefaultModel(ProviderId),
    /// 构造具体的推理适配器失败。
    #[error("构造 {provider} 的推理适配器失败：{source}")]
    Provider {
        /// 构造失败的线路。
        provider: ProviderId,
        /// 底层错误。
        #[source]
        source: zcode_ai::AiError,
    },
    /// 初始化凭据存储失败。
    #[error("初始化凭据存储失败：{0}")]
    Auth(#[from] zcode_ai::AuthError),
    /// 凭据存储发现任务被中断（`spawn_blocking` 的宿主任务 panic 或被取消）。
    #[error("凭据初始化任务被中断：{0}")]
    AuthTaskJoin(#[from] tokio::task::JoinError),
}

/// 目录查询 + 凭据发现 -> provider 实例。`override_id` 来自 `--model`。
///
/// 凭据存储的发现（打开/创建 `~/.zcode/auth.json` 及其跨进程锁文件）挪进
/// `spawn_blocking`：虽然文件很小，但锁文件在多进程并发登录时可能短暂阻塞，
/// 不适合直接放在调用方的执行器线程上等。
pub(crate) async fn build(
    config: &Config,
    override_id: Option<&str>,
) -> Result<(Arc<dyn Provider>, ResolvedModel), ModelError> {
    let resolved = resolve_model(config, override_id)?;
    let auth = Arc::new(tokio::task::spawn_blocking(AuthStore::discover).await??);
    let provider = construct_provider(resolved.provider, &resolved.id, auth)?;
    Ok((provider, resolved))
}

/// 只解析不建 provider，供 `zcode models` 与错误提示用。
pub(crate) fn resolve_model(
    config: &Config,
    override_id: Option<&str>,
) -> Result<ResolvedModel, ModelError> {
    let env_model = std::env::var("ZCODE_MODEL").ok();
    let available = available_providers();
    resolve_core(
        config,
        override_id,
        env_model.as_deref(),
        &available,
        &BundledModelCatalog,
    )
}

/// 目录查询的最小接口，把"匹配算法"与"内置 `models.json` 的具体内容"解耦。
///
/// 生产实现（[`BundledModelCatalog`]）打 [`BundledCatalog`]；测试注入固定候选表，
/// 不依赖会被 `gen-models` 生成器随时覆写的真实目录内容——回归测试要打在
/// 解析器/描述符上，不要打在内置 JSON 上（见 `rule://zcode-architecture`）。
trait ModelCatalog {
    /// 某个 catalog provider id（如 `"anthropic"`）下的全部模型 id。
    fn model_ids(&self, catalog_provider_id: &str) -> Vec<Box<str>>;
    /// 精确取一条模型规格；不存在或查询失败时返回 `None`。
    fn model_spec(&self, catalog_provider_id: &str, model_id: &str) -> Option<ModelSpec>;
}

/// [`ModelCatalog`] 的生产实现，打内置目录。
struct BundledModelCatalog;

impl ModelCatalog for BundledModelCatalog {
    fn model_ids(&self, catalog_provider_id: &str) -> Vec<Box<str>> {
        match BundledCatalog::provider_model_ids(catalog_provider_id) {
            Ok(ids) => ids,
            Err(err) => {
                tracing::warn!(provider = catalog_provider_id, error = %err, "查询内置模型目录失败");
                Vec::new()
            }
        }
    }

    fn model_spec(&self, catalog_provider_id: &str, model_id: &str) -> Option<ModelSpec> {
        match BundledCatalog::model(catalog_provider_id, model_id) {
            Ok(Some(model_ref)) => Some(model_ref.spec().clone()),
            Ok(None) => None,
            Err(err) => {
                tracing::warn!(provider = catalog_provider_id, model = model_id, error = %err, "查询内置模型目录失败");
                None
            }
        }
    }
}

/// [`resolve_model`] 的纯函数核心：所有依赖都以参数注入，不接触真实环境变量、
/// 凭据文件或内置 JSON，供测试直接调用。
fn resolve_core(
    config: &Config,
    override_id: Option<&str>,
    env_model: Option<&str>,
    available: &HashSet<ProviderId>,
    catalog: &dyn ModelCatalog,
) -> Result<ResolvedModel, ModelError> {
    let provider_override = match config.model.provider.as_deref() {
        Some(raw) => Some(
            ProviderId::parse(raw)
                .ok_or_else(|| ModelError::UnknownProviderOverride(raw.to_owned()))?,
        ),
        None => None,
    };

    let requested = override_id.or(env_model).or(config.model.id.as_deref());

    let (provider, model_id) = match requested {
        Some(query) => resolve_by_query(query, provider_override, available, catalog)?,
        None => resolve_by_auto_select(provider_override, available)?,
    };

    let requested_effort = match config.model.thinking.as_deref() {
        Some(raw) => parse_thinking_setting(raw)?,
        None => Effort::Off,
    };

    let catalog_id = catalog_provider_id(provider);
    let spec = catalog.model_spec(catalog_id, &model_id);
    let context_window = spec
        .as_ref()
        .and_then(|spec| spec.limit.context)
        .map_or(FALLBACK_CONTEXT_WINDOW, u64::from);
    let thinking = resolve_thinking(requested_effort, catalog_id, &model_id, spec.as_ref());

    Ok(ResolvedModel {
        id: String::from(model_id),
        provider,
        context_window,
        thinking,
    })
}

/// 按用户给出的查询串（精确/模糊）解析出具体的 `(线路, 模型 id)`。
fn resolve_by_query(
    query: &str,
    provider_override: Option<ProviderId>,
    available: &HashSet<ProviderId>,
    catalog: &dyn ModelCatalog,
) -> Result<(ProviderId, Box<str>), ModelError> {
    let namespaces = search_namespaces(provider_override);

    let mut exact = Vec::new();
    let mut fuzzy = Vec::new();
    let needle = query.to_ascii_lowercase();
    for &ns in &namespaces {
        for id in catalog.model_ids(ns) {
            if id.as_ref() == query {
                exact.push((ns, id));
            } else if id.to_ascii_lowercase().contains(&needle) {
                fuzzy.push((ns, id));
            }
        }
    }

    let hits = if exact.is_empty() { fuzzy } else { exact };

    let (catalog_id, model_id) = match hits.as_slice() {
        [] => {
            let all_namespaces = search_namespaces(None);
            let suggestions = suggest(query, &all_namespaces, catalog);
            return Err(ModelError::UnknownModel(unknown_model_message(
                query,
                &suggestions,
            )));
        }
        [(ns, id)] => (*ns, id.clone()),
        multiple => {
            let candidates: Vec<String> = multiple
                .iter()
                .map(|(ns, id)| format!("{ns}/{id}"))
                .collect();
            return Err(ModelError::AmbiguousModel(ambiguous_model_message(
                query,
                &candidates,
            )));
        }
    };

    if let Some(provider) = provider_override {
        if available.contains(&provider) {
            return Ok((provider, model_id));
        }
        return Err(ModelError::MissingCredentials(missing_credentials_message(
            &[provider],
        )));
    }

    let group = lines_for_catalog(catalog_id);
    match group
        .iter()
        .copied()
        .find(|provider| available.contains(provider))
    {
        Some(provider) => Ok((provider, model_id)),
        None => Err(ModelError::MissingCredentials(missing_credentials_message(
            &group,
        ))),
    }
}

/// 没有任何显式模型请求时：按 [`PRIORITY`] 顺序挑第一条已配置凭据的线路，
/// 再取该线路目录里登记的默认模型。
fn resolve_by_auto_select(
    provider_override: Option<ProviderId>,
    available: &HashSet<ProviderId>,
) -> Result<(ProviderId, Box<str>), ModelError> {
    let candidates: Vec<ProviderId> = match provider_override {
        Some(provider) => vec![provider],
        None => PRIORITY.to_vec(),
    };

    let provider = candidates
        .iter()
        .copied()
        .find(|provider| available.contains(provider))
        .ok_or_else(|| ModelError::MissingCredentials(missing_credentials_message(&candidates)))?;

    let catalog_id = catalog_provider_id(provider);
    let default_model = descriptors::descriptor(catalog_id)
        .and_then(|descriptor| descriptor.default_model)
        .ok_or(ModelError::NoDefaultModel(provider))?;

    Ok((provider, Box::from(default_model)))
}

/// 待搜索的 catalog provider id 集合：显式指定线路时只搜那一个命名空间，
/// 否则搜全部三个（按 [`PRIORITY`] 首次出现的顺序去重）。
fn search_namespaces(provider_override: Option<ProviderId>) -> Vec<&'static str> {
    if let Some(provider) = provider_override {
        vec![catalog_provider_id(provider)]
    } else {
        let mut namespaces: Vec<&'static str> = Vec::new();
        for provider in PRIORITY {
            let id = catalog_provider_id(provider);
            if !namespaces.contains(&id) {
                namespaces.push(id);
            }
        }
        namespaces
    }
}

/// 零候选时的"你是不是想输入"建议：去掉双方的非字母数字字符后再做子串匹配，
/// 覆盖 `gpt5` 匹配不上 `gpt-5.6` 这类因分隔符导致的失配。
fn suggest(query: &str, namespaces: &[&str], catalog: &dyn ModelCatalog) -> Vec<String> {
    let normalized_query = normalize(query);
    if normalized_query.is_empty() {
        return Vec::new();
    }
    let mut hits = Vec::new();
    for &ns in namespaces {
        for id in catalog.model_ids(ns) {
            let normalized_id = normalize(&id);
            if normalized_id.contains(&normalized_query)
                || normalized_query.contains(&normalized_id)
            {
                hits.push(format!("{ns}/{id}"));
                if hits.len() >= MAX_SUGGESTIONS {
                    return hits;
                }
            }
        }
    }
    hits
}

/// 只保留 ASCII 字母数字并转小写，供 [`suggest`] 做去分隔符匹配。
fn normalize(s: &str) -> String {
    s.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// 某条线路对应的内置目录 provider id。`OpenAi` / `OpenAiCodex` 共享
/// `models.json` 的 `openai` 条目（同一批线上模型 id，只是鉴权与 wire 不同），
/// `Xai` / `XaiOAuth` 同理共享 `xai` 条目——两者都已用
/// `node -e "JSON.parse(...)"` 核实过 `models.json` 顶层确实没有单独的
/// `openai-codex` / `xai-oauth` 键。
const fn catalog_provider_id(provider: ProviderId) -> &'static str {
    match provider {
        ProviderId::Anthropic => "anthropic",
        ProviderId::OpenAi | ProviderId::OpenAiCodex => "openai",
        ProviderId::Xai | ProviderId::XaiOAuth => "xai",
    }
}

/// 共享同一个 catalog 命名空间的全部线路，按 [`PRIORITY`] 的顺序排列。
/// 用于"已经确定要用哪个模型，但还不知道走哪条线路"时的信用挑选。
fn lines_for_catalog(catalog_id: &str) -> Vec<ProviderId> {
    PRIORITY
        .into_iter()
        .filter(|&provider| catalog_provider_id(provider) == catalog_id)
        .collect()
}

/// 把 `config.model.thinking` 的四档字符串解析成 [`Effort`]；`None` 视为 `off`。
fn parse_thinking_setting(raw: &str) -> Result<Effort, ModelError> {
    match raw {
        "off" => Ok(Effort::Off),
        "low" => Ok(Effort::Low),
        "medium" => Ok(Effort::Medium),
        "high" => Ok(Effort::High),
        other => Err(ModelError::UnknownThinking(other.to_owned())),
    }
}

/// 把请求的思考档位转成具体要发的 [`Thinking`]。
///
/// 模型没有可控思考阶梯（`spec` 未知，或
/// [`resolve_model_thinking`] 返回 `None`）时静默降级为 `Thinking::Disabled`，
/// 只记 `tracing::debug!`，不报错——这是产品需求：用户没有为每个模型单独配置
/// 思考档位的义务，选了一个不支持思考的模型不该导致启动失败。
fn resolve_thinking(
    requested: Effort,
    catalog_id: &str,
    model_id: &str,
    spec: Option<&ModelSpec>,
) -> Thinking {
    let Some(spec) = spec else {
        if requested != Effort::Off {
            tracing::debug!(model_id, "模型规格未知，思考已禁用");
        }
        return Thinking::Disabled;
    };

    let Some(thinking_config) = resolve_model_thinking(catalog_id, model_id, spec) else {
        if requested != Effort::Off {
            tracing::debug!(model_id, "模型不支持可控思考档位，已静默降级为关闭");
        }
        return Thinking::Disabled;
    };

    let effective = thinking_config.clamp(requested);
    if effective == Effort::Off {
        // 无论线上模式是什么,钳位后仍落在 Off 就是关闭思考——不需要按模式分支。
        return Thinking::Disabled;
    }

    match thinking_config.mode {
        ThinkingControlMode::Effort => Thinking::Effort(effective),
        ThinkingControlMode::Budget => {
            if let Some(tokens) = thinking_config.budget(effective) {
                Thinking::Budget { tokens }
            } else {
                tracing::debug!(model_id, ?effective, "思考档位缺少预算映射，已禁用思考");
                Thinking::Disabled
            }
        }
        ThinkingControlMode::GoogleLevel | ThinkingControlMode::AnthropicAdaptive => {
            // 本仓五条线路目前不会产出这两种模式(只有尚未接入的 Google Gemini
            // 才会走到 `google_thinking()`),这里只是防御式兜底。
            tracing::debug!(
                model_id,
                mode = ?thinking_config.mode,
                "本仓五条线路暂不支持该思考控制模式，已禁用思考"
            );
            Thinking::Disabled
        }
    }
}

/// 本地（不联网）判定各线路是否已配置凭据。
///
/// "已配置"只看凭据文件里有没有该 provider 的记录，或者对应环境变量是否非空——
/// 不校验 OAuth token 是否已过期。过期只影响后续刷新能不能成功，不改变
/// "已登录过"这个粗粒度信号；细粒度校验留给 `AuthStore::access` 在真正发请求时做。
fn available_providers() -> HashSet<ProviderId> {
    let store = match FileCredentialStore::discover() {
        Ok(store) => Some(store),
        Err(err) => {
            tracing::debug!(error = %err, "打开凭据存储失败，按无凭据处理");
            None
        }
    };
    PRIORITY
        .into_iter()
        .filter(|&provider| {
            has_stored_credential(store.as_ref(), provider) || has_env_credential(provider)
        })
        .collect()
}

/// 凭据文件里是否有该 provider 的记录。
fn has_stored_credential(store: Option<&FileCredentialStore>, provider: ProviderId) -> bool {
    store
        .and_then(|store| store.load(provider).ok())
        .flatten()
        .is_some()
}

/// [`ProviderId::bearer_env`] 列出的环境变量里是否有非空取值。
fn has_env_credential(provider: ProviderId) -> bool {
    provider
        .bearer_env()
        .iter()
        .any(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty()))
}

/// 生成"未登录"错误文案。纯函数：只做字符串拼接，不接触任何 I/O，独立可测。
///
/// 必须给出可执行的修复命令，参考 oh-my-pi 错误消息带修复指令的做法
/// （`packages/coding-agent/src/tools/approval.ts:201-210`：给方向，不只报错）。
fn missing_credentials_message(providers: &[ProviderId]) -> String {
    let names: Vec<&str> = providers.iter().map(|provider| provider.as_str()).collect();
    let commands: Vec<String> = providers
        .iter()
        .map(|provider| format!("zcode auth login {}", provider.as_str()))
        .collect();
    format!(
        "模型需要以下提供商之一的凭据，但均未登录：{}。请先运行 {} 完成登录后重试。",
        names.join(" 或 "),
        commands.join(" 或 "),
    )
}

/// 生成"多个候选"错误文案。
fn ambiguous_model_message(query: &str, candidates: &[String]) -> String {
    format!(
        "模型 \"{query}\" 匹配到多个候选，请用完整 id 或加 `--provider` 消歧：{}",
        candidates.join("、"),
    )
}

/// 生成"零候选"错误文案。
fn unknown_model_message(query: &str, suggestions: &[String]) -> String {
    if suggestions.is_empty() {
        format!("未找到匹配 \"{query}\" 的模型。用 `zcode models` 查看可用模型列表。")
    } else {
        format!(
            "未找到匹配 \"{query}\" 的模型，你是不是想输入：{}",
            suggestions.join("、"),
        )
    }
}

/// 按 [`ResolvedModel::provider`] 构造具体的推理适配器。
fn construct_provider(
    provider: ProviderId,
    model_id: &str,
    auth: Arc<AuthStore>,
) -> Result<Arc<dyn Provider>, ModelError> {
    let wrap = |source: zcode_ai::AiError| ModelError::Provider { provider, source };
    match provider {
        ProviderId::Anthropic => Ok(Arc::new(AnthropicProvider::new(auth).map_err(wrap)?)),
        ProviderId::OpenAi => {
            // "openai" 描述符的 wire 固定为 `WireFormat::OpenAiResponses`
            // （`crates/catalog/src/descriptors.rs:189-192` 的注释：ChatGPT 现在
            // 力推的那条线，`ResponsesConfig::openai()` 就是它的默认配置）。
            Ok(Arc::new(
                ResponsesProvider::new(auth, ResponsesConfig::openai()).map_err(wrap)?,
            ))
        }
        ProviderId::OpenAiCodex => Ok(Arc::new(openai_codex::provider(auth).map_err(wrap)?)),
        ProviderId::Xai => Ok(Arc::new(xai::chat_provider(auth, model_id).map_err(wrap)?)),
        ProviderId::XaiOAuth => Ok(Arc::new(xai::oauth_provider(auth).map_err(wrap)?)),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use zcode_agent::ApprovalMode;

    use super::*;
    use crate::config::{
        ApprovalConfig, DaemonConfig, ModelConfig, SessionConfig, ToolsConfig, UiConfig,
    };

    /// 固定候选表，不依赖真实 `models.json`（见模块内 [`ModelCatalog`] 文档）。
    struct FakeCatalog {
        models: BTreeMap<&'static str, Vec<&'static str>>,
    }

    impl FakeCatalog {
        fn new(models: &[(&'static str, &[&'static str])]) -> Self {
            Self {
                models: models.iter().map(|&(ns, ids)| (ns, ids.to_vec())).collect(),
            }
        }
    }

    impl ModelCatalog for FakeCatalog {
        fn model_ids(&self, catalog_provider_id: &str) -> Vec<Box<str>> {
            self.models
                .get(catalog_provider_id)
                .map(|ids| ids.iter().map(|id| Box::from(*id)).collect())
                .unwrap_or_default()
        }

        fn model_spec(&self, _catalog_provider_id: &str, _model_id: &str) -> Option<ModelSpec> {
            None
        }
    }

    fn test_catalog() -> FakeCatalog {
        FakeCatalog::new(&[
            (
                "anthropic",
                &["claude-sonnet-5", "claude-opus-4-6", "claude-haiku-4"],
            ),
            ("openai", &["gpt-5.6", "gpt-5.3-codex", "gpt-4o-mini"]),
            ("xai", &["grok-4.5", "grok-3-mini"]),
        ])
    }

    fn test_config(id: Option<&str>, thinking: Option<&str>, provider: Option<&str>) -> Config {
        Config {
            model: ModelConfig {
                id: id.map(str::to_owned),
                thinking: thinking.map(str::to_owned),
                provider: provider.map(str::to_owned),
            },
            approval: ApprovalConfig {
                mode: ApprovalMode::default(),
                policies: std::collections::HashMap::new(),
            },
            tools: ToolsConfig {
                disabled: Vec::new(),
                bash_timeout_secs: 120,
                read_max_lines: 300,
            },
            session: SessionConfig {
                dir: PathBuf::from("."),
            },
            daemon: DaemonConfig {
                enabled: false,
                runtime_dir: PathBuf::from("."),
            },
            ui: UiConfig {
                show_thinking: false,
            },
        }
    }

    fn all_available() -> HashSet<ProviderId> {
        PRIORITY.into_iter().collect()
    }

    #[test]
    fn exact_id_hits_the_right_provider() {
        let config = test_config(None, None, None);
        let catalog = test_catalog();
        let resolved = resolve_core(&config, Some("grok-4.5"), None, &all_available(), &catalog)
            .expect("应命中");
        assert_eq!(resolved.id, "grok-4.5");
        assert_eq!(resolved.provider, ProviderId::XaiOAuth);
    }

    #[test]
    fn unique_abbreviation_hits_a_single_candidate() {
        let config = test_config(None, None, None);
        let catalog = test_catalog();
        let resolved = resolve_core(&config, Some("sonnet"), None, &all_available(), &catalog)
            .expect("应命中");
        assert_eq!(resolved.id, "claude-sonnet-5");
        assert_eq!(resolved.provider, ProviderId::Anthropic);
    }

    #[test]
    fn ambiguous_abbreviation_lists_all_candidates_instead_of_picking_one() {
        let config = test_config(None, None, None);
        let catalog = test_catalog();
        let err = resolve_core(&config, Some("gpt"), None, &all_available(), &catalog)
            .expect_err("应报错而不是静默选第一个");
        let message = err.to_string();
        assert!(message.contains("openai/gpt-5.6"), "{message}");
        assert!(message.contains("openai/gpt-5.3-codex"), "{message}");
        assert!(message.contains("openai/gpt-4o-mini"), "{message}");
    }

    #[test]
    fn unknown_query_reports_no_candidates() {
        let config = test_config(None, None, None);
        let catalog = test_catalog();
        let err = resolve_core(
            &config,
            Some("totally-unknown-xyz"),
            None,
            &all_available(),
            &catalog,
        )
        .expect_err("不存在的查询必须报错");
        assert!(matches!(err, ModelError::UnknownModel(_)));
    }

    #[test]
    fn priority_chain_prefers_override_over_env_and_config() {
        let config = test_config(Some("gpt-5.6"), None, None);
        let catalog = test_catalog();
        let resolved = resolve_core(
            &config,
            Some("grok-4.5"),
            Some("claude-sonnet-5"),
            &all_available(),
            &catalog,
        )
        .expect("override 应压过 env 与 config");
        assert_eq!(resolved.id, "grok-4.5");
    }

    #[test]
    fn priority_chain_prefers_env_over_config_when_no_override() {
        let config = test_config(Some("gpt-5.6"), None, None);
        let catalog = test_catalog();
        let resolved = resolve_core(
            &config,
            None,
            Some("claude-sonnet-5"),
            &all_available(),
            &catalog,
        )
        .expect("env 应压过 config");
        assert_eq!(resolved.id, "claude-sonnet-5");
    }

    #[test]
    fn priority_chain_falls_back_to_config_when_nothing_else_set() {
        let config = test_config(Some("gpt-5.6"), None, None);
        let catalog = test_catalog();
        let resolved = resolve_core(&config, None, None, &all_available(), &catalog)
            .expect("没有 override/env 时应使用 config.model.id");
        assert_eq!(resolved.id, "gpt-5.6");
    }

    #[test]
    fn auto_select_follows_priority_order_among_available_providers() {
        let config = test_config(None, None, None);
        let catalog = test_catalog();
        let available: HashSet<ProviderId> =
            [ProviderId::OpenAi, ProviderId::Xai].into_iter().collect();
        let resolved = resolve_core(&config, None, None, &available, &catalog)
            .expect("OpenAi 排在 Xai 之前应该被优先选中");
        assert_eq!(resolved.provider, ProviderId::OpenAi);
        assert_eq!(resolved.id, "gpt-5.6");
    }

    #[test]
    fn auto_select_without_any_credential_reports_all_login_commands() {
        let config = test_config(None, None, None);
        let catalog = test_catalog();
        let err = resolve_core(&config, None, None, &HashSet::new(), &catalog)
            .expect_err("没有任何凭据必须报错，不能瞎猜一个线路");
        let message = err.to_string();
        assert!(message.contains("zcode auth login anthropic"), "{message}");
        assert!(message.contains("zcode auth login xai"), "{message}");
    }

    #[test]
    fn explicit_provider_override_restricts_the_search_namespace() {
        let config = test_config(None, None, Some("anthropic"));
        let catalog = test_catalog();
        // "codex" 只在 openai 命名空间里存在,pin 到 anthropic 后必须找不到。
        let err = resolve_core(&config, Some("codex"), None, &all_available(), &catalog)
            .expect_err("pin 到 anthropic 后不应该跨命名空间命中 openai 的模型");
        assert!(matches!(err, ModelError::UnknownModel(_)));
    }

    #[test]
    fn missing_credentials_message_contains_an_actionable_command() {
        let message = missing_credentials_message(&[ProviderId::Anthropic]);
        assert!(
            message.contains("zcode auth login anthropic"),
            "错误文案必须包含可执行命令: {message}"
        );
    }

    #[test]
    fn missing_credentials_message_lists_every_line_in_the_group() {
        let message = missing_credentials_message(&[ProviderId::OpenAiCodex, ProviderId::OpenAi]);
        assert!(
            message.contains("zcode auth login openai-codex"),
            "{message}"
        );
        assert!(message.contains("zcode auth login openai"), "{message}");
    }

    #[test]
    fn thinking_disabled_when_model_spec_unknown() {
        assert_eq!(
            resolve_thinking(Effort::High, "anthropic", "claude-sonnet-5", None),
            Thinking::Disabled
        );
    }

    #[test]
    fn suggest_matches_across_separator_normalization() {
        let catalog = test_catalog();
        let namespaces = ["openai"];
        let suggestions = suggest("gpt56", &namespaces, &catalog);
        assert!(
            suggestions.iter().any(|s| s == "openai/gpt-5.6"),
            "{suggestions:?}"
        );
    }
}
