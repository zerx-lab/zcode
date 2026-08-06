//! token 估算与压缩决策。
//!
//! # 分工
//!
//! 本模块只回答三个问题：一批消息大概占多少 token（[`estimate_messages`] /
//! [`estimate_context`]）、提供商回报的用量该怎么折算成"它看到的上下文占用"
//! （[`reported_context_tokens`]）、以及现在该不该压缩、从哪切（[`plan_compaction`]）。
//! 真正生成摘要、把 [`crate::session::CompactionReason`] 落成 [`crate::session::EntryKind::Compaction`]
//! 条目是 `turn` 模块与调用方的职责，这里不碰任何 I/O。
//!
//! # 参考仓调研结论
//!
//! - 常量取值与"字符数估算 token、图片按固定价"的做法抄自 jcode
//!   `crates/jcode-compaction-core/src/lib.rs:1-77`（常量）与 `:297-329`
//!   （`content_char_count`，图片走 [`IMAGE_TOKEN_COST`] 而非 base64 长度）。
//! - "提供商用量该怎么折算成上下文占用，且要跟 UI 侧栏用同一套算法"抄自 jcode
//!   `crates/jcode-compaction-core/src/lib.rs:362-387`
//!   （`effective_context_tokens_from_usage`，对应 jcode issue #441）。
//! - "压缩决策要取 `max(本地估算, 提供商回报)` 而不是纯信提供商"抄自 oh-my-pi
//!   `packages/agent/src/compaction/compaction.ts:340-357`
//!   （`compactionContextTokens`）：`before_provider_request` 阶段的请求体压缩/截断
//!   会让提供商回报的用量小于真实存量，纯信提供商会让真实历史无限增长直到溢出。
//! - "压缩切点必须落在工具配对边界，找不到就不压"抄自 jcode
//!   `crates/jcode-compaction-core/src/lib.rs:238-291`（`safe_compaction_cutoff`）。
//!
//! oh-my-pi 的预留（reserve）/ 阶梯摘要预算（`compaction.ts:187-329`）本模块没有照搬：
//! 那一套是"给摘要本身留多少输出空间"的问题，属于生成摘要那一步（`turn` 模块 /
//! 调用方），跟"要不要触发压缩、从哪切"是两个问题，硬塞进来只会让这个模块背上
//! 不属于它的职责。

use std::collections::HashSet;

use crate::id::EntryId;
use crate::session::message::{
    MessageRecord, StoredAssistantContent, StoredMessage, StoredToolResultContent, StoredUsage,
    StoredUserContent,
};

/// 字符 → token 的粗估系数：约每 4 字节算 1 token。
///
/// 抄源 jcode `crates/jcode-compaction-core/src/lib.rs:43`（`CHARS_PER_TOKEN`）。
/// **上游没有给出这个系数的依据**（既不是任何分词器的实测均值，也没有引用来源），
/// 本仓原样沿用，待用真实会话样本实测修正。
pub const CHARS_PER_TOKEN: u64 = 4;

/// 一张图片计入上下文的固定 token 价。
///
/// 抄源 jcode `crates/jcode-compaction-core/src/lib.rs:58`（`IMAGE_TOKEN_COST`）。
/// 前提写在 [`estimate_messages`] 上：提供商按分辨率计费而非按 base64 传输长度计费，
/// 用原始长度会把一张几百 KB 的截图估成几十万 token，误差约百倍。
pub const IMAGE_TOKEN_COST: u64 = 1_600;

/// 系统提示 + 工具定义的固定开销估值，不出现在消息列表里但计入上下文占用。
///
/// 抄源 jcode `crates/jcode-compaction-core/src/lib.rs:60-63`（`SYSTEM_OVERHEAD_TOKENS`）：
/// 上游给出的推导是"系统提示约 8k + 50 余个工具定义约 10k"，本身也是估算，非精确值。
pub const SYSTEM_OVERHEAD_TOKENS: u64 = 18_000;

