//! 提供商静态描述符表：怎么连、默认用哪个模型、能不能运行时发现。
//!
//! 每条描述符的 `id` 必须与 [`crate::spec::CatalogFile`]（即 `models.json`）的顶层键
//! 逐字对应——除非它标了 [`ProviderDescriptor::discovery_only`]。下方的
//! `non_discovery_only_ids_exist_in_bundled_catalog` 测试强制这条约束：
//! oh-my-pi 的 `descriptors.ts` 那张 68 条静态表已经跟生成物漂移到 64 个 key 还在用
//! 不安全 cast 掩盖（`packages/catalog/src/provider-models/descriptors.ts:69-550`），
//! 本仓不重蹈覆辙。
//!
//! 三个本地端点（`ollama` / `lmstudio` / `vllm`）标了 `discovery_only = true`：
//! 它们只能靠运行时探测 `{base_url}/models` 发现模型，绝不进内置目录——把某台机器
//! 上跑着的本地模型烤进 `models.json` 会泄露生成者本机的配置。`lmstudio` 虽然
//! 恰好在当前 `models.json` 里有一条真实记录（上游生成时命中过某台机器的 LM Studio
//! 实例，`base_url` 指向 `127.0.0.1`），但那正是本文档警告的债——这里刻意不把它
//! 当作"内置默认模型"的来源，`default_model` 仍是 `None`。`ollama` / `vllm` 干脆没有
//! 对应的顶层键。

use std::env;

/// 一个提供商的静态描述:怎么连、默认用哪个模型、能不能运行时发现。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderDescriptor {
    /// 与 `models.json` 里的顶层键一致(`discovery_only` 时例外)。
    pub id: &'static str,
    /// 人类可读名称。
    pub display_name: &'static str,
    /// OpenAI 兼容 base URL;非兼容线路(Anthropic Messages 等)也写它自己的根。
    pub base_url: &'static str,
    /// 承载 API key 的环境变量,按优先级排列;本地端点可以是空切片。
    pub env_vars: &'static [&'static str],
    /// 默认模型 id;无内置默认时 `None`(本地端点、纯运行时发现的提供商)。
    pub default_model: Option<&'static str>,
    /// 能否调运行时发现端点补全模型列表。
    pub discovery: DiscoveryKind,
    /// 只靠运行时发现,不进内置目录(ollama / lmstudio / vllm 这类本地端点)。
    ///
    /// 前提:把它们烤进生成物会泄露生成者本机的 `http://127.0.0.1:...` 配置,
    /// 详见模块文档。
    pub discovery_only: bool,
    /// 线格式,决定用哪个适配器。
    pub wire: WireFormat,
}

/// 运行时模型发现的方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoveryKind {
    /// 不支持运行时发现,模型列表只能来自内置目录。
    None,
    /// `GET {base_url}/models`,返回 OpenAI `/v1/models` 那种 `{"data": [...]}` 形态。
    OpenAiModels,
}

/// 线格式,决定请求/响应体怎么拼、走哪个适配器。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireFormat {
    /// Anthropic 的 `/v1/messages`。
    AnthropicMessages,
    /// OpenAI 的 `/chat/completions`(以及照抄这套形态的兼容提供商)。
    OpenAiChat,
    /// OpenAI 新的 `/responses`。
    OpenAiResponses,
}

