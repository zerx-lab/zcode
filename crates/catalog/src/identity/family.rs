//! 模型族判定、prompt 方言选择与展示名清洗。
//!
//! 全部是无缓存纯函数——参见 `mod.rs` 顶部关于无界 memo 的取舍说明。

use crate::identity::classify::{bare_model_id, strip_bracket_tags};

/// 模型族。判定用于选 prompt 方言、能力推断与 UI 分组。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModelFamily {
    /// Anthropic Claude 系列。
    Claude,
    /// OpenAI GPT 系列（`gpt-*`）。
    Gpt,
    /// OpenAI 推理系列（`o1`/`o3`/`o4-mini` 等）。
    OSeries,
    /// OpenAI Codex 系列（`codex-*`）。
    Codex,
    /// Google Gemini 系列。
    Gemini,
    /// xAI Grok 系列。
    Grok,
    /// 智谱 GLM 系列。
    Glm,
    /// 阿里 Qwen 系列。
    Qwen,
    /// `DeepSeek` 系列。
    DeepSeek,
    /// 月之暗面 Kimi 系列。
    Kimi,
    /// Meta Llama 系列。
    Llama,
    /// Mistral（含 Mixtral）系列。
    Mistral,
    /// `MiniMax` 系列。
    MiniMax,
    /// 无法识别归入的其余模型。
    Other,
}

impl ModelFamily {
    /// 族的字符串标识，用于展示与序列化。
    ///
    /// `Other` 返回空串——照搬上游 `family.ts:246-261` 的约定：未识别的族没有
    /// 一个有意义的展示名，与其发明一个不如留空让调用方自行兜底。
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Gpt => "gpt",
            Self::OSeries => "o-series",
            Self::Codex => "codex",
            Self::Gemini => "gemini",
            Self::Grok => "grok",
            Self::Glm => "glm",
            Self::Qwen => "qwen",
            Self::DeepSeek => "deepseek",
            Self::Kimi => "kimi",
            Self::Llama => "llama",
            Self::Mistral => "mistral",
            Self::MiniMax => "minimax",
            Self::Other => "",
        }
    }
}

/// 纯字符串判定，已剥前缀与方括号标签。**不引 regex**，用 `starts_with` / `contains`。
///
/// 一次转小写后全程用普通 `starts_with`/`contains` 判定，不必对每个分支各写一遍
/// 大小写变体（id 通常几十字节，一次分配的代价可忽略）。
///
/// `codex`/o-series 的判定放在 `gpt` 之前：`gpt-5.1-codex`、`o4-mini` 都以字母
/// 打头但不属于普通 GPT 分类，必须先排除掉才轮到宽松的 `gpt`/`chatgpt` 前缀判定。
#[must_use]
pub fn model_family(id: &str) -> ModelFamily {
    let lower = id.to_ascii_lowercase();
    let id = lower.as_str();

    if id.starts_with("claude") {
        ModelFamily::Claude
    } else if id.starts_with("gemini") {
        ModelFamily::Gemini
    } else if id.contains("codex") {
        ModelFamily::Codex
    } else if is_o_series(id) {
        ModelFamily::OSeries
    } else if id.starts_with("gpt") || id.starts_with("chatgpt") {
        ModelFamily::Gpt
    } else if id.starts_with("grok") {
        ModelFamily::Grok
    } else if id.starts_with("glm") {
        ModelFamily::Glm
    } else if id.starts_with("qwen") {
        ModelFamily::Qwen
    } else if id.starts_with("deepseek") {
        ModelFamily::DeepSeek
    } else if id.starts_with("kimi") {
        ModelFamily::Kimi
    } else if id.starts_with("llama") {
        ModelFamily::Llama
    } else if id.starts_with("mistral") || id.starts_with("mixtral") {
        ModelFamily::Mistral
    } else if id.starts_with("minimax") {
        ModelFamily::MiniMax
    } else {
        ModelFamily::Other
    }
}

/// `o` 后紧跟一位数字才算 o-series（`o3`、`o4-mini`），避免和 `opus`/`openai` 混淆。
fn is_o_series(id: &str) -> bool {
    let Some(rest) = id.strip_prefix('o') else {
        return false;
    };
    rest.chars().next().is_some_and(|c| c.is_ascii_digit())
}