/// 触发主动压缩的占用比：`window` 的 80%。
///
/// 抄源 jcode `crates/jcode-compaction-core/src/lib.rs:9`（`COMPACTION_THRESHOLD`，原类型
/// `f32`）。**上游没有给出 0.80 这个数字的依据**，本仓原样沿用，待用真实会话样本实测修正。
///
/// 这个常量只用于对外展示 / 文档；[`ContextBudget::threshold`] 的实际计算走
/// [`COMPACTION_THRESHOLD_PERCENT`]（整数百分比），避免 `f64 -> u64` 需要被 lint 禁掉的
/// `as` 转换。两者保持同步由 `tests::threshold_percent_constants_match_f64_constants` 断言。
pub const COMPACTION_THRESHOLD: f64 = 0.80;

/// 升级为紧急压缩的占用比：`window` 的 95%。出处、依据、与整数常量的关系同
/// [`COMPACTION_THRESHOLD`]；对应 jcode `:13` 的 `CRITICAL_THRESHOLD`。
pub const CRITICAL_THRESHOLD: f64 = 0.95;

/// [`COMPACTION_THRESHOLD`] 的整数百分比形式，供 [`ContextBudget::threshold`] 直接做
/// 整数运算，不必把 `f64` 转回 `u64`。
const COMPACTION_THRESHOLD_PERCENT: u64 = 80;

/// [`CRITICAL_THRESHOLD`] 的整数百分比形式，用途同 [`COMPACTION_THRESHOLD_PERCENT`]。
const CRITICAL_THRESHOLD_PERCENT: u64 = 95;

/// 保留的最近消息数（jcode 命名为"turn"，但上游实现里就是按消息数而非语义轮次计数，
/// 见 jcode `crates/jcode-base/src/compaction.rs:616`
/// `active.len().saturating_sub(RECENT_TURNS_TO_KEEP)`；本模块沿用同样的口径，不另造
/// "轮次"边界检测）。
///
/// 抄源 jcode `crates/jcode-compaction-core/src/lib.rs:19`（`RECENT_TURNS_TO_KEEP`）。
/// **上游没有给出 10 这个数字的依据**，本仓原样沿用，待实测修正。
pub const RECENT_TURNS_TO_KEEP: usize = 10;

/// [`ContextBudget::for_model`] 在目录查不到模型时使用的兜底窗口容量。
///
/// 与 jcode `crates/jcode-compaction-core/src/lib.rs:6`（`DEFAULT_TOKEN_BUDGET`）取值一致：
/// 对应 Claude 系列公开文档给出的上下文上限，是已知模型里偏保守（而非最大）的窗口——
/// 用作兜底不会让压缩阈值虚高进而漏触发压缩。
const DEFAULT_CONTEXT_WINDOW: u64 = 200_000;

/// 上下文预算：给定模型的窗口容量后，压缩阈值与紧急阈值都由它派生。
#[derive(Debug, Clone, Copy)]
pub struct ContextBudget {
    /// 模型的上下文窗口容量，单位 token。
    pub window: u64,
}

impl ContextBudget {
    /// 从模型 id 查内置目录拿上下文窗口容量；查不到时用 [`DEFAULT_CONTEXT_WINDOW`] 兜底。
    ///
    /// 只有模型 id、没有 provider id，因此走
    /// [`zcode_catalog::models::BundledCatalog::find_model_everywhere`]——它会解析全部
    /// provider（惰性缓存后不重复付出解析开销），比按 provider 精确查询贵，但本函数只在
    /// 会话初始化 / 切换模型时调用一次，不在逐条消息的热路径上。同一个 model id 被多家
    /// provider 托管时取第一个命中的窗口。
    #[must_use]
    pub fn for_model(model: &str) -> Self {
        let window = zcode_catalog::models::BundledCatalog::find_model_everywhere(model)
            .ok()
            .and_then(|hits| hits.first().and_then(|hit| hit.spec().limit.context))
            .map_or(DEFAULT_CONTEXT_WINDOW, u64::from);
        Self { window }
    }