/// 全部描述符,按 `id` 字典序严格递增排列(`descriptor` 靠这个做二分查找)。
///
/// `base_url` 与 `default_model` 的取值依据:
/// - 有 `api` 字段的提供商(openrouter / deepseek / moonshotai / zhipuai /
///   fireworks-ai)直接照抄 `models.json` 里的 `api` 字符串。
/// - 没有 `api` 字段的(anthropic / openai / xai / google / groq / mistral /
///   togetherai / cerebras)用各家官方文档公开的默认根地址——`models.json` 生成器
///   本就没打算给这些线路填 `api`,不是遗漏。
/// - 每个 `default_model` 都是该提供商 `models.json` 模型表里真实存在的 id
///   (由 `default_models_exist_in_their_provider` 测试兜底)。
pub const PROVIDERS: &[ProviderDescriptor] = &[
    ProviderDescriptor {
        id: "anthropic",
        display_name: "Anthropic",
        base_url: "https://api.anthropic.com",
        env_vars: &["ANTHROPIC_API_KEY"],
        default_model: Some("claude-sonnet-5"),
        // Anthropic 有 `/v1/models`,但响应形态跟 OpenAI 的不是一回事,
        // 不能塞进 `DiscoveryKind::OpenAiModels`(它专指 OpenAI 那套 schema)。
        discovery: DiscoveryKind::None,
        discovery_only: false,
        wire: WireFormat::AnthropicMessages,
    },
    ProviderDescriptor {
        id: "cerebras",
        display_name: "Cerebras",
        base_url: "https://api.cerebras.ai/v1",
        env_vars: &["CEREBRAS_API_KEY"],
        default_model: Some("gpt-oss-120b"),
        discovery: DiscoveryKind::OpenAiModels,
        discovery_only: false,
        wire: WireFormat::OpenAiChat,
    },
    ProviderDescriptor {
        id: "deepseek",
        display_name: "DeepSeek",
        base_url: "https://api.deepseek.com",
        env_vars: &["DEEPSEEK_API_KEY"],
        default_model: Some("deepseek-chat"),
        discovery: DiscoveryKind::OpenAiModels,
        discovery_only: false,
        wire: WireFormat::OpenAiChat,
    },
    ProviderDescriptor {
        id: "fireworks-ai",
        display_name: "Fireworks AI",
        base_url: "https://api.fireworks.ai/inference/v1/",
        env_vars: &["FIREWORKS_API_KEY"],
        default_model: Some("accounts/fireworks/models/kimi-k3"),
        discovery: DiscoveryKind::OpenAiModels,
        discovery_only: false,
        wire: WireFormat::OpenAiChat,
    },
    ProviderDescriptor {
        id: "google",
        display_name: "Google",
        base_url: "https://generativelanguage.googleapis.com/v1beta/openai",
        env_vars: &[
            "GOOGLE_API_KEY",
            "GOOGLE_GENERATIVE_AI_API_KEY",
            "GEMINI_API_KEY",
        ],
        default_model: Some("gemini-2.5-pro"),
        discovery: DiscoveryKind::OpenAiModels,
        discovery_only: false,
        wire: WireFormat::OpenAiChat,
    },
    ProviderDescriptor {
        id: "groq",
        display_name: "Groq",
        base_url: "https://api.groq.com/openai/v1",
        env_vars: &["GROQ_API_KEY"],
        default_model: Some("llama-3.3-70b-versatile"),
        discovery: DiscoveryKind::OpenAiModels,
        discovery_only: false,
        wire: WireFormat::OpenAiChat,
    },
    ProviderDescriptor {
        id: "lmstudio",
        display_name: "LMStudio",
        base_url: "http://127.0.0.1:1234/v1",
        env_vars: &["LMSTUDIO_API_KEY"],
        default_model: None,
        discovery: DiscoveryKind::OpenAiModels,
        discovery_only: true,
        wire: WireFormat::OpenAiChat,
    },
    ProviderDescriptor {
        id: "mistral",
        display_name: "Mistral",
        base_url: "https://api.mistral.ai/v1",
        env_vars: &["MISTRAL_API_KEY"],
        default_model: Some("mistral-large-latest"),
        discovery: DiscoveryKind::OpenAiModels,
        discovery_only: false,
        wire: WireFormat::OpenAiChat,
    },
    ProviderDescriptor {
        id: "moonshotai",
        display_name: "Moonshot AI",
        base_url: "https://api.moonshot.ai/v1",
        env_vars: &["MOONSHOT_API_KEY"],
        default_model: Some("kimi-k2-thinking"),
        discovery: DiscoveryKind::OpenAiModels,
        discovery_only: false,
        wire: WireFormat::OpenAiChat,
    },
    ProviderDescriptor {
        id: "ollama",
        display_name: "Ollama",
        base_url: "http://127.0.0.1:11434/v1",
        env_vars: &[],
        default_model: None,
        discovery: DiscoveryKind::OpenAiModels,
        discovery_only: true,
        wire: WireFormat::OpenAiChat,
    },
    ProviderDescriptor {
        id: "openai",
        display_name: "OpenAI",
        base_url: "https://api.openai.com/v1",
        env_vars: &["OPENAI_API_KEY"],
        default_model: Some("gpt-5.6"),
        discovery: DiscoveryKind::OpenAiModels,
        discovery_only: false,
        // `zcode-ai` 同时提供 `openai_chat`(Chat Completions)与
        // `openai_responses`(Responses)两套适配器;API-key 直连线路挂 Responses,
        // 因为那是 OpenAI 现在力推、`ResponsesConfig::openai()` 默认走的那条。
        wire: WireFormat::OpenAiResponses,
    },
    ProviderDescriptor {
        id: "openrouter",
        display_name: "OpenRouter",
        base_url: "https://openrouter.ai/api/v1",
        env_vars: &["OPENROUTER_API_KEY"],
        default_model: Some("anthropic/claude-sonnet-5"),
        discovery: DiscoveryKind::OpenAiModels,
        discovery_only: false,
        wire: WireFormat::OpenAiChat,
    },
    ProviderDescriptor {
        id: "togetherai",
        display_name: "Together AI",
        base_url: "https://api.together.xyz/v1",
        env_vars: &["TOGETHER_API_KEY"],
        default_model: Some("moonshotai/Kimi-K3"),
        discovery: DiscoveryKind::OpenAiModels,
        discovery_only: false,
        wire: WireFormat::OpenAiChat,
    },
    ProviderDescriptor {
        id: "vllm",
        display_name: "vLLM",
        base_url: "http://127.0.0.1:8000/v1",
        env_vars: &[],
        default_model: None,
        discovery: DiscoveryKind::OpenAiModels,
        discovery_only: true,
        wire: WireFormat::OpenAiChat,
    },
    ProviderDescriptor {
        id: "xai",
        display_name: "xAI",
        base_url: "https://api.x.ai/v1",
        env_vars: &["XAI_API_KEY"],
        default_model: Some("grok-4.5"),
        discovery: DiscoveryKind::OpenAiModels,
        discovery_only: false,
        // `zcode-ai` 的 xAI 适配器:API key 走 Chat Completions,
        // SuperGrok OAuth 才走 Responses(见 `crates/ai/CHANGELOG.md`)。
        // 这里描述的是 API-key 直连线路。
        wire: WireFormat::OpenAiChat,
    },
    ProviderDescriptor {
        id: "zhipuai",
        display_name: "Zhipu AI",
        base_url: "https://open.bigmodel.cn/api/paas/v4",
        env_vars: &["ZHIPU_API_KEY"],
        default_model: Some("glm-4.6"),
        discovery: DiscoveryKind::OpenAiModels,
        discovery_only: false,
        wire: WireFormat::OpenAiChat,
    },
];