/// 面向 prompt 的方言：不同族对工具描述的偏好不同，兜底 `Xml`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PromptDialect {
    /// 用 XML 风格标签描述工具/结构化内容——Anthropic 官方推荐写法，也是默认兜底。
    Xml,
    /// 用 Markdown 风格描述——OpenAI 系（GPT/O 系列/Codex）更吃这一套。
    Markdown,
}

/// 按模型族选择 prompt 方言；未特别声明偏好的族一律兜底 `Xml`。
#[must_use]
pub fn prompt_dialect(family: ModelFamily) -> PromptDialect {
    match family {
        ModelFamily::Gpt | ModelFamily::OSeries | ModelFamily::Codex => PromptDialect::Markdown,
        _ => PromptDialect::Xml,
    }
}

/// 展示用名称清洗：剥提供商前缀与方括号标签。
///
/// **绝不剥变体标签**（`(Thinking)` / `(free)` / `(Fast)` / 日期 / 地区 / 尺寸）
/// ——它们映射到不同的线上 id，剥掉就发错模型；这些标签用圆括号或纯文本表达，
/// [`strip_bracket_tags`] 只处理方括号，天然不会碰到它们。剥空时返回原串。
#[must_use]
pub fn clean_model_name(id: &str) -> &str {
    let cleaned = strip_bracket_tags(bare_model_id(id));
    if cleaned.is_empty() { id } else { cleaned }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_known_families() {
        assert_eq!(model_family("claude-opus-4-5"), ModelFamily::Claude);
        assert_eq!(model_family("gemini-2.5-pro"), ModelFamily::Gemini);
        assert_eq!(model_family("gpt-5.4"), ModelFamily::Gpt);
        assert_eq!(model_family("o4-mini"), ModelFamily::OSeries);
        assert_eq!(model_family("codex-mini"), ModelFamily::Codex);
        assert_eq!(model_family("gpt-5.1-codex"), ModelFamily::Codex);
        assert_eq!(model_family("grok-4"), ModelFamily::Grok);
        assert_eq!(model_family("qwen-3-max"), ModelFamily::Qwen);
        assert_eq!(model_family("deepseek-v3"), ModelFamily::DeepSeek);
        assert_eq!(model_family("kimi-k2"), ModelFamily::Kimi);
        assert_eq!(model_family("llama-3.1-70b"), ModelFamily::Llama);
        assert_eq!(model_family("mistral-large"), ModelFamily::Mistral);
        assert_eq!(model_family("minimax-m1"), ModelFamily::MiniMax);
    }

    #[test]
    fn glm_not_captured_by_gpt_or_o_series_rules() {
        assert_eq!(model_family("glm-4.6"), ModelFamily::Glm);
        assert_eq!(model_family("glm-4.5-air"), ModelFamily::Glm);
    }

    #[test]
    fn unknown_id_is_other_with_empty_str() {
        let family = model_family("some-totally-unknown-model");
        assert_eq!(family, ModelFamily::Other);
        assert_eq!(family.as_str(), "");
    }

    #[test]
    fn prompt_dialect_prefers_markdown_for_openai_family() {
        assert_eq!(prompt_dialect(ModelFamily::Gpt), PromptDialect::Markdown);
        assert_eq!(
            prompt_dialect(ModelFamily::OSeries),
            PromptDialect::Markdown
        );
        assert_eq!(prompt_dialect(ModelFamily::Codex), PromptDialect::Markdown);
        assert_eq!(prompt_dialect(ModelFamily::Claude), PromptDialect::Xml);
        assert_eq!(prompt_dialect(ModelFamily::Other), PromptDialect::Xml);
    }

    #[test]
    fn clean_model_name_strips_provider_and_bracket_tags_only() {
        assert_eq!(
            clean_model_name("openrouter/anthropic/claude-opus-4-5"),
            "claude-opus-4-5"
        );
        assert_eq!(
            clean_model_name("[Kiro] claude-opus-4-5"),
            "claude-opus-4-5"
        );
        // 变体标签必须原样保留：圆括号/日期/尺寸不是 strip_bracket_tags 的目标。
        assert_eq!(
            clean_model_name("claude-opus-4-5 (Thinking)"),
            "claude-opus-4-5 (Thinking)"
        );
        assert_eq!(clean_model_name("gpt-4o (free)"), "gpt-4o (free)");
        assert_eq!(clean_model_name("llama-3.1-70b"), "llama-3.1-70b");
    }

    #[test]
    fn clean_model_name_falls_back_to_original_when_stripped_empty() {
        assert_eq!(clean_model_name("[only-a-tag]"), "[only-a-tag]");
    }
}
