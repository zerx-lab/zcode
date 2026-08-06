//! 思考配置：控制模式、effort 阶梯、线上字段推导。
//!
//! 移植自 oh-my-pi `packages/catalog/src/types.ts:23-91`
//! （`ThinkingControlMode` / `ThinkingConfig`）与 `model-thinking.ts`
//! （ladder 表 + `resolveModelThinking`）。三张 `Partial<Record<Effort, T>>`
//! 换成定长数组：`effort_map` / `effort_budgets` 用 [`Effort::index`] 直接下标
//! （0..7，[`Effort`] 现有七档），`effort_routing` 多一个专用的“完全关闭”
//! 槽位（下标 7），见该字段文档——这是唯一形状上的出入，其余语义原样保留。

use tracing::debug;

use crate::effort::Effort;
use crate::spec::ModelSpec;

/// 提供商控制思考的方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ThinkingControlMode {
    /// OpenAI Responses 风格：`reasoning.effort` 枚举。
    Effort,
    /// Anthropic 风格：`thinking.budget_tokens` 数值预算。
    Budget,
    /// Google 风格：`thinkingConfig.thinkingLevel` 档位。
    GoogleLevel,
    /// Anthropic 自适应：只开关，不给预算。
    AnthropicAdaptive,
}

/// 单个模型的思考配置：控制模式、可用 effort 阶梯、线上字段映射。
#[derive(Debug, Clone, PartialEq)]
pub struct ThinkingConfig {
    /// 提供商控制思考的方式。
    pub mode: ThinkingControlMode,
    /// 低→高，**永不为空**。空 ladder 在构造时就要 `Err`。
    efforts: Box<[Effort]>,
    /// effort → 线上字段值（如 Google 的 `"low"`/`"high"`）。下标由
    /// [`Effort::index`] 给出。
    effort_map: [Option<Box<str>>; 7],
    /// effort → 实际要发的模型 id（同一逻辑模型分裂成多个线上 id 时）。
    ///
    /// 下标 `0..7` 对应 [`Effort::index`]，用于给某个**具体阶梯档位**单独
    /// 覆写线上 id；下标 `7` 是专用的“完全关闭”槽位，对应上游
    /// `effortRouting[effort ?? "off"]` 里字面量 `"off"` 键——与
    /// `effort_routing[Effort::Off.index()]`（“钳到 Off 这一具体档位时”的
    /// 覆写）是两件事：调用方可能压根没有选中任何阶梯、只是要关闭思考，
    /// 此时才落到下标 7。[`Self::wire_model_id`] 按“具体档位 → 关闭槽位 →
    /// 调用方兜底”三级回退查询。
    effort_routing: [Option<Box<str>>; 8],
    /// effort → `budget_tokens`。下标由 [`Effort::index`] 给出。
    effort_budgets: [Option<u32>; 7],
    /// off 时是否要**显式**发一个抑制字段（而不是省略字段）——例如 Google
    /// `thinkingLevel: "MINIMAL"` + `includeThoughts: false`：Cloud Code
    /// Assist 在字段缺失时会回填服务端预设的默认预算，省略不等于关闭。
    pub suppress_when_off: bool,
    /// 是否必须带 effort：上游拒绝真正关闭思考（如某些 `OpenRouter` Gemini
    /// 端点），`off` 请求要被钳到本模型支持的最低档，而不是省略/关闭。
    pub requires_effort: bool,
}

/// 思考配置构造与查询失败的原因。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ThinkingError {
    /// effort 阶梯为空——一个思考配置至少要有一档，否则 [`ThinkingConfig`]
    /// 就没有存在的意义。
    #[error("effort 阶梯不能为空")]
    EmptyLadder,
    /// 请求的 effort 不在本模型支持的阶梯里。
    #[error("模型不支持 effort 档位 {effort}")]
    UnsupportedEffort {
        /// 被拒绝的 effort。
        effort: Effort,
    },
}

impl ThinkingConfig {
    /// `effort_routing` 里专用于“完全关闭思考”的下标，见该字段文档。
    const OFF_ROUTING_INDEX: usize = 7;

    /// 构造一个思考配置。`efforts` 会被排序去重；为空时返回
    /// `Err(ThinkingError::EmptyLadder)`。
    pub fn new(mode: ThinkingControlMode, mut efforts: Vec<Effort>) -> Result<Self, ThinkingError> {
        efforts.sort_unstable();
        efforts.dedup();
        if efforts.is_empty() {
            return Err(ThinkingError::EmptyLadder);
        }
        Ok(Self {
            mode,
            efforts: efforts.into_boxed_slice(),
            effort_map: std::array::from_fn(|_| None),
            effort_routing: std::array::from_fn(|_| None),
            effort_budgets: std::array::from_fn(|_| None),
            suppress_when_off: false,
            requires_effort: false,
        })
    }