    /// 触发主动压缩的 token 数：`window * 80 / 100`。
    #[must_use]
    pub fn threshold(self) -> u64 {
        self.window.saturating_mul(COMPACTION_THRESHOLD_PERCENT) / 100
    }

    /// 升级为紧急压缩的 token 数：`window * 95 / 100`。
    #[must_use]
    pub fn critical(self) -> u64 {
        self.window.saturating_mul(CRITICAL_THRESHOLD_PERCENT) / 100
    }
}

/// 把 `str::len()`（UTF-8 字节数）安全转换成 `u64`，不使用 `as`。
///
/// 64 位平台上 `usize` 与 `u64` 同宽，转换必然精确；`unwrap_or(u64::MAX)` 只是防御性
/// 兜底，不代表真实会发生截断。刻意用 `.len()` 而不是 `chars().count()`——后者是
/// O(n) 的 UTF-8 遍历，压缩决策要在每次估算时对整段历史反复调用，不能背这个开销
/// （jcode `crates/jcode-agent/src/tools.rs:8` 就是这个反面教材）。
fn byte_len(text: &str) -> u64 {
    u64::try_from(text.len()).unwrap_or(u64::MAX)
}

/// 一条用户消息内容块的（非图片字符数, 图片张数）。
fn user_content_footprint(content: &[StoredUserContent]) -> (u64, u64) {
    let mut chars = 0u64;
    let mut images = 0u64;
    for block in content {
        match block {
            StoredUserContent::Text { text } => chars = chars.saturating_add(byte_len(text)),
            StoredUserContent::Image { .. } => images = images.saturating_add(1),
        }
    }
    (chars, images)
}

/// 一条助手消息内容块的非图片字符数（助手消息不携带图片）。
fn assistant_content_footprint(content: &[StoredAssistantContent]) -> u64 {
    let mut chars = 0u64;
    for block in content {
        let block_chars = match block {
            StoredAssistantContent::Text { text } => byte_len(text),
            StoredAssistantContent::Thinking { text, signature } => {
                byte_len(text).saturating_add(signature.as_deref().map_or(0, byte_len))
            }
            StoredAssistantContent::RedactedThinking { data } => byte_len(data),
            StoredAssistantContent::ToolCall {
                id,
                name,
                arguments,
            } => byte_len(id)
                .saturating_add(byte_len(name))
                .saturating_add(byte_len(arguments)),
        };
        chars = chars.saturating_add(block_chars);
    }
    chars
}

/// 一条工具结果消息内容块的（非图片字符数, 图片张数）。
fn tool_result_content_footprint(content: &[StoredToolResultContent]) -> (u64, u64) {
    let mut chars = 0u64;
    let mut images = 0u64;
    for block in content {
        match block {
            StoredToolResultContent::Text { text } => chars = chars.saturating_add(byte_len(text)),
            StoredToolResultContent::Image { .. } => images = images.saturating_add(1),
        }
    }
    (chars, images)
}

/// 单条消息的（非图片字符数, 图片张数）。
fn message_footprint(message: &StoredMessage) -> (u64, u64) {
    match message {
        StoredMessage::User { content, .. } => user_content_footprint(content),
        StoredMessage::Assistant { content, .. } => (assistant_content_footprint(content), 0),
        StoredMessage::ToolResult {
            tool_call_id,
            tool_name,
            content,
            ..
        } => {
            let (mut chars, images) = tool_result_content_footprint(content);
            chars = chars
                .saturating_add(byte_len(tool_call_id))
                .saturating_add(byte_len(tool_name));
            (chars, images)
        }
    }
}

