//! 工具注册表：名字到实现的映射 + 一次性编译好的参数 schema。
//!
//! # 为什么 schema 在注册期编译，不在每次调用时编译
//!
//! opencode 把 `decodeUnknownEffect(parameters)` 的编译结果提到 `init()` 里只做一次，
//! 注释直接写明理由：逐次调用都重新构造解析闭包的开销会被摊到每一次模型工具调用上
//! （`packages/opencode/src/tool/tool.ts:104-107`——"Compile the parser closure once per
//! tool init ... hoisting avoids re-closing it for every LLM tool invocation"）。本实现
//! 更进一步：编译失败直接让 [`ToolRegistry::register`] 报错，而不是把一份编不出校验器的
//! schema 悄悄放进去、等到模型真的调用这个工具时才发现参数校验形同虚设——宁可启动失败。
//!
//! # `definitions()` 为什么按名排序
//!
//! jcode 的等价方法在返回前显式 `sort_by(name)`，注释写的是
//! "critical for prompt cache hits"（`crates/jcode-app-core/src/tool/mod.rs:339-342`）：
//! 大多数提供商的 prompt cache 按前缀做内容寻址，工具定义顺序但凡因为 `HashMap` 遍历序
//! 抖动一次，缓存前缀就整体失效。

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value;
use zcode_schema::{CompiledSchema, SchemaCache, SchemaError, render_validation_error};

use crate::tool::Tool;

/// 建议列表的最大条数。
///
/// 抄源 jcode `crates/jcode-app-core/src/tool/mod.rs:378-416`（`closest_tool_names`）：
/// 三级启发式打分后只取前 3 个，多了对模型没有额外帮助，只会占 token。
const MAX_SUGGESTIONS: usize = 3;

/// 工具注册表：工具名 → 实现，外加按内容哈希去重的编译后 schema。
///
/// 同名工具多次注册的 schema 若字面相同（如多个工具共享同一份参数子 schema），
/// [`SchemaCache`] 只编译一次；这与本注册表"每个工具名只编译一次"的目标正交，
/// 二者叠加只会更省，不会冲突。
#[derive(Debug, Default)]
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
    compiled: HashMap<String, Arc<CompiledSchema>>,
    schema_cache: SchemaCache,
}

impl ToolRegistry {
    /// 创建一个空注册表。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册一个工具；同名覆盖并返回旧的实现。
    ///
    /// 注册时立即编译 `tool.parameters()` 返回的 schema：编译失败（结构不合法、
    /// `$ref` 无法解析、`pattern` 不是 `regex` crate 支持的语法）直接返回
    /// [`SchemaError`]，不把这个工具放进注册表——宁可启动失败，也不要让一个校验器
    /// 编不出来的工具混进去，等模型发起调用时才发现参数校验根本没生效。
    ///
    /// # Errors
    /// `tool.parameters()` 编译失败时返回 [`SchemaError`]。
    pub fn register(&mut self, tool: Arc<dyn Tool>) -> Result<Option<Arc<dyn Tool>>, SchemaError> {
        let schema = tool.parameters();
        let compiled = self.schema_cache.get_or_compile(&schema)?;
        let name = tool.name().to_owned();
        self.compiled.insert(name.clone(), compiled);
        Ok(self.tools.insert(name, tool))
    }

    /// 按名查找已注册的工具。
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Arc<dyn Tool>> {
        self.tools.get(name)
    }

    /// 已注册的工具数。
    #[must_use]
    pub fn len(&self) -> usize {
        self.tools.len()
    }

    /// 注册表是否为空。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    /// 下发给提供商的工具定义，按名升序排列。
    ///
    /// 排序是 prompt 缓存命中的必要条件，见模块文档；`HashMap` 的遍历序不提供任何
    /// 稳定性保证，所以这里必须显式排序，不能依赖插入顺序凑巧稳定。
    #[must_use]
    pub fn definitions(&self) -> Vec<zcode_ai::Tool> {
        let mut defs: Vec<zcode_ai::Tool> = self
            .tools
            .values()
            .map(|tool| zcode_ai::Tool {
                name: tool.name().to_owned(),
                description: tool.description().to_owned(),
                parameters: tool.parameters(),
                strict: None,
            })
            .collect();
        defs.sort_by(|a, b| a.name.cmp(&b.name));
        defs
    }