/// 按 `id` 查描述符。[`PROVIDERS`] 按 `id` 严格递增排列,这里用二分查找。
#[must_use]
pub fn descriptor(id: &str) -> Option<&'static ProviderDescriptor> {
    PROVIDERS
        .binary_search_by(|probe| probe.id.cmp(id))
        .ok()
        .and_then(|index| PROVIDERS.get(index))
}

/// 从当前进程环境变量里取 `descriptor` 的 API key:
/// 按 [`ProviderDescriptor::env_vars`] 的顺序取第一个非空值。
#[must_use]
pub fn api_key_from_env(descriptor: &ProviderDescriptor) -> Option<String> {
    // 先把用得到的环境变量整体读一遍再传给纯函数,好让 `first_non_empty_env`
    // 不摸真实进程环境——测试才能注入假数据,不用碰 `std::env::set_var`
    // (2024 edition 里它是 `unsafe`,而且并行测试互相踩变量)。
    let snapshot: Vec<(&str, Option<String>)> = descriptor
        .env_vars
        .iter()
        .map(|&name| (name, env::var(name).ok()))
        .collect();
    let borrowed: Vec<(&str, Option<&str>)> = snapshot
        .iter()
        .map(|(name, value)| (*name, value.as_deref()))
        .collect();
    first_non_empty_env(descriptor.env_vars, &borrowed)
}