/// 估算一批消息的 token 占用（不含 [`SYSTEM_OVERHEAD_TOKENS`] 系统开销）。
///
/// 图片按 [`IMAGE_TOKEN_COST`] 计固定价，**不**按 base64 数据长度估算：提供商按分辨率
/// 而非传输编码后的字节数计费，用原始长度会把一张几百 KB 的内联截图估成几十万
/// token（约高估百倍），进而触发不必要的压缩——压缩后图片仍在保留窗口内，估算值
/// 降不下来，导致连续触发"三连压缩"却始终压不到阈值以下（抄源 jcode
/// `crates/jcode-compaction-core/src/lib.rs:44-57` 的 `IMAGE_TOKEN_COST` 文档）。
#[must_use]
pub fn estimate_messages(messages: &[StoredMessage]) -> u64 {
    let mut chars = 0u64;
    let mut images = 0u64;
    for message in messages {
        let (msg_chars, msg_images) = message_footprint(message);
        chars = chars.saturating_add(msg_chars);
        images = images.saturating_add(msg_images);
    }
    (chars / CHARS_PER_TOKEN).saturating_add(images.saturating_mul(IMAGE_TOKEN_COST))
}

/// 加上系统开销（[`SYSTEM_OVERHEAD_TOKENS`]）的总估算。
#[must_use]
pub fn estimate_context(messages: &[StoredMessage]) -> u64 {
    estimate_messages(messages).saturating_add(SYSTEM_OVERHEAD_TOKENS)
}

/// 把提供商回报的用量换算成"它看到的上下文占用"。
///
/// 不同提供商对 `input`/`prompt` token 的计账口径不一致：
/// - **分离计账**（Anthropic 系）：`input` 只是未命中缓存的剩余部分，缓存读/写是独立
///   计数器，真实上下文占用要把三者相加。
/// - **子集计账**（OpenAI 系）：`input`（`prompt_tokens`）本身已经包含被缓存命中的部分，
///   `cached_tokens` 只是其中一个子集，不能再加一遍，否则会重复计数。
///
/// 判据（模型名含 `claude` / `anthropic`，或 `cache_write > 0`，或 `cache_read > input`）
/// 与换算逻辑抄源 jcode `crates/jcode-compaction-core/src/lib.rs:362-387`
/// （`effective_context_tokens_from_usage`）。上游文档强调的前提在这里同样成立：
/// 侧栏展示的上下文占用与压缩触发判断必须走同一个函数，否则两处会互相矛盾
/// （jcode issue #441）。
#[must_use]
pub fn reported_context_tokens(model: &str, usage: StoredUsage) -> u64 {
    if usage.input == 0 {
        return 0;
    }
    let lower = model.to_lowercase();
    let split_accounting = lower.contains("claude")
        || lower.contains("anthropic")
        || usage.cache_write > 0
        || usage.cache_read > usage.input;
    if split_accounting {
        usage
            .input
            .saturating_add(usage.cache_read)
            .saturating_add(usage.cache_write)
    } else {
        usage.input
    }
}

/// 压缩决策取的占用值：`max(本地估算, 提供商回报)`。
///
/// 抄源 oh-my-pi `packages/agent/src/compaction/compaction.ts:340-357`
/// （`compactionContextTokens`）：提供商回报的用量通常是事实来源，但请求体在发出前
/// 可能被截断/压缩（例如 headroom 压缩扩展、隧道混淆器），导致提供商报的 prompt
/// token 数比真实存量的会话历史小。纯信提供商的用量会让真实历史无限增长直到溢出，
/// 而 native 压缩这时候已经用不了了。取两者较大值，把估算值当作地板，保证压缩触发
/// 不受任何链路上的请求体压缩影响；展示/计费仍然用提供商回报的精确值，只有压缩
/// 决策取这个地板。
#[must_use]
pub fn effective_context_tokens(estimate: u64, reported: Option<u64>) -> u64 {
    match reported {
        Some(reported) => estimate.max(reported),
        None => estimate,
    }
}

/// 压缩计划。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompactionPlan {
    /// 不需要压缩。
    None,
    /// 需要压缩：摘要 `..first_kept`，保留 `first_kept..`。
    Compact {
        /// 保留段的第一条消息 id；`None` 表示全部摘要。
        first_kept: Option<EntryId>,
        /// 是否已达 [`CRITICAL_THRESHOLD`]（紧急压缩）。
        urgent: bool,
    },
}