    /// 本模型支持的 effort 阶梯，低→高。
    #[must_use]
    pub fn efforts(&self) -> &[Effort] {
        &self.efforts
    }

    /// 该 effort 是否在本模型的阶梯里。
    #[must_use]
    pub fn supports(&self, effort: Effort) -> bool {
        self.efforts.contains(&effort)
    }

    /// 把请求的档位钳到本模型支持的最接近档：取 ≤ requested 的最大者，都不
    /// 满足则取最低档。`efforts` 由构造时的非空校验保证至少有一档，故末尾
    /// 的 `unwrap_or` 分支在实践中不会触发，仅作类型层面的兜底。
    #[must_use]
    pub fn clamp(&self, requested: Effort) -> Effort {
        self.efforts
            .iter()
            .copied()
            .filter(|&effort| effort <= requested)
            .max()
            .or_else(|| self.efforts.iter().copied().min())
            .unwrap_or(Effort::Off)
    }

    /// 严格校验请求的档位是否被支持；不支持时返回错误而非静默钳位，供需要
    /// 精确反馈“这个模型压根没有该档位”的调用方使用（如 CLI 参数校验）。
    /// 与 [`Self::clamp`] 互补：`clamp` 用于路由请求（总要发出点什么），
    /// 这里用于拒绝而不是妥协。
    pub fn require(&self, requested: Effort) -> Result<Effort, ThinkingError> {
        if self.supports(requested) {
            Ok(requested)
        } else {
            Err(ThinkingError::UnsupportedEffort { effort: requested })
        }
    }

    /// 该 effort 对应的线上字段值；`None` 表示省略字段（或该 effort 无
    /// 映射）。
    #[must_use]
    pub fn wire_effort(&self, effort: Effort) -> Option<&str> {
        self.effort_map
            .get(effort.index())
            .and_then(|slot| slot.as_deref())
    }

    /// 解析实际要请求的线上模型 id：优先按具体阶梯档位覆写；`effort ==
    /// Off` 时若无按档覆写则退到专用的“关闭”槽位；两者都没有则退到
    /// `fallback`（调用方的 `request_model_id ?? id`）。
    #[must_use]
    pub fn wire_model_id<'a>(&'a self, effort: Effort, fallback: &'a str) -> &'a str {
        if let Some(id) = self
            .effort_routing
            .get(effort.index())
            .and_then(|slot| slot.as_deref())
        {
            return id;
        }
        if effort == Effort::Off
            && let Some(id) = self
                .effort_routing
                .get(Self::OFF_ROUTING_INDEX)
                .and_then(|slot| slot.as_deref())
        {
            return id;
        }
        fallback
    }

    /// 该 effort 对应的 `budget_tokens`；仅 `mode: Budget` 时有实际意义。
    #[must_use]
    pub fn budget(&self, effort: Effort) -> Option<u32> {
        self.effort_budgets.get(effort.index()).copied().flatten()
    }

    /// 批量设置 effort → 线上字段值映射。
    #[must_use]
    pub fn with_effort_map(
        mut self,
        entries: impl IntoIterator<Item = (Effort, Box<str>)>,
    ) -> Self {
        for (effort, wire) in entries {
            if let Some(slot) = self.effort_map.get_mut(effort.index()) {
                *slot = Some(wire);
            }
        }
        self
    }

    /// 批量设置线上模型 id 路由。`key = None` 写入专用的“关闭”槽位（见
    /// [`Self::OFF_ROUTING_INDEX`]），`key = Some(effort)` 写入该具体阶梯
    /// 档位的按档覆写。
    #[must_use]
    pub fn with_effort_routing(
        mut self,
        entries: impl IntoIterator<Item = (Option<Effort>, Box<str>)>,
    ) -> Self {
        for (key, wire_id) in entries {
            let idx = key.map_or(Self::OFF_ROUTING_INDEX, Effort::index);
            if let Some(slot) = self.effort_routing.get_mut(idx) {
                *slot = Some(wire_id);
            }
        }
        self
    }

    /// 批量设置 effort → `budget_tokens` 映射。
    #[must_use]
    pub fn with_effort_budgets(mut self, entries: impl IntoIterator<Item = (Effort, u32)>) -> Self {
        for (effort, budget) in entries {
            if let Some(slot) = self.effort_budgets.get_mut(effort.index()) {
                *slot = Some(budget);
            }
        }
        self
    }
}

