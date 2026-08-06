//! 内置目录 `models.json` 的磁盘形态。
//!
//! 这里的类型同时是**生成器的输出契约**与**运行时的解析契约**：
//! `gen-models` 用它们序列化，[`crate::models`] 用它们反序列化。改字段必须两侧同时成立。
//!
//! 序列化确定性由两点保证：容器一律用 [`BTreeMap`]（键按字典序），
//! 字段顺序由结构体声明顺序固定。因此同一份上游快照在任何机器上生成的字节完全一致。
//!
//! `models.json` 是生成物，绝不手工编辑（见 `rule://zcode-architecture`）。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// 一份完整目录：`提供商 id → 提供商`。
pub type CatalogFile = BTreeMap<Box<str>, ProviderSpec>;

/// 一个提供商及其模型。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderSpec {
    /// 提供商 id，等于它在 [`CatalogFile`] 里的键。
    pub id: Box<str>,
    /// 人类可读名称。
    pub name: Box<str>,
    /// 该提供商的 OpenAI 兼容 base URL；上游未给出时为 `None`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api: Option<Box<str>>,
    /// 承载 API key 的环境变量名，按上游给出的优先级排列。
    #[serde(default, skip_serializing_if = "<[Box<str>]>::is_empty")]
    pub env: Box<[Box<str>]>,
    /// `模型 id → 模型`。
    pub models: BTreeMap<Box<str>, ModelSpec>,
}

/// 一个模型的静态元数据。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelSpec {
    /// 线上模型 id，等于它在 [`ProviderSpec::models`] 里的键。
    pub id: Box<str>,
    /// 人类可读名称。
    pub name: Box<str>,
    /// 计价。**缺失表示定价未知，绝不等同于免费**——账单归因不能撒谎，
    /// 上游对约 7% 的条目不给价，把它们记成 0 会让成本统计静默偏低。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<CostSpec>,
    /// 容量上限。
    pub limit: LimitSpec,
    /// 支持的输入模态。
    pub input: Box<[Modality]>,
    /// 支持的输出模态。
    pub output: Box<[Modality]>,
    /// 是否具备推理/思考能力。注意「会推理但无可控档位」是合法状态：
    /// 此处为 `true` 而 [`crate::thinking`] 侧无 ladder。
    pub reasoning: bool,
    /// 是否支持工具调用。
    pub tool_call: bool,
    /// 生命周期状态；`None` = 正式可用。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<ModelStatus>,
}

/// 每百万 token 的美元单价。
///
/// 单位统一为 $/M token，与上游 catalog 一致；换算成单次请求成本见
/// [`crate::models::calculate_cost`]。
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct CostSpec {
    /// 未命中缓存的输入 token 单价。
    pub input: f64,
    /// 输出 token 单价。
    pub output: f64,
    /// 缓存命中读取单价；上游未给出时为 `None`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read: Option<f64>,
    /// 缓存写入单价；上游未给出时为 `None`。
    ///
    /// Anthropic 的 1h 档由 `input * 2` 推导而非取此字段——倍率是 Anthropic 公布的
    /// 模型无关常量，而存储在目录里的 `cache_write` 会随上游快照漂移。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write: Option<f64>,
    /// 上下文超过 200k token 后的阶梯单价；仅 Anthropic / Gemini 长上下文档位有。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_over_200k: Option<TierCostSpec>,
}

/// 长上下文阶梯的替代单价，字段语义与 [`CostSpec`] 同名字段一致。
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct TierCostSpec {
    /// 未命中缓存的输入 token 单价。
    pub input: f64,
    /// 输出 token 单价。
    pub output: f64,
    /// 缓存命中读取单价。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read: Option<f64>,
    /// 缓存写入单价。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write: Option<f64>,
}

/// 容量上限，单位 token。
///
/// 一律 `Option`：`None` 表示未知，绝不用 `0` 或魔数哨兵冒充
/// ——上游历史上用过 `222222` / `8888` 这类哨兵并因此出过错。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LimitSpec {
    /// 上下文窗口总量。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<u32>,
    /// 单次响应的最大输出 token。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<u32>,
    /// 单次请求的最大输入 token；仅少数提供商单独限制。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<u32>,
}

/// 内容模态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Modality {
    /// 纯文本。
    Text,
    /// 位图图像。
    Image,
    /// PDF 文档（提供商侧自行拆页）。
    Pdf,
    /// 音频。
    Audio,
    /// 视频。
    Video,
}

/// 模型生命周期状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelStatus {
    /// 早期预览，随时可能变更或下线。
    Alpha,
    /// 公测。
    Beta,
    /// 已弃用，仍可调用但会在未来移除。
    Deprecated,
}

impl ModelSpec {
    /// 该模型是否接受图像输入。
    #[must_use]
    pub fn supports_image_input(&self) -> bool {
        self.input.contains(&Modality::Image)
    }

    /// 该模型是否仍建议使用（非弃用）。
    #[must_use]
    pub fn is_usable(&self) -> bool {
        !matches!(self.status, Some(ModelStatus::Deprecated))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialization_is_deterministic_and_skips_empty_fields() {
        let spec = ModelSpec {
            id: "m".into(),
            name: "M".into(),
            cost: None,
            limit: LimitSpec {
                context: Some(1),
                output: None,
                input: None,
            },
            input: Box::from([Modality::Text]),
            output: Box::from([Modality::Text]),
            reasoning: false,
            tool_call: true,
            status: None,
        };
        let json = serde_json::to_string(&spec).unwrap();
        assert_eq!(
            json,
            r#"{"id":"m","name":"M","limit":{"context":1},"input":["text"],"output":["text"],"reasoning":false,"tool_call":true}"#
        );
        let back: ModelSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(back, spec);
    }

    #[test]
    fn deprecated_models_are_not_usable() {
        let mut spec = ModelSpec {
            id: "m".into(),
            name: "M".into(),
            cost: None,
            limit: LimitSpec::default(),
            input: Box::from([Modality::Text, Modality::Image]),
            output: Box::from([Modality::Text]),
            reasoning: true,
            tool_call: true,
            status: Some(ModelStatus::Beta),
        };
        assert!(spec.is_usable());
        assert!(spec.supports_image_input());
        spec.status = Some(ModelStatus::Deprecated);
        assert!(!spec.is_usable());
    }
}