/// 记录一条消息里出现的 `ToolCall` id（写入 `available` 并从 `missing` 移除）与
/// `ToolResult` 引用的 id（若尚不可用则写入 `missing`）。
fn track_tool_ids<'a>(
    message: &'a StoredMessage,
    available: &mut HashSet<&'a str>,
    missing: &mut HashSet<&'a str>,
) {
    match message {
        StoredMessage::Assistant { content, .. } => {
            for block in content {
                if let StoredAssistantContent::ToolCall { id, .. } = block {
                    available.insert(id.as_str());
                    missing.remove(id.as_str());
                }
            }
        }
        StoredMessage::ToolResult { tool_call_id, .. } => {
            if !available.contains(tool_call_id.as_str()) {
                missing.insert(tool_call_id.as_str());
            }
        }
        StoredMessage::User { .. } => {}
    }
}

/// 从 `initial_cutoff` 起向前搜索一个不撕裂工具配对的安全切点。
///
/// `records[cutoff..]` 是拟保留的原文；若其中有 `ToolResult` 引用了段外（已被摘要
/// 吃掉）的 `ToolCall`，就把 `cutoff` 前移，把对应的 `ToolCall` 一并纳入保留段，直到
/// 配对补齐。`ToolCall` 在保留段而其 `ToolResult` 不在的情况不需要单独处理：`ToolCall`
/// 总是先于它的 `ToolResult` 出现，保留段是一段后缀，`ToolCall` 若被保留，它之后（更晚）
/// 的 `ToolResult` 必然也在保留段内。
///
/// 补不齐（历史上存在无主的 `ToolResult`，一路前移到整段历史都不够）返回 `None`，
/// 交给 [`plan_compaction`] 放弃本次压缩。
///
/// 抄源 jcode `crates/jcode-compaction-core/src/lib.rs:238-291`
/// （`safe_compaction_cutoff`），把"补不齐时退回 0（=不切）"改造成显式 `None`，
/// 让调用方对"这次压不了"有类型层面的信号，而不是悄悄返回一个等价于不压的切点。
fn safe_cutoff(records: &[MessageRecord], initial_cutoff: usize) -> Option<usize> {
    let mut cutoff = initial_cutoff.min(records.len());
    let mut available: HashSet<&str> = HashSet::new();
    let mut missing: HashSet<&str> = HashSet::new();

    for record in records.get(cutoff..).unwrap_or_default() {
        track_tool_ids(&record.message, &mut available, &mut missing);
    }
    if missing.is_empty() {
        return Some(cutoff);
    }

    let prefix = records.get(..cutoff).unwrap_or_default();
    for (idx, record) in prefix.iter().enumerate().rev() {
        track_tool_ids(&record.message, &mut available, &mut missing);
        if missing.is_empty() {
            cutoff = idx;
            return Some(cutoff);
        }
    }
    None
}