    /// 校验一次工具调用的参数。
    ///
    /// 失败时返回的文本已经过 [`render_validation_error`] 渲染，可以原样作为
    /// `is_error` 的工具结果喂回模型；`name` 不存在时返回 [`ToolRegistry::unknown_tool_message`]。
    /// 因为参数用的是注册期编译好的 [`CompiledSchema`]，这里不会重新解析 schema 本身。
    ///
    /// # Errors
    /// 参数不满足 schema，或 `name` 未注册时返回渲染好的错误文本。
    pub fn validate(&self, name: &str, args: &Value) -> Result<(), String> {
        let Some(compiled) = self.compiled.get(name) else {
            return Err(self.unknown_tool_message(name));
        };
        compiled
            .validate(args)
            .map_err(|error| render_validation_error(name, &error, args))
    }

    /// 未知工具名的推荐（最多 `MAX_SUGGESTIONS` 个）。
    ///
    /// 三级启发式打分，抄源 jcode `crates/jcode-app-core/src/tool/mod.rs:378-416`
    /// （`closest_tool_names`，issue #104：阻断模型在幻觉工具名上反复打转）：
    ///
    /// 1. 大小写无关完全相等（分数 0，理论上不会真的命中——命中就说明调用方大小写传错了）。
    /// 2. 前缀或包含关系（分数 1/2，覆盖"多打/少打了几个字符"这类最常见笔误）。
    /// 3. 有界 Levenshtein 编辑距离（分数 `3 + 距离`），阈值 `max(较长名字长度 / 3, 2)`——
    ///    只推荐"看起来像"的名字，完全不相干的候选距离必然超阈值，直接被过滤掉。
    ///
    /// 结果先按分数、分数相同再按字典序排序，取前 `MAX_SUGGESTIONS` 个。
    #[must_use]
    pub fn suggestions(&self, name: &str) -> Vec<&str> {
        let needle = name.trim().to_ascii_lowercase();
        if needle.is_empty() {
            return Vec::new();
        }
        let mut scored: Vec<(usize, &str)> = self
            .tools
            .keys()
            .filter_map(|candidate| {
                let hay = candidate.to_ascii_lowercase();
                let score = if hay == needle {
                    0
                } else if hay.starts_with(&needle) || needle.starts_with(&hay) {
                    1
                } else if hay.contains(&needle) || needle.contains(&hay) {
                    2
                } else {
                    let dist = levenshtein(&needle, &hay);
                    let threshold = (hay.len().max(needle.len()) / 3).max(2);
                    if dist > threshold {
                        return None;
                    }
                    3 + dist
                };
                Some((score, candidate.as_str()))
            })
            .collect();
        scored.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(b.1)));
        scored
            .into_iter()
            .take(MAX_SUGGESTIONS)
            .map(|(_, candidate)| candidate)
            .collect()
    }

    /// 未知工具名时喂回模型的说明：包含推荐（若有）与全部可用工具名。
    ///
    /// 形状抄源 jcode 的 `Unknown tool: {name}. Did you mean: {..}? Available tools: {..}.`
    /// （`crates/jcode-app-core/src/tool/mod.rs:565-569`）——同时给"猜"和"兜底列表"两条
    /// 恢复路径，模型不需要再发一轮工具调用去发现自己拼错了名字。
    #[must_use]
    pub fn unknown_tool_message(&self, name: &str) -> String {
        let mut available: Vec<&str> = self.tools.keys().map(String::as_str).collect();
        available.sort_unstable();
        let mut message = format!("Unknown tool: {name}.");
        let suggestions = self.suggestions(name);
        if !suggestions.is_empty() {
            message.push_str(" Did you mean: ");
            message.push_str(&suggestions.join(", "));
            message.push('?');
        }
        message.push_str(" Available tools: ");
        message.push_str(&available.join(", "));
        message.push('.');
        message
    }
}

