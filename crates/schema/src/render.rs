//! 把校验失败渲染成回灌给模型的错误文本。
//!
//! 形状对齐 oh-my-pi 的 tool-call 校验错误渲染（`validation.ts:1619-1621,1838-1851,1968-1979`），
//! 但补了上游没有的截断递归深度上限（见 [`MAX_TRUNCATE_DEPTH`]）。

use std::fmt::Write as _;

use serde_json::{Map, Value};

use crate::error::ValidationError;

/// 单个字符串字段在错误文本里保留的最大字符数（Unicode 码点，非字节）。
///
/// write/edit 类工具调用的整包参数可达数百 KB，且校验失败后会原样回灌给模型上下文；
/// 256 是"看得出是什么内容"与"不烧 token"的折中，照搬上游同名常量的取值。
pub const MAX_ERROR_ARG_STRING_LENGTH: usize = 256;

/// [`truncate_args_for_error`] 递归下降的深度上限。
///
/// 上游对应逻辑没有深度上限（`validation.ts:1838-1851`），是已知缺陷；这里补上，
/// 防御极端嵌套（或恶意构造）的参数把渲染过程拖成与深度成正比的开销甚至栈溢出风险。
pub const MAX_TRUNCATE_DEPTH: usize = 32;

/// 渲染一段可直接回灌给模型的校验失败说明。
///
/// 输出形状固定：
///
/// ```text
/// Validation failed for tool "<name>":
///   - <path>: <message>
///   - <path>: <message>
///
/// Received arguments:
/// <缩进 2 格的 pretty JSON>
/// ```
///
/// `received` 会先经 [`truncate_args_for_error`] 截断长字符串，再原样打印，不做任何
/// 语义改写（模型需要看到自己传了什么）。
#[must_use]
pub fn render_validation_error(
    tool_name: &str,
    error: &ValidationError,
    received: &Value,
) -> String {
    let truncated = truncate_args_for_error(received);
    let pretty = serde_json::to_string_pretty(&truncated).unwrap_or_else(|_| truncated.to_string());
    let indented = indent_lines(&pretty, "  ");

    let mut rendered = format!("Validation failed for tool \"{tool_name}\":\n");
    for issue in &error.issues {
        let _ = writeln!(rendered, "  - {}: {}", issue.display_path(), issue.message);
    }
    rendered.push_str("\nReceived arguments:\n");
    rendered.push_str(&indented);
    rendered
}

fn indent_lines(text: &str, prefix: &str) -> String {
    text.lines()
        .map(|line| format!("{prefix}{line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 递归截断 `value` 里的每个字符串字段到 [`MAX_ERROR_ARG_STRING_LENGTH`] 个字符。
///
/// 超出 [`MAX_TRUNCATE_DEPTH`] 层的子树整体替换成 `"… [depth limit]"`（上游没有这层保护）。
#[must_use]
pub fn truncate_args_for_error(value: &Value) -> Value {
    truncate_depth(value, 0)
}

fn truncate_depth(value: &Value, depth: usize) -> Value {
    if depth >= MAX_TRUNCATE_DEPTH {
        return Value::String("… [depth limit]".to_string());
    }
    match value {
        Value::String(s) => Value::String(truncate_string(s)),
        Value::Array(items) => {
            Value::Array(items.iter().map(|v| truncate_depth(v, depth + 1)).collect())
        }
        Value::Object(map) => {
            let mut out = Map::with_capacity(map.len());
            for (key, v) in map {
                out.insert(key.clone(), truncate_depth(v, depth + 1));
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

fn truncate_string(s: &str) -> String {
    let total = s.chars().count();
    if total <= MAX_ERROR_ARG_STRING_LENGTH {
        return s.to_string();
    }
    let head: String = s.chars().take(MAX_ERROR_ARG_STRING_LENGTH).collect();
    format!(
        "{head}… [truncated {} chars]",
        total - MAX_ERROR_ARG_STRING_LENGTH
    )
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::error::{PathSegment, ValidationIssue};

    #[test]
    fn render_matches_expected_shape() {
        let error = ValidationError {
            issues: vec![
                ValidationIssue {
                    path: Vec::new(),
                    message: "缺少必需属性 \"path\"".to_string(),
                    keyword: "required",
                    expected_types: Vec::new(),
                    from_union_branch: false,
                },
                ValidationIssue {
                    path: vec![PathSegment::Key("count".into())],
                    message: "期望类型为 number，实际是 string".to_string(),
                    keyword: "type",
                    expected_types: Vec::new(),
                    from_union_branch: false,
                },
            ],
        };
        let received = json!({ "count": "3" });
        let rendered = render_validation_error("read_file", &error, &received);

        let expected = concat!(
            "Validation failed for tool \"read_file\":\n",
            "  - root: 缺少必需属性 \"path\"\n",
            "  - count: 期望类型为 number，实际是 string\n",
            "\n",
            "Received arguments:\n",
            "  {\n",
            "    \"count\": \"3\"\n",
            "  }",
        );
        assert_eq!(rendered, expected);
    }

    #[test]
    fn truncate_string_field_adds_suffix_with_remaining_count() {
        let long = "a".repeat(MAX_ERROR_ARG_STRING_LENGTH + 10);
        let value = json!({ "content": long });
        let truncated = truncate_args_for_error(&value);
        let content = truncated.get("content").and_then(Value::as_str).unwrap();
        assert!(content.starts_with(&"a".repeat(MAX_ERROR_ARG_STRING_LENGTH)));
        assert!(content.ends_with("… [truncated 10 chars]"));
    }

    #[test]
    fn truncate_short_string_is_unchanged() {
        let value = json!({ "content": "short" });
        assert_eq!(truncate_args_for_error(&value), value);
    }

    #[test]
    fn truncate_depth_limit_replaces_deep_subtree() {
        let mut nested = json!("leaf");
        for _ in 0..(MAX_TRUNCATE_DEPTH + 5) {
            nested = json!([nested]);
        }
        let truncated = truncate_args_for_error(&nested);

        // 沿着数组一路下钻，必须在 MAX_TRUNCATE_DEPTH 层内遇到深度限制占位符。
        let mut cursor = &truncated;
        let mut hit_limit = false;
        for _ in 0..(MAX_TRUNCATE_DEPTH + 5) {
            match cursor {
                Value::Array(items) => cursor = items.first().expect("已知构造为单元素数组"),
                Value::String(s) if s == "… [depth limit]" => {
                    hit_limit = true;
                    break;
                }
                _ => break,
            }
        }
        assert!(
            hit_limit,
            "超过 MAX_TRUNCATE_DEPTH 的子树应该被替换成深度限制占位符"
        );
    }
}