/// 制定压缩计划。
///
/// `occupied` 是 [`effective_context_tokens`] 算出的占用值。未达
/// [`ContextBudget::threshold`] 不压缩；达到后，初始切点取"倒数第
/// [`RECENT_TURNS_TO_KEEP`] 条消息"，再交给 [`safe_cutoff`] 调整到不撕裂工具配对的
/// 位置。调整后的切点等于 0（意味着连一条消息都摘要不了）或者根本找不到安全切点，
/// 都视为"这次不压"——宁可让上下文继续增长，也不生成一份会让下一次请求 400 的
/// 保留段（jcode `safe_compaction_cutoff` 同策：找不到就返回等价于不切的值）。
#[must_use]
pub fn plan_compaction(
    records: &[MessageRecord],
    budget: ContextBudget,
    occupied: u64,
) -> CompactionPlan {
    if occupied < budget.threshold() {
        return CompactionPlan::None;
    }
    let urgent = occupied >= budget.critical();

    let initial_cutoff = records.len().saturating_sub(RECENT_TURNS_TO_KEEP);
    if initial_cutoff == 0 {
        return CompactionPlan::None;
    }

    match safe_cutoff(records, initial_cutoff) {
        None | Some(0) => CompactionPlan::None,
        Some(cutoff) => match records.get(cutoff) {
            Some(record) => CompactionPlan::Compact {
                first_kept: Some(record.id.clone()),
                urgent,
            },
            None => CompactionPlan::None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::message::{StoredImage, StoredStopReason};

    fn record(message: StoredMessage) -> MessageRecord {
        MessageRecord {
            id: EntryId::generate(),
            message,
        }
    }

    fn assistant_text(text: &str) -> StoredMessage {
        StoredMessage::Assistant {
            content: vec![StoredAssistantContent::Text {
                text: text.to_owned(),
            }],
            model: None,
            usage: StoredUsage::default(),
            stop_reason: StoredStopReason::default(),
        }
    }

    fn assistant_tool_calls(ids: &[&str]) -> StoredMessage {
        StoredMessage::Assistant {
            content: ids
                .iter()
                .map(|id| StoredAssistantContent::ToolCall {
                    id: (*id).to_owned(),
                    name: "read".to_owned(),
                    arguments: "{}".to_owned(),
                })
                .collect(),
            model: None,
            usage: StoredUsage::default(),
            stop_reason: StoredStopReason::default(),
        }
    }

    fn tool_result(id: &str) -> StoredMessage {
        StoredMessage::ToolResult {
            tool_call_id: id.to_owned(),
            tool_name: "read".to_owned(),
            content: vec![StoredToolResultContent::Text {
                text: "ok".to_owned(),
            }],
            is_error: false,
        }
    }

    // ── 常量一致性 ──────────────────────────────────────────────────────

    #[test]
    fn threshold_percent_constants_match_f64_constants() {
        let compaction_percent =
            f64::from(u32::try_from(COMPACTION_THRESHOLD_PERCENT).expect("80 在 u32 范围内"))
                / 100.0;
        let critical_percent =
            f64::from(u32::try_from(CRITICAL_THRESHOLD_PERCENT).expect("95 在 u32 范围内")) / 100.0;
        assert!((compaction_percent - COMPACTION_THRESHOLD).abs() < f64::EPSILON);
        assert!((critical_percent - CRITICAL_THRESHOLD).abs() < f64::EPSILON);
    }

    #[test]
    fn threshold_and_critical_are_percentages_of_window() {
        let budget = ContextBudget { window: 1000 };
        assert_eq!(budget.threshold(), 800);
        assert_eq!(budget.critical(), 950);
    }

    #[test]
    fn for_model_falls_back_to_default_window_for_unknown_model() {
        let budget = ContextBudget::for_model("definitely-not-a-real-model-id-xyz");
        assert_eq!(budget.window, DEFAULT_CONTEXT_WINDOW);
    }

    // ── token 估算 ──────────────────────────────────────────────────────

    #[test]
    fn estimate_messages_charges_images_a_flat_cost_not_base64_length() {
        // 2MB 的 base64 字符串；若按长度估算会把这单张图片算成约 50 万 token，
        // 换一张更大的图仍应稳定落在 IMAGE_TOKEN_COST，验证"固定价"而非"随长度撑爆"。
        let huge_image = StoredImage {
            media_type: "image/png".to_owned(),
            data: "A".repeat(2_000_000),
        };
        let message = StoredMessage::User {
            content: vec![StoredUserContent::Image { image: huge_image }],
            display_role: None,
        };
        let messages = [message];
        assert_eq!(estimate_messages(&messages), IMAGE_TOKEN_COST);
    }

    #[test]
    fn estimate_context_adds_system_overhead() {
        let message = StoredMessage::user("hi");
        let messages = [message];
        assert_eq!(
            estimate_context(&messages),
            estimate_messages(&messages) + SYSTEM_OVERHEAD_TOKENS
        );
    }

    // ── 提供商用量换算 ──────────────────────────────────────────────────

    #[test]
    fn reported_context_tokens_uses_split_accounting_for_claude() {
        let usage = StoredUsage {
            input: 100,
            output: 20,
            cache_read: 50,
            cache_write: 30,
            reasoning: 0,
        };
        assert_eq!(reported_context_tokens("claude-sonnet-4-6", usage), 180);
    }

    #[test]
    fn reported_context_tokens_detects_split_accounting_without_claude_in_the_name() {
        // cache_write > 0 本身就是分离计账的证据，不依赖模型名。
        let usage = StoredUsage {
            input: 100,
            output: 0,
            cache_read: 0,
            cache_write: 10,
            reasoning: 0,
        };
        assert_eq!(reported_context_tokens("some-custom-model", usage), 110);
    }

    #[test]
    fn reported_context_tokens_uses_subset_accounting_for_openai() {
        let usage = StoredUsage {
            input: 100,
            output: 20,
            cache_read: 40,
            cache_write: 0,
            reasoning: 0,
        };
        assert_eq!(reported_context_tokens("gpt-4o", usage), 100);
    }

    #[test]
    fn effective_context_tokens_takes_the_larger_of_estimate_and_reported() {
        assert_eq!(effective_context_tokens(100, Some(50)), 100);
        assert_eq!(effective_context_tokens(50, Some(120)), 120);
        assert_eq!(effective_context_tokens(70, None), 70);
    }

    // ── 压缩决策 ────────────────────────────────────────────────────────

    #[test]
    fn plan_compaction_below_threshold_does_nothing() {
        let budget = ContextBudget { window: 1000 };
        let records = vec![record(StoredMessage::user("hi"))];
        assert_eq!(plan_compaction(&records, budget, 500), CompactionPlan::None);
    }

    #[test]
    fn plan_compaction_shifts_cutoff_to_avoid_an_orphan_tool_result() {
        // 13 条记录，倒数 10 条(RECENT_TURNS_TO_KEEP)以内的朴素切点(idx 3)正好落在
        // 一条孤儿 ToolResult 上；安全切点必须回退到 idx 2 的配对 ToolCall。
        let records = vec![
            record(StoredMessage::user("q0")),     // 0
            record(assistant_text("a0")),          // 1
            record(assistant_tool_calls(&["t1"])), // 2 — 安全切点应落在这
            record(tool_result("t1")),             // 3 — 朴素切点，孤儿
            record(StoredMessage::user("q1")),     // 4
            record(assistant_text("a1")),          // 5
            record(StoredMessage::user("q2")),     // 6
            record(assistant_text("a2")),          // 7
            record(StoredMessage::user("q3")),     // 8
            record(assistant_text("a3")),          // 9
            record(StoredMessage::user("q4")),     // 10
            record(assistant_text("a4")),          // 11
            record(StoredMessage::user("q5")),     // 12
        ];
        assert_eq!(records.len(), 13);

        let expected_id = records[2].id.clone();
        assert!(matches!(
            records[2].message,
            StoredMessage::Assistant { .. }
        ));

        let budget = ContextBudget { window: 1000 }; // threshold=800, critical=950
        match plan_compaction(&records, budget, 850) {
            CompactionPlan::Compact { first_kept, urgent } => {
                assert_eq!(first_kept, Some(expected_id));
                assert!(!urgent, "850 < critical(950)，不应标记紧急");
            }
            CompactionPlan::None => panic!("850 已超过 threshold(800)，应给出压缩计划"),
        }
    }

    #[test]
    fn plan_compaction_gives_up_when_the_whole_chain_is_one_tool_pairing() {
        // 单条消息里发起 11 个工具调用，随后 11 条工具结果依次跟着——任何朴素切点都会
        // 撕裂配对，唯一安全的切点是 0（=不切任何东西），应当退化为 None。
        let ids: Vec<String> = (0..11).map(|i| format!("t{i}")).collect();
        let id_refs: Vec<&str> = ids.iter().map(String::as_str).collect();

        let mut records = vec![record(assistant_tool_calls(&id_refs))];
        for id in &ids {
            records.push(record(tool_result(id)));
        }
        assert_eq!(records.len(), 12);

        let budget = ContextBudget { window: 1000 }; // threshold=800
        assert_eq!(plan_compaction(&records, budget, 900), CompactionPlan::None);
    }
}