/// `api_key_from_env` 的纯函数核心:给定"环境变量名 → 取值结果"的显式列表
/// (而不是真的去读进程环境),按 `env_vars` 的顺序返回第一个非空值。
fn first_non_empty_env(env_vars: &[&str], env: &[(&str, Option<&str>)]) -> Option<String> {
    for name in env_vars {
        let found = env
            .iter()
            .find(|(key, _)| key == name)
            .and_then(|(_, value)| *value)
            .filter(|value| !value.is_empty());
        if let Some(value) = found {
            return Some(value.to_owned());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::CatalogFile;

    /// `models.json` 原文,仅供测试做集合一致性校验;运行时加载走 [`crate::models`]。
    const CATALOG_JSON: &str = include_str!("models.json");

    fn bundled_catalog() -> CatalogFile {
        serde_json::from_str(CATALOG_JSON).unwrap()
    }

    #[test]
    fn providers_are_sorted_by_id_with_no_duplicates() {
        let strictly_increasing = PROVIDERS
            .iter()
            .zip(PROVIDERS.iter().skip(1))
            .all(|(a, b)| a.id < b.id);
        assert!(
            strictly_increasing,
            "PROVIDERS 必须按 id 严格递增且无重复排列"
        );
    }

    #[test]
    fn non_discovery_only_ids_exist_in_bundled_catalog() {
        let catalog = bundled_catalog();
        for provider in PROVIDERS {
            if provider.discovery_only {
                continue;
            }
            assert!(
                catalog.contains_key(provider.id),
                "{} 不是 discovery_only,但在 models.json 里找不到",
                provider.id
            );
        }
    }

    #[test]
    fn default_models_exist_in_their_own_provider() {
        let catalog = bundled_catalog();
        for provider in PROVIDERS {
            if provider.discovery_only {
                assert!(
                    provider.default_model.is_none(),
                    "{} 是 discovery_only,不该有内置默认模型",
                    provider.id
                );
                continue;
            }
            let Some(default_model) = provider.default_model else {
                continue;
            };
            let spec = catalog
                .get(provider.id)
                .unwrap_or_else(|| panic!("{} 应已由上一条测试保证存在", provider.id));
            assert!(
                spec.models.contains_key(default_model),
                "{} 的默认模型 {default_model} 不在它自己的模型表里",
                provider.id
            );
        }
    }

    #[test]
    fn descriptor_finds_known_id_and_rejects_unknown() {
        let anthropic = descriptor("anthropic").unwrap();
        assert_eq!(anthropic.id, "anthropic");
        assert_eq!(anthropic.wire, WireFormat::AnthropicMessages);
        assert!(descriptor("does-not-exist").is_none());
    }

    #[test]
    fn first_non_empty_env_skips_missing_and_empty_then_picks_first_hit() {
        let env: [(&str, Option<&str>); 3] = [
            ("MISSING", None),
            ("EMPTY", Some("")),
            ("SET", Some("secret")),
        ];
        assert_eq!(
            first_non_empty_env(&["MISSING", "EMPTY", "SET"], &env),
            Some("secret".to_owned())
        );
    }

    #[test]
    fn first_non_empty_env_prefers_earlier_names_over_later_ones() {
        let env: [(&str, Option<&str>); 2] = [("FIRST", Some("a")), ("SECOND", Some("b"))];
        assert_eq!(
            first_non_empty_env(&["FIRST", "SECOND"], &env),
            Some("a".to_owned())
        );
    }

    #[test]
    fn first_non_empty_env_returns_none_when_nothing_usable() {
        let env: [(&str, Option<&str>); 2] = [("A", None), ("B", Some(""))];
        assert_eq!(first_non_empty_env(&["A", "B"], &env), None);
    }
}
