//! `zcode-schema` 的错误类型。
//!
//! 分两层：[`SchemaError`] 是编译期错误——schema 本身不合法，出现在
//! [`crate::compile::CompiledSchema::compile`]；[`ValidationError`]（携带一组
//! [`ValidationIssue`]）是运行期错误——schema 合法，但某个实例不满足它。

use std::fmt;

/// 编译期错误：schema 本身不合法，无法编译成 [`crate::compile::CompiledSchema`]。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SchemaError {
    /// schema 节点既不是 JSON 对象也不是布尔值（draft 2020-12 只允许这两种形状）。
    #[error("{pointer} 处的 schema 既不是对象也不是布尔值")]
    NotAnObjectOrBool {
        /// 出问题节点相对 schema 根的 JSON Pointer（根节点为空字符串）。
        pointer: String,
    },
    /// 某个 keyword 的取值形状不合法（如 `type` 既不是字符串也不是字符串数组）。
    #[error("{pointer} 处的 keyword `{keyword}` 不合法：{reason}")]
    InvalidKeyword {
        /// 出问题节点相对 schema 根的 JSON Pointer。
        pointer: String,
        /// 出问题的 keyword 名。
        keyword: &'static str,
        /// 具体不合法的原因。
        reason: String,
    },
    /// `$ref` 无法在本文档内解析——不是 `#` 开头的片段，或指向的 JSON Pointer 不存在。
    ///
    /// 外部 URI 引用（如 `"https://example.com/schema.json"`）一律落在这一类。
    #[error("{pointer} 处的 $ref `{reference}` 无法解析")]
    UnresolvableRef {
        /// 出问题节点相对 schema 根的 JSON Pointer。
        pointer: String,
        /// 未能解析的引用字符串。
        reference: String,
    },
    /// `$ref` 链退化成一串"纯引用"节点（`{"$ref": ...}` 首尾相连）且深度超过
    /// [`crate::validate::MAX_REF_DEPTH`]，判定为死循环引用。
    #[error("$ref `{reference}` 的解析深度超过上限")]
    RefDepthExceeded {
        /// 触发深度上限的起始引用字符串。
        reference: String,
    },
}

/// 实例不满足 schema 时，从实例根到出问题字段路径中的一段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathSegment {
    /// 对象属性名。
    Key(Box<str>),
    /// 数组下标。
    Index(usize),
}

impl fmt::Display for PathSegment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Key(key) => write!(f, "{key}"),
            Self::Index(index) => write!(f, "{index}"),
        }
    }
}

/// JSON 值的类型标签，用于 `type` 校验失败时告知期望类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonType {
    /// `null`。
    Null,
    /// `true`/`false`。
    Boolean,
    /// JSON 对象。
    Object,
    /// JSON 数组。
    Array,
    /// 任意 JSON 数字。
    Number,
    /// 小数部分为零的 JSON 数字（`2.0` 也算，见校验器的容忍性说明）。
    Integer,
    /// JSON 字符串。
    String,
}

impl JsonType {
    /// 该类型在错误文本里的展示名（与 JSON Schema `type` keyword 的取值同名）。
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Null => "null",
            Self::Boolean => "boolean",
            Self::Object => "object",
            Self::Array => "array",
            Self::Number => "number",
            Self::Integer => "integer",
            Self::String => "string",
        }
    }
}

impl fmt::Display for JsonType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 一条校验失败。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationIssue {
    /// 从实例根到出问题字段的路径；空表示根本身。
    pub path: Vec<PathSegment>,
    /// 面向人类/模型的错误描述。
    pub message: String,
    /// 触发该 issue 的 keyword 名（如 `"type"`、`"required"`、`"anyOf"`）。
    pub keyword: &'static str,
    /// `type` 校验失败时的期望类型集合；其他 keyword 通常留空。
    pub expected_types: Vec<JsonType>,
    /// 该 issue 是否落在 `anyOf`/`oneOf` 组合子自身的路径上。
    ///
    /// 只有组合子本身产出的"没有任何分支满足"这类汇总 issue 才置位；分支内部探测时
    /// 产生的更深层字段 issue 从不上浮到 [`crate::compile::CompiledSchema::collect_issues`]
    /// 的输出里，因此永远不会带着这个标记出现在更深的路径上。下游用它抑制对组合子分支的
    /// 自动修复尝试（分支本身是"或"关系，没有单一"正确"修法）。
    pub from_union_branch: bool,
}

impl ValidationIssue {
    /// 渲染 `path`：根层写字面量 `"root"`，否则各段用 `/` 连接（如 `"items/0/name"`）。
    #[must_use]
    pub fn display_path(&self) -> String {
        if self.path.is_empty() {
            "root".to_string()
        } else {
            self.path
                .iter()
                .map(PathSegment::to_string)
                .collect::<Vec<_>>()
                .join("/")
        }
    }
}

/// 一次校验失败的完整结果：一个或多个 [`ValidationIssue`]，按发现顺序排列。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError {
    /// 所有失败点。
    pub issues: Vec<ValidationIssue>,
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, issue) in self.issues.iter().enumerate() {
            if index > 0 {
                writeln!(f)?;
            }
            write!(f, "{}: {}", issue.display_path(), issue.message)?;
        }
        Ok(())
    }
}

impl std::error::Error for ValidationError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_path_root_is_literal() {
        let issue = ValidationIssue {
            path: Vec::new(),
            message: "不满足".to_string(),
            keyword: "type",
            expected_types: Vec::new(),
            from_union_branch: false,
        };
        assert_eq!(issue.display_path(), "root");
    }

    #[test]
    fn display_path_joins_segments_with_slash() {
        let issue = ValidationIssue {
            path: vec![
                PathSegment::Key("items".into()),
                PathSegment::Index(0),
                PathSegment::Key("name".into()),
            ],
            message: "不满足".to_string(),
            keyword: "type",
            expected_types: Vec::new(),
            from_union_branch: false,
        };
        assert_eq!(issue.display_path(), "items/0/name");
    }

    #[test]
    fn validation_error_display_joins_issues_by_line() {
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
                    expected_types: vec![JsonType::Number],
                    from_union_branch: false,
                },
            ],
        };
        let rendered = error.to_string();
        assert_eq!(
            rendered,
            "root: 缺少必需属性 \"path\"\ncount: 期望类型为 number，实际是 string"
        );
    }

    #[test]
    fn schema_error_messages_include_pointer_and_keyword() {
        let error = SchemaError::InvalidKeyword {
            pointer: "/properties/x".to_string(),
            keyword: "type",
            reason: "必须是字符串或字符串数组".to_string(),
        };
        assert_eq!(
            error.to_string(),
            "/properties/x 处的 keyword `type` 不合法：必须是字符串或字符串数组"
        );
    }
}