// Anthropic `claude-*`（含 `-thinking` 后缀变体，如 `claude-opus-4-6-thinking`，
// 该后缀不影响识别，仅由上游身份归一化剥离）。Budget 模式：`budget_tokens`
// 与 `max_tokens` 共同受限，这里给的是 Anthropic 文档里的常见档位，不是硬
// 上限——实际预算由调用方按剩余输出预算再夹一次。`Off` 省略 `thinking`
// 字段即可关闭，无需显式抑制。
fn anthropic_thinking() -> Result<ThinkingConfig, ThinkingError> {
    let config = ThinkingConfig::new(
        ThinkingControlMode::Budget,
        vec![Effort::Off, Effort::Low, Effort::Medium, Effort::High],
    )?
    .with_effort_budgets([
        (Effort::Low, 4096),
        (Effort::Medium, 16384),
        (Effort::High, 32768),
    ]);
    Ok(config)
}

// OpenAI `gpt-5*` / `o*`：Responses 风格 `reasoning.effort`，线上字段值与
// `Effort::as_str()` 完全同构。不含 `Off`：这些模型不接受完全关闭推理，
// `requires_effort = true` 让 off 请求钳到最低档 `Minimal`。
fn openai_thinking() -> Result<ThinkingConfig, ThinkingError> {
    const EFFORTS: [Effort; 4] = [Effort::Minimal, Effort::Low, Effort::Medium, Effort::High];
    let config = ThinkingConfig::new(ThinkingControlMode::Effort, EFFORTS.to_vec())?
        .with_effort_map(EFFORTS.map(|effort| (effort, Box::from(effort.as_str()))));
    Ok(ThinkingConfig {
        requires_effort: true,
        ..config
    })
}

// OpenAI Codex（`gpt-5*-codex*`）：同样是 Responses `reasoning.effort`，但
// Codex 后端不接受 `minimal`，最低档是 `Low`。同样不可完全关闭。
fn openai_codex_thinking() -> Result<ThinkingConfig, ThinkingError> {
    const EFFORTS: [Effort; 3] = [Effort::Low, Effort::Medium, Effort::High];
    let config = ThinkingConfig::new(ThinkingControlMode::Effort, EFFORTS.to_vec())?
        .with_effort_map(EFFORTS.map(|effort| (effort, Box::from(effort.as_str()))));
    Ok(ThinkingConfig {
        requires_effort: true,
        ..config
    })
}

// xAI `grok-*`：只收 `low`/`high` 两档（`crates/ai/src/provider/xai.rs` 的
// `REASONING_EFFORT_PREFIXES` 白名单已验证这是 xAI 实际接受的取值集合，
// 中间档位会被拒绝而非静默降级）。同样不可完全关闭。
fn xai_thinking() -> Result<ThinkingConfig, ThinkingError> {
    const EFFORTS: [Effort; 2] = [Effort::Low, Effort::High];
    let config = ThinkingConfig::new(ThinkingControlMode::Effort, EFFORTS.to_vec())?
        .with_effort_map(EFFORTS.map(|effort| (effort, Box::from(effort.as_str()))));
    Ok(ThinkingConfig {
        requires_effort: true,
        ..config
    })
}

// Google `gemini-*`：`thinkingConfig.thinkingLevel` 档位。关闭时必须显式发
// 抑制字段（`thinkingLevel: "MINIMAL"` + `includeThoughts: false`）而不是
// 省略——省略时 Cloud Code Assist 会回填该模型 id 烘焙好的服务端默认预算，
// 因此 `suppress_when_off = true`。
fn google_thinking() -> Result<ThinkingConfig, ThinkingError> {
    let config = ThinkingConfig::new(
        ThinkingControlMode::GoogleLevel,
        vec![Effort::Off, Effort::Low, Effort::Medium, Effort::High],
    )?
    .with_effort_map([
        (Effort::Low, Box::from("low")),
        (Effort::Medium, Box::from("medium")),
        (Effort::High, Box::from("high")),
    ]);
    Ok(ThinkingConfig {
        suppress_when_off: true,
        ..config
    })
}

// o1/o3/o4-mini 等 OpenAI O 系列推理模型：首字符 `o` 后接 ASCII 数字。用这个
// 弱前缀而非单纯 `starts_with("o")`，避免误伤非推理模型（如
// `omni-moderation-latest`）。
fn is_openai_o_series(model_id: &str) -> bool {
    let mut chars = model_id.chars();
    matches!(chars.next(), Some('o')) && matches!(chars.next(), Some(c) if c.is_ascii_digit())
}