/// 经典 Levenshtein 编辑距离（Unicode 码点级别），仅用于工具名"猜你是不是想输入"的
/// 近似匹配，两行滚动数组已经足够，不追求更优的算法。
///
/// 抄源 jcode `crates/jcode-app-core/src/tool/mod.rs:1068-1091`，按本仓
/// `clippy::indexing_slicing = deny` 的约束把裸 `[]` 索引换成 `.get()`/`.get_mut()`。
fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr: Vec<usize> = vec![0; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        if let Some(slot) = curr.get_mut(0) {
            *slot = i + 1;
        }
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            let delete = prev
                .get(j + 1)
                .copied()
                .unwrap_or(usize::MAX)
                .saturating_add(1);
            let insert = curr.get(j).copied().unwrap_or(usize::MAX).saturating_add(1);
            let substitute = prev
                .get(j)
                .copied()
                .unwrap_or(usize::MAX)
                .saturating_add(cost);
            if let Some(slot) = curr.get_mut(j + 1) {
                *slot = delete.min(insert).min(substitute);
            }
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev.get(b.len()).copied().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use serde_json::json;

    use super::*;
    use crate::error::ToolError;
    use crate::tool::{ToolContext, ToolOutput};

    #[derive(Debug)]
    struct StubTool {
        name: &'static str,
        schema: Value,
    }

    impl StubTool {
        fn new(name: &'static str) -> Self {
            Self {
                name,
                schema: json!({
                    "type": "object",
                    "properties": { "path": { "type": "string" } },
                    "required": ["path"],
                }),
            }
        }
    }

    #[async_trait]
    impl Tool for StubTool {
        fn name(&self) -> &str {
            self.name
        }

        fn description(&self) -> &'static str {
            "stub"
        }

        fn parameters(&self) -> Value {
            self.schema.clone()
        }

        async fn execute(&self, _args: Value, _ctx: ToolContext) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput::text("ok"))
        }
    }

    #[test]
    fn definitions_are_sorted_by_name() {
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(StubTool::new("write"))).unwrap();
        registry.register(Arc::new(StubTool::new("read"))).unwrap();
        registry.register(Arc::new(StubTool::new("edit"))).unwrap();

        let defs = registry.definitions();
        let names: Vec<&str> = defs.iter().map(|def| def.name.as_str()).collect();
        assert_eq!(names, vec!["edit", "read", "write"]);
    }

    #[test]
    fn register_rejects_invalid_schema_immediately() {
        let mut registry = ToolRegistry::new();
        let bad = Arc::new(StubTool {
            name: "bad",
            schema: json!({ "type": "not-a-real-type" }),
        });

        assert!(
            registry.register(bad).is_err(),
            "非法 schema 必须在 register 就报错，而不是等到 validate 才发现"
        );
        assert!(registry.is_empty(), "编译失败的工具不该留在注册表里");
    }

    #[test]
    fn validate_reports_the_offending_field() {
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(StubTool::new("read"))).unwrap();

        let error = registry.validate("read", &json!({})).unwrap_err();
        assert!(
            error.contains("path"),
            "错误文本必须点名缺失的字段：{error}"
        );
    }

    #[test]
    fn suggestions_recover_a_typo() {
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(StubTool::new("read"))).unwrap();
        registry.register(Arc::new(StubTool::new("write"))).unwrap();

        assert_eq!(registry.suggestions("raed"), vec!["read"]);
    }

    #[test]
    fn suggestions_stay_empty_for_unrelated_names() {
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(StubTool::new("read"))).unwrap();
        registry.register(Arc::new(StubTool::new("write"))).unwrap();

        assert!(
            registry.suggestions("xkcd_teleport").is_empty(),
            "完全不相干的名字不该硬凑推荐"
        );
    }

    #[test]
    fn unknown_tool_message_lists_suggestions_and_all_tools() {
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(StubTool::new("read"))).unwrap();

        let message = registry.unknown_tool_message("raed");
        assert!(message.contains("Did you mean: read?"), "{message}");
        assert!(message.contains("Available tools: read."), "{message}");
    }
}