/// 从静态元数据推导某模型的思考配置。
///
/// 返回 `None` 表示“无可控档位”——**这与“不会推理”不是一回事**：
/// `spec.reasoning == true` 且本函数返回 `None` 是合法状态（对应上游
/// `types.ts:38-42`），表示模型会推理但没有暴露可调档位（或本函数尚未收录
/// 该模型的族）。`spec.reasoning == false` 时直接短路返回 `None`：不会推理
/// 的模型不该有思考配置，无论 id 是否恰好撞上某个族的命名模式。
///
/// 匹配用前缀/子串判定（不引 regex）；覆盖本仓 `zcode-ai` 已支持的五条线：
/// Anthropic、OpenAI（含 Codex）、xAI、以及尚未接入但目录里已有条目的
/// Google Gemini。
#[must_use]
pub fn resolve_model_thinking(
    provider_id: &str,
    model_id: &str,
    spec: &ModelSpec,
) -> Option<ThinkingConfig> {
    if !spec.reasoning {
        return None;
    }

    // `models.json` 抽样的 provider id（anthropic/openai/xai/google）均为
    // 小写，故按精确匹配即可，不做大小写折叠。
    let built = if model_id.contains("codex") {
        openai_codex_thinking()
    } else if provider_id == "anthropic" && model_id.starts_with("claude") {
        anthropic_thinking()
    } else if provider_id == "openai"
        && (model_id.starts_with("gpt-5") || is_openai_o_series(model_id))
    {
        openai_thinking()
    } else if provider_id == "xai" && model_id.starts_with("grok") {
        xai_thinking()
    } else if provider_id.starts_with("google") && model_id.starts_with("gemini") {
        google_thinking()
    } else {
        debug!(provider_id, model_id, "推理模型无可识别的 effort 阶梯");
        return None;
    };

    // 上面五条 ladder 全是硬编码的非空常量，`ThinkingConfig::new` 只会在
    // 阶梯为空时报错，这里恒为 `Ok`。
    built.ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::{LimitSpec, ModelSpec};

    fn spec(reasoning: bool) -> ModelSpec {
        ModelSpec {
            id: "test-model".into(),
            name: "Test Model".into(),
            cost: None,
            limit: LimitSpec::default(),
            input: Vec::new().into_boxed_slice(),
            output: Vec::new().into_boxed_slice(),
            reasoning,
            tool_call: false,
            status: None,
        }
    }

    #[test]
    fn empty_ladder_is_rejected() {
        let err = ThinkingConfig::new(ThinkingControlMode::Budget, vec![]).unwrap_err();
        assert_eq!(err, ThinkingError::EmptyLadder);
    }

    #[test]
    fn duplicate_and_unordered_efforts_are_normalized() {
        let config = ThinkingConfig::new(
            ThinkingControlMode::Effort,
            vec![Effort::High, Effort::Low, Effort::Low],
        )
        .unwrap();
        assert_eq!(config.efforts(), &[Effort::Low, Effort::High]);
    }

    #[test]
    fn clamp_pins_to_highest_supported_when_requested_exceeds_ladder() {
        let config = ThinkingConfig::new(
            ThinkingControlMode::Effort,
            vec![Effort::Low, Effort::Medium, Effort::High],
        )
        .unwrap();
        assert_eq!(config.clamp(Effort::Max), Effort::High);
    }

    #[test]
    fn clamp_pins_to_lowest_supported_when_requested_below_ladder() {
        let config = ThinkingConfig::new(
            ThinkingControlMode::Effort,
            vec![Effort::Minimal, Effort::Low, Effort::Medium, Effort::High],
        )
        .unwrap();
        assert_eq!(config.clamp(Effort::Off), Effort::Minimal);
    }

    #[test]
    fn clamp_is_identity_for_a_supported_effort() {
        let config = ThinkingConfig::new(
            ThinkingControlMode::Effort,
            vec![Effort::Low, Effort::Medium, Effort::High],
        )
        .unwrap();
        assert_eq!(config.clamp(Effort::Medium), Effort::Medium);
    }

    #[test]
    fn require_rejects_an_effort_outside_the_ladder() {
        let config =
            ThinkingConfig::new(ThinkingControlMode::Effort, vec![Effort::Low, Effort::High])
                .unwrap();
        assert_eq!(config.require(Effort::Low), Ok(Effort::Low));
        assert_eq!(
            config.require(Effort::Medium),
            Err(ThinkingError::UnsupportedEffort {
                effort: Effort::Medium
            })
        );
    }

    #[test]
    fn wire_model_id_falls_back_through_three_tiers() {
        let config = ThinkingConfig::new(
            ThinkingControlMode::Effort,
            vec![Effort::Off, Effort::Low, Effort::High],
        )
        .unwrap()
        .with_effort_routing([(Some(Effort::High), Box::from("model-high"))]);

        // 第一层：具体档位有专门覆写。
        assert_eq!(config.wire_model_id(Effort::High, "fallback"), "model-high");
        // 第二/三层暂缺覆写：没有专门覆写、也没有关闭槽位时，落到调用方兜底。
        assert_eq!(config.wire_model_id(Effort::Low, "fallback"), "fallback");
        assert_eq!(config.wire_model_id(Effort::Off, "fallback"), "fallback");

        let with_off_routing = config.with_effort_routing([(None, Box::from("model-off"))]);
        // 第二层：off 且无具体覆写时，落到专用的关闭槽位。
        assert_eq!(
            with_off_routing.wire_model_id(Effort::Off, "fallback"),
            "model-off"
        );
        // 具体覆写（High）优先于关闭槽位/兜底，不受影响。
        assert_eq!(
            with_off_routing.wire_model_id(Effort::High, "fallback"),
            "model-high"
        );

        let with_off_specific = with_off_routing
            .with_effort_routing([(Some(Effort::Off), Box::from("model-off-specific"))]);
        // 具体档位覆写（Off 自身）优先于关闭槽位。
        assert_eq!(
            with_off_specific.wire_model_id(Effort::Off, "fallback"),
            "model-off-specific"
        );
    }

    #[test]
    fn budget_and_wire_effort_are_none_when_unmapped() {
        let config =
            ThinkingConfig::new(ThinkingControlMode::Budget, vec![Effort::Off, Effort::High])
                .unwrap();
        assert_eq!(config.budget(Effort::High), None);
        assert_eq!(config.wire_effort(Effort::High), None);
    }

    #[test]
    fn resolve_anthropic_claude_uses_budget_ladder() {
        let config = resolve_model_thinking("anthropic", "claude-opus-4-6", &spec(true)).unwrap();
        assert_eq!(config.mode, ThinkingControlMode::Budget);
        assert_eq!(
            config.efforts(),
            &[Effort::Off, Effort::Low, Effort::Medium, Effort::High]
        );
        assert_eq!(config.budget(Effort::Medium), Some(16384));
        assert!(!config.requires_effort);
    }

    #[test]
    fn resolve_openai_gpt5_uses_effort_ladder_without_off() {
        let config = resolve_model_thinking("openai", "gpt-5.2", &spec(true)).unwrap();
        assert_eq!(config.mode, ThinkingControlMode::Effort);
        assert!(!config.supports(Effort::Off));
        assert!(config.requires_effort);
        assert_eq!(config.wire_effort(Effort::Minimal), Some("minimal"));
    }

    #[test]
    fn resolve_openai_o_series_matches_via_digit_after_o() {
        let config = resolve_model_thinking("openai", "o3-mini", &spec(true)).unwrap();
        assert_eq!(config.mode, ThinkingControlMode::Effort);
    }

    #[test]
    fn resolve_openai_codex_takes_priority_over_generic_gpt5_ladder() {
        let config = resolve_model_thinking("openai", "gpt-5.3-codex", &spec(true)).unwrap();
        assert_eq!(
            config.efforts(),
            &[Effort::Low, Effort::Medium, Effort::High]
        );
        assert!(!config.supports(Effort::Minimal));
    }

    #[test]
    fn resolve_xai_grok_only_supports_low_and_high() {
        let config = resolve_model_thinking("xai", "grok-4.3", &spec(true)).unwrap();
        assert_eq!(config.efforts(), &[Effort::Low, Effort::High]);
        assert!(!config.supports(Effort::Medium));
    }

    #[test]
    fn resolve_google_gemini_requires_explicit_suppress_when_off() {
        let config = resolve_model_thinking("google", "gemini-3-pro-preview", &spec(true)).unwrap();
        assert_eq!(config.mode, ThinkingControlMode::GoogleLevel);
        assert!(config.suppress_when_off);
        assert_eq!(config.wire_effort(Effort::Low), Some("low"));
    }

    #[test]
    fn resolve_unknown_model_family_returns_none_even_when_reasoning_is_true() {
        assert_eq!(
            resolve_model_thinking("mystery", "weird-model-9", &spec(true)),
            None
        );
    }

    #[test]
    fn resolve_non_reasoning_model_returns_none_regardless_of_name() {
        // 名字撞上 claude 前缀，但 `spec.reasoning == false`：不该有思考配置。
        assert_eq!(
            resolve_model_thinking("anthropic", "claude-opus-4-6", &spec(false)),
            None
        );
    }
}
