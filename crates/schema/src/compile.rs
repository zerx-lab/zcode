//! 惰性编译 + 按内容哈希缓存的 schema 编译器。
//!
//! [`CompiledSchema::compile`] 对 schema 做一趟结构性预检（校验每个节点是对象或布尔值、
//! 校验已知 keyword 的取值形状、解析并校验 `$ref`、预编译 `pattern` 正则），产出的
//! [`CompiledSchema`] 之后可反复调用 [`CompiledSchema::validate`] 而不再重新解析。
//!
//! # `pattern` 是 fail-closed 的
//!
//! JSON Schema 的 `pattern`/`patternProperties` 是 ECMA-262 正则语义，`regex` crate
//! 不支持 look-around、反向引用这类写法。遇到编译不了的 pattern，**不会**把这条 keyword
//! 悄悄降级成"不校验"——那等价于让约束静默失效：`{"pattern":"^(?!admin$).+"}` 对
//! `"admin"` 会被判通过，这是一个错误答案，比"不支持"更糟。因此 `compile()` 在这种情况下
//! 直接返回 [`SchemaError::InvalidKeyword`]，让调用方在注册工具时就发现这份 schema 在本
//! 引擎上不成立，而不是在运行期悄悄放行本该被拒绝的实例。校验器存在的价值就是"说通过就是
//! 真通过"：宁可拒绝一个我们支持不了的 schema，也不给假阳性。
//!
//! [`CompiledSchema::unsupported_keywords`] 因此只报告语义上**确实宽松处理**的 keyword
//! （`unevaluatedProperties`/`unevaluatedItems`——2020-12 里需要跨兄弟 keyword 联合求值，
//! 本校验器的单趟结构没有这个信息，故意不实现），不再承载 `pattern`。
//!
//! # 缓存 key 是内容哈希
//!
//! [`SchemaCache`] 按 schema 的**内容哈希**去重编译结果，多个工具共享同一份 schema
//! （常见于同一 provider 的多个 tool 定义复用同一个参数子 schema）时只编译一次。
//! 这里有意反着 oh-my-pi 的 `stamps.ts` 做——上游用 JS 对象身份做 key，schema 原地改写
//! 内容时缓存不会失效，靠"约定不原地改写"兜底，是已记录的技术债。内容哈希遇到碰撞时
//! （`u64` 空间里概率极低，但不是零）用 `==` 复核，不等则重新编译并覆盖旧条目。

use std::collections::{BTreeSet, HashMap};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{Arc, RwLock};

use regex::Regex;
use serde_json::{Map, Value};

use crate::error::{SchemaError, ValidationError, ValidationIssue};
use crate::validate::{self, MAX_REF_DEPTH};

/// 编译好的 schema：结构已校验，`pattern` 已预编译成 [`Regex`]，可反复校验实例。
#[derive(Debug)]
pub struct CompiledSchema {
    root: Value,
    patterns: HashMap<Box<str>, Regex>,
    unsupported: Vec<&'static str>,
}

impl CompiledSchema {
    /// 编译一个 JSON Schema（draft 2020-12 子集）。
    ///
    /// 递归校验每个子 schema 节点是对象或布尔值、校验已知 keyword 的取值形状、
    /// 解析并校验本文档内的 `$ref`（外部 URI 引用一律报错）、预编译 `pattern` 正则
    /// （fail-closed：编译不了直接报错，见模块文档）。
    ///
    /// # Errors
    /// schema 结构不合法、`$ref` 无法解析、`pattern` 不是 `regex` crate 支持的语法、
    /// 或 `$ref` 链退化成纯引用死循环时返回 [`SchemaError`]。
    pub fn compile(schema: Value) -> Result<Self, SchemaError> {
        let mut patterns = HashMap::new();
        let mut unsupported = BTreeSet::new();
        walk(
            String::new(),
            &schema,
            &schema,
            &mut patterns,
            &mut unsupported,
        )?;
        Ok(Self {
            root: schema,
            patterns,
            unsupported: unsupported.into_iter().collect(),
        })
    }

    /// 校验一个 JSON 实例，返回聚合的所有失败点；全部满足时返回 `Ok(())`。
    ///
    /// # Errors
    /// 实例不满足 schema 时返回携带所有 [`ValidationIssue`] 的 [`ValidationError`]。
    pub fn validate(&self, instance: &Value) -> Result<(), ValidationError> {
        let mut issues = Vec::new();
        self.collect_issues(instance, &mut issues);
        if issues.is_empty() {
            Ok(())
        } else {
            Err(ValidationError { issues })
        }
    }

    /// 校验一个 JSON 实例，把所有失败点追加到 `out`（不清空 `out` 已有内容）。
    pub fn collect_issues(&self, instance: &Value, out: &mut Vec<ValidationIssue>) {
        validate::validate_root(&self.root, &self.patterns, instance, out);
    }

    /// 编译后的 schema 原始 JSON（供调试/序列化透传）。
    #[must_use]
    pub fn as_value(&self) -> &Value {
        &self.root
    }

    /// 本次编译遇到但不支持、按宽松语义处理的 keyword 名（如 `unevaluatedProperties`）。
    ///
    /// 空切片表示 schema 里出现的所有 keyword 都被完整支持。`pattern` 编译失败不会出现
    /// 在这里——那会让 [`CompiledSchema::compile`] 直接返回 [`SchemaError`]，见模块文档。
    #[must_use]
    pub fn unsupported_keywords(&self) -> &[&'static str] {
        &self.unsupported
    }
}

/// 递归遍历 schema 树：校验节点形状、编译 `pattern` 正则、校验 `$ref` 可解析性。
///
/// `pointer` 是当前节点相对 schema 根的 JSON Pointer（错误消息用），`node` 是当前节点，
/// `root` 是整份 schema（`$ref` 解析基准，遍历过程中不变）。遍历顺着 schema 的字面结构
/// 走（`properties`/`$defs` 等），从不跟随 `$ref` 展开引用目标——`$defs`/`definitions`
/// 本身也在遍历范围内，所以引用目标迟早会被结构性地访问到；这也是遍历天然无环、
/// 不需要深度保护的原因（唯一的例外是纯引用链探测，见 [`chase_pure_ref_chain`]）。
fn validate_shapes(obj: &Map<String, Value>, pointer: &str) -> Result<(), SchemaError> {
    validate_type_keyword(obj, pointer)?;
    require_nonempty_array_shape(obj, "enum", pointer)?;
    require_string_array_shape(obj, "required", pointer)?;
    require_boolean_shape(obj, "uniqueItems", pointer)?;
    for key in [
        "minLength",
        "maxLength",
        "minItems",
        "maxItems",
        "minProperties",
        "maxProperties",
        "minContains",
        "maxContains",
    ] {
        require_non_negative_integer_shape(obj, key, pointer)?;
    }
    for key in ["minimum", "maximum", "exclusiveMinimum", "exclusiveMaximum"] {
        require_number_shape(obj, key, pointer)?;
    }
    require_positive_number_shape(obj, "multipleOf", pointer)?;
    validate_dependent_required(obj, pointer)
}

fn walk(
    pointer: String,
    node: &Value,
    root: &Value,
    patterns: &mut HashMap<Box<str>, Regex>,
    unsupported: &mut BTreeSet<&'static str>,
) -> Result<(), SchemaError> {
    if node.is_boolean() {
        return Ok(());
    }
    let Some(obj) = node.as_object() else {
        return Err(SchemaError::NotAnObjectOrBool { pointer });
    };

    validate_shapes(obj, &pointer)?;

    if let Some(pat) = obj.get("pattern") {
        let Some(pat_str) = pat.as_str() else {
            return Err(SchemaError::InvalidKeyword {
                pointer,
                keyword: "pattern",
                reason: "必须是字符串".to_string(),
            });
        };
        compile_pattern(&pointer, pat_str, patterns)?;
    }

    if let Some(refv) = obj.get("$ref") {
        let Some(ref_str) = refv.as_str() else {
            return Err(SchemaError::InvalidKeyword {
                pointer,
                keyword: "$ref",
                reason: "必须是字符串".to_string(),
            });
        };
        resolve_ref(ref_str, root, &pointer)?;
        chase_pure_ref_chain(ref_str, root)?;
    }

    if obj.contains_key("unevaluatedProperties") {
        unsupported.insert("unevaluatedProperties");
    }
    if obj.contains_key("unevaluatedItems") {
        unsupported.insert("unevaluatedItems");
    }

    walk_map_values(&pointer, obj, "properties", root, patterns, unsupported)?;
    walk_map_values(
        &pointer,
        obj,
        "dependentSchemas",
        root,
        patterns,
        unsupported,
    )?;
    walk_map_values(&pointer, obj, "$defs", root, patterns, unsupported)?;
    walk_map_values(&pointer, obj, "definitions", root, patterns, unsupported)?;

    walk_pattern_properties(&pointer, obj, root, patterns, unsupported)?;

    for key in [
        "additionalProperties",
        "propertyNames",
        "items",
        "contains",
        "not",
        "if",
        "then",
        "else",
    ] {
        if let Some(sub) = obj.get(key) {
            walk(format!("{pointer}/{key}"), sub, root, patterns, unsupported)?;
        }
    }

    walk_prefix_items(&pointer, obj, root, patterns, unsupported)?;

    for key in ["allOf", "anyOf", "oneOf"] {
        if let Some(value) = obj.get(key) {
            let Some(arr) = value.as_array() else {
                return Err(SchemaError::InvalidKeyword {
                    pointer,
                    keyword: key,
                    reason: "必须是数组".to_string(),
                });
            };
            if arr.is_empty() {
                return Err(SchemaError::InvalidKeyword {
                    pointer,
                    keyword: key,
                    reason: "不能是空数组".to_string(),
                });
            }
            for (i, sub) in arr.iter().enumerate() {
                walk(
                    format!("{pointer}/{key}/{i}"),
                    sub,
                    root,
                    patterns,
                    unsupported,
                )?;
            }
        }
    }

    Ok(())
}

/// 遍历 `patternProperties`：键本身是正则，需要连同子 schema 一起编译。
fn walk_pattern_properties(
    pointer: &str,
    obj: &Map<String, Value>,
    root: &Value,
    patterns: &mut HashMap<Box<str>, Regex>,
    unsupported: &mut BTreeSet<&'static str>,
) -> Result<(), SchemaError> {
    let Some(pp) = obj.get("patternProperties") else {
        return Ok(());
    };
    let Some(pp_obj) = pp.as_object() else {
        return Err(SchemaError::InvalidKeyword {
            pointer: pointer.to_owned(),
            keyword: "patternProperties",
            reason: "必须是对象（正则 -> 子 schema 的映射）".to_string(),
        });
    };
    for (pat_key, sub) in pp_obj {
        compile_pattern(pointer, pat_key, patterns)?;
        walk(
            format!("{pointer}/patternProperties/{pat_key}"),
            sub,
            root,
            patterns,
            unsupported,
        )?;
    }
    Ok(())
}

/// 遍历 `prefixItems` 数组里的每个子 schema。
fn walk_prefix_items(
    pointer: &str,
    obj: &Map<String, Value>,
    root: &Value,
    patterns: &mut HashMap<Box<str>, Regex>,
    unsupported: &mut BTreeSet<&'static str>,
) -> Result<(), SchemaError> {
    let Some(items) = obj.get("prefixItems") else {
        return Ok(());
    };
    let Some(arr) = items.as_array() else {
        return Err(SchemaError::InvalidKeyword {
            pointer: pointer.to_owned(),
            keyword: "prefixItems",
            reason: "必须是数组".to_string(),
        });
    };
    for (i, sub) in arr.iter().enumerate() {
        walk(
            format!("{pointer}/prefixItems/{i}"),
            sub,
            root,
            patterns,
            unsupported,
        )?;
    }
    Ok(())
}

/// 遍历 `obj[key]`（一个 `名字 -> 子 schema` 的映射，如 `properties`/`$defs`）的每个值。
fn walk_map_values(
    pointer: &str,
    obj: &Map<String, Value>,
    key: &'static str,
    root: &Value,
    patterns: &mut HashMap<Box<str>, Regex>,
    unsupported: &mut BTreeSet<&'static str>,
) -> Result<(), SchemaError> {
    let Some(value) = obj.get(key) else {
        return Ok(());
    };
    let Some(map) = value.as_object() else {
        return Err(SchemaError::InvalidKeyword {
            pointer: pointer.to_string(),
            keyword: key,
            reason: "必须是对象（名字 -> 子 schema 的映射）".to_string(),
        });
    };
    for (name, sub) in map {
        walk(
            format!("{pointer}/{key}/{name}"),
            sub,
            root,
            patterns,
            unsupported,
        )?;
    }
    Ok(())
}

/// `type` 必须是字符串或字符串数组，且每个取值都必须是已知的 JSON 类型名。
fn validate_type_keyword(obj: &Map<String, Value>, pointer: &str) -> Result<(), SchemaError> {
    let Some(ty) = obj.get("type") else {
        return Ok(());
    };
    let names: Vec<&str> = if let Some(s) = ty.as_str() {
        vec![s]
    } else if let Some(arr) = ty.as_array() {
        let mut collected = Vec::with_capacity(arr.len());
        for item in arr {
            let Some(s) = item.as_str() else {
                return Err(SchemaError::InvalidKeyword {
                    pointer: pointer.to_string(),
                    keyword: "type",
                    reason: "数组元素必须是字符串".to_string(),
                });
            };
            collected.push(s);
        }
        collected
    } else {
        return Err(SchemaError::InvalidKeyword {
            pointer: pointer.to_string(),
            keyword: "type",
            reason: "必须是字符串或字符串数组".to_string(),
        });
    };
    for name in names {
        if validate::json_type_from_name(name).is_none() {
            return Err(SchemaError::InvalidKeyword {
                pointer: pointer.to_string(),
                keyword: "type",
                reason: format!(
                    "未知的类型名 \"{name}\"（必须是 null|boolean|object|array|number|integer|string 之一）"
                ),
            });
        }
    }
    Ok(())
}

/// `dependentRequired` 必须是对象，且每个值必须是字符串数组（不是子 schema）。
fn validate_dependent_required(obj: &Map<String, Value>, pointer: &str) -> Result<(), SchemaError> {
    let Some(value) = obj.get("dependentRequired") else {
        return Ok(());
    };
    let Some(map) = value.as_object() else {
        return Err(SchemaError::InvalidKeyword {
            pointer: pointer.to_string(),
            keyword: "dependentRequired",
            reason: "必须是对象（属性名 -> 字符串数组的映射）".to_string(),
        });
    };
    for (trigger, siblings) in map {
        let ok = siblings
            .as_array()
            .is_some_and(|arr| arr.iter().all(Value::is_string));
        if !ok {
            return Err(SchemaError::InvalidKeyword {
                pointer: pointer.to_string(),
                keyword: "dependentRequired",
                reason: format!("\"{trigger}\" 对应的值必须是字符串数组"),
            });
        }
    }
    Ok(())
}

/// `key` 存在时必须是布尔值。
fn require_boolean_shape(
    obj: &Map<String, Value>,
    key: &'static str,
    pointer: &str,
) -> Result<(), SchemaError> {
    match obj.get(key) {
        None | Some(Value::Bool(_)) => Ok(()),
        Some(_) => Err(SchemaError::InvalidKeyword {
            pointer: pointer.to_string(),
            keyword: key,
            reason: "必须是布尔值".to_string(),
        }),
    }
}

/// `key` 存在时必须是任意 JSON 数字。
fn require_number_shape(
    obj: &Map<String, Value>,
    key: &'static str,
    pointer: &str,
) -> Result<(), SchemaError> {
    let Some(value) = obj.get(key) else {
        return Ok(());
    };
    if value.as_f64().is_some() {
        Ok(())
    } else {
        Err(SchemaError::InvalidKeyword {
            pointer: pointer.to_string(),
            keyword: key,
            reason: "必须是数值".to_string(),
        })
    }
}

/// `key` 存在时必须是大于 0 的 JSON 数字（`multipleOf` 专用：0 或负数没有意义）。
fn require_positive_number_shape(
    obj: &Map<String, Value>,
    key: &'static str,
    pointer: &str,
) -> Result<(), SchemaError> {
    let Some(value) = obj.get(key) else {
        return Ok(());
    };
    let ok = value.as_f64().is_some_and(|n| n.is_finite() && n > 0.0);
    if ok {
        Ok(())
    } else {
        Err(SchemaError::InvalidKeyword {
            pointer: pointer.to_string(),
            keyword: key,
            reason: "必须是大于 0 的数值".to_string(),
        })
    }
}

/// `key` 存在时必须是非负整数——接受 `as_u64()` 直接命中的整数字面量，也接受写成浮点
/// 字面量但小数部分为零的形式（如 `5.0`），与运行期 `type: "integer"` 的容忍度一致。
fn require_non_negative_integer_shape(
    obj: &Map<String, Value>,
    key: &'static str,
    pointer: &str,
) -> Result<(), SchemaError> {
    let Some(value) = obj.get(key) else {
        return Ok(());
    };
    let ok = value.as_u64().is_some()
        || value
            .as_f64()
            .is_some_and(|n| n.is_finite() && n >= 0.0 && n.fract() == 0.0);
    if ok {
        Ok(())
    } else {
        Err(SchemaError::InvalidKeyword {
            pointer: pointer.to_string(),
            keyword: key,
            reason: "必须是非负整数".to_string(),
        })
    }
}

/// `key` 存在时必须是非空数组（元素类型不限，`enum` 专用——任意 JSON 值都能出现在里面）。
fn require_nonempty_array_shape(
    obj: &Map<String, Value>,
    key: &'static str,
    pointer: &str,
) -> Result<(), SchemaError> {
    let Some(value) = obj.get(key) else {
        return Ok(());
    };
    let Some(arr) = value.as_array() else {
        return Err(SchemaError::InvalidKeyword {
            pointer: pointer.to_string(),
            keyword: key,
            reason: "必须是数组".to_string(),
        });
    };
    if arr.is_empty() {
        return Err(SchemaError::InvalidKeyword {
            pointer: pointer.to_string(),
            keyword: key,
            reason: "不能是空数组".to_string(),
        });
    }
    Ok(())
}

/// `key` 存在时必须是字符串数组（允许空数组——`required: []` 合法，表示没有必需属性）。
fn require_string_array_shape(
    obj: &Map<String, Value>,
    key: &'static str,
    pointer: &str,
) -> Result<(), SchemaError> {
    let Some(value) = obj.get(key) else {
        return Ok(());
    };
    let ok = value
        .as_array()
        .is_some_and(|arr| arr.iter().all(Value::is_string));
    if ok {
        Ok(())
    } else {
        Err(SchemaError::InvalidKeyword {
            pointer: pointer.to_string(),
            keyword: key,
            reason: "必须是字符串数组".to_string(),
        })
    }
}

/// 编译一个 `pattern`/`patternProperties` 键用到的正则并存进 `patterns`。
///
/// fail-closed（见模块文档）：`regex` crate 编译不了时直接返回 [`SchemaError::InvalidKeyword`]，
/// 不放行、不降级。
fn compile_pattern(
    pointer: &str,
    pattern: &str,
    patterns: &mut HashMap<Box<str>, Regex>,
) -> Result<(), SchemaError> {
    if patterns.contains_key(pattern) {
        return Ok(());
    }
    let regex = Regex::new(pattern).map_err(|err| SchemaError::InvalidKeyword {
        pointer: pointer.to_string(),
        keyword: "pattern",
        reason: format!("regex 引擎无法编译（可能用了 ECMA-262 专有语法）：{err}"),
    })?;
    patterns.insert(pattern.into(), regex);
    Ok(())
}

/// 校验 `$ref` 能在本文档内解析：必须是 `#` 开头的 JSON Pointer 片段，且 `root.pointer()`
/// 能定位到值；外部 URI 引用（不以 `#` 开头）一律报错。
fn resolve_ref<'a>(
    reference: &str,
    root: &'a Value,
    pointer: &str,
) -> Result<&'a Value, SchemaError> {
    let Some(fragment) = reference.strip_prefix('#') else {
        return Err(SchemaError::UnresolvableRef {
            pointer: pointer.to_string(),
            reference: reference.to_string(),
        });
    };
    root.pointer(fragment)
        .ok_or_else(|| SchemaError::UnresolvableRef {
            pointer: pointer.to_string(),
            reference: reference.to_string(),
        })
}

/// 沿着"纯引用"（`{"$ref": ...}` 单键节点）链条前进，探测退化成死循环的 `$ref` 链。
///
/// 只在目标节点**恰好只有 `$ref` 一个键**时继续追踪——一旦目标节点带有其他 keyword
/// （真正的结构，如 `properties`），针对它的实例校验必然随结构下降而终止（有 `properties`
/// 就意味着实例要真的往下钻一层才能继续匹配 `$ref`，不可能在同一个实例节点上原地循环），
/// 不需要这层保险丝。真正的风险只在纯引用互指时出现（`A: {"$ref": B}`，`B: {"$ref": A}`），
/// 这类链条不消耗任何实例结构，运行期的 `(ref, 实例身份)` 配对去环无法在编译期预判到，
/// 所以在这里单独用有限步数的追踪兜底。
fn chase_pure_ref_chain(start: &str, root: &Value) -> Result<(), SchemaError> {
    let mut current = start.to_string();
    for _ in 0..MAX_REF_DEPTH {
        let Some(fragment) = current.strip_prefix('#') else {
            return Ok(());
        };
        let Some(target) = root.pointer(fragment) else {
            return Ok(());
        };
        let Some(obj) = target.as_object() else {
            return Ok(());
        };
        if obj.len() != 1 {
            return Ok(());
        }
        let Some(next) = obj.get("$ref").and_then(Value::as_str) else {
            return Ok(());
        };
        current = next.to_string();
    }
    Err(SchemaError::RefDepthExceeded {
        reference: start.to_string(),
    })
}

/// 按 schema 内容哈希缓存的编译结果；多处引用同一份 schema（值相等，不要求同一对象）时只编译一次。
#[derive(Debug, Default)]
pub struct SchemaCache {
    entries: RwLock<HashMap<u64, Arc<CompiledSchema>>>,
}

impl SchemaCache {
    /// 创建一个空缓存。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 取得 `schema` 的编译结果；已缓存则直接返回，否则编译并存入缓存。
    ///
    /// 缓存 key 是 `schema` 的内容哈希；命中后用 `==` 复核内容（防哈希碰撞），不等则
    /// 重新编译并覆盖旧条目。
    ///
    /// # Errors
    /// `schema` 编译失败时返回 [`SchemaError`]，不写入缓存。
    pub fn get_or_compile(&self, schema: &Value) -> Result<Arc<CompiledSchema>, SchemaError> {
        let key = content_hash(schema);
        if let Ok(guard) = self.entries.read()
            && let Some(hit) = guard.get(&key)
            && hit.as_value() == schema
        {
            return Ok(Arc::clone(hit));
        }
        let compiled = Arc::new(CompiledSchema::compile(schema.clone())?);
        if let Ok(mut guard) = self.entries.write() {
            guard.insert(key, Arc::clone(&compiled));
        }
        Ok(compiled)
    }

    /// 当前缓存条目数。
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.read().map_or(0, |guard| guard.len())
    }

    /// 缓存是否为空。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 清空缓存。
    pub fn clear(&self) {
        if let Ok(mut guard) = self.entries.write() {
            guard.clear();
        }
    }
}

/// 对 `serde_json::Value` 做稳定的内容哈希：对象键先排序再入哈，避免 `serde_json::Map`
/// 保留的插入顺序不同、内容相同的两份 schema 得到不同哈希；浮点数按 `to_bits()` 入哈
/// （`f64` 未实现 `Hash`）。
fn content_hash(value: &Value) -> u64 {
    let mut hasher = DefaultHasher::new();
    hash_value(value, &mut hasher);
    hasher.finish()
}

fn hash_value(value: &Value, hasher: &mut impl Hasher) {
    match value {
        Value::Null => 0u8.hash(hasher),
        Value::Bool(b) => {
            1u8.hash(hasher);
            b.hash(hasher);
        }
        Value::Number(n) => {
            2u8.hash(hasher);
            n.as_f64().unwrap_or(f64::NAN).to_bits().hash(hasher);
        }
        Value::String(s) => {
            3u8.hash(hasher);
            s.hash(hasher);
        }
        Value::Array(items) => {
            4u8.hash(hasher);
            items.len().hash(hasher);
            for item in items {
                hash_value(item, hasher);
            }
        }
        Value::Object(obj) => {
            5u8.hash(hasher);
            let mut keys: Vec<&String> = obj.keys().collect();
            keys.sort_unstable();
            keys.len().hash(hasher);
            for key in keys {
                key.hash(hasher);
                if let Some(v) = obj.get(key) {
                    hash_value(v, hasher);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn compile_rejects_non_object_non_bool_node() {
        let err = CompiledSchema::compile(json!({ "properties": { "a": 5 } })).unwrap_err();
        assert!(
            matches!(err, SchemaError::NotAnObjectOrBool { pointer } if pointer == "/properties/a")
        );
    }

    #[test]
    fn compile_rejects_malformed_type_shape() {
        let err = CompiledSchema::compile(json!({ "type": 5 })).unwrap_err();
        assert!(matches!(
            err,
            SchemaError::InvalidKeyword {
                keyword: "type",
                ..
            }
        ));
    }

    #[test]
    fn compile_rejects_external_ref() {
        let err = CompiledSchema::compile(json!({ "$ref": "https://example.com/schema.json" }))
            .unwrap_err();
        assert!(matches!(err, SchemaError::UnresolvableRef { .. }));
    }

    #[test]
    fn compile_rejects_unresolvable_local_ref() {
        let err = CompiledSchema::compile(json!({ "$ref": "#/$defs/missing" })).unwrap_err();
        assert!(matches!(err, SchemaError::UnresolvableRef { .. }));
    }

    #[test]
    fn compile_rejects_pure_ref_cycle_via_depth_fuse() {
        let schema = json!({
            "$ref": "#/$defs/a",
            "$defs": { "a": { "$ref": "#/$defs/b" }, "b": { "$ref": "#/$defs/a" } }
        });
        let err = CompiledSchema::compile(schema).unwrap_err();
        assert!(matches!(err, SchemaError::RefDepthExceeded { .. }));
    }

    #[test]
    fn compile_accepts_self_referential_recursive_schema() {
        // 真正常见的自引用形状（链表/树）：目标节点带其他结构，不是纯引用链，必须能编译。
        let schema = json!({
            "$ref": "#/$defs/node",
            "$defs": {
                "node": {
                    "type": "object",
                    "properties": { "next": { "$ref": "#/$defs/node" } }
                }
            }
        });
        assert!(CompiledSchema::compile(schema).is_ok());
    }

    #[test]
    fn pattern_compiles_and_validates() {
        let compiled =
            CompiledSchema::compile(json!({ "type": "string", "pattern": "^[a-z]+$" })).unwrap();
        assert!(compiled.validate(&json!("abc")).is_ok());
        assert!(compiled.validate(&json!("ABC")).is_err());
        assert!(compiled.unsupported_keywords().is_empty());
    }

    #[test]
    fn pattern_with_lookahead_fails_compile_instead_of_silently_passing() {
        // ECMA-262 的负向先行断言，`regex` crate 不支持——fail-closed：拒绝整个 compile()，
        // 而不是把这条 pattern 悄悄降级成"不校验"（那会让 "admin" 静默通过）。
        let err = CompiledSchema::compile(json!({
            "type": "string",
            "pattern": "^(?!admin$).+"
        }))
        .unwrap_err();
        assert!(matches!(
            err,
            SchemaError::InvalidKeyword {
                keyword: "pattern",
                ..
            }
        ));
    }

    #[test]
    fn unsupported_keywords_reports_unevaluated_properties() {
        let compiled = CompiledSchema::compile(json!({ "unevaluatedProperties": false })).unwrap();
        assert_eq!(compiled.unsupported_keywords(), ["unevaluatedProperties"]);
    }

    #[test]
    fn compile_rejects_unknown_type_name() {
        let err = CompiledSchema::compile(json!({ "type": "banana" })).unwrap_err();
        assert!(matches!(
            err,
            SchemaError::InvalidKeyword {
                keyword: "type",
                ..
            }
        ));
    }

    #[test]
    fn compile_rejects_enum_wrong_shape() {
        assert!(matches!(
            CompiledSchema::compile(json!({ "enum": "x" })).unwrap_err(),
            SchemaError::InvalidKeyword {
                keyword: "enum",
                ..
            }
        ));
        assert!(matches!(
            CompiledSchema::compile(json!({ "enum": [] })).unwrap_err(),
            SchemaError::InvalidKeyword {
                keyword: "enum",
                ..
            }
        ));
    }

    #[test]
    fn compile_rejects_required_wrong_shape() {
        let err = CompiledSchema::compile(json!({ "required": "x" })).unwrap_err();
        assert!(matches!(
            err,
            SchemaError::InvalidKeyword {
                keyword: "required",
                ..
            }
        ));
        // 空数组合法：没有必需属性。
        assert!(CompiledSchema::compile(json!({ "required": [] })).is_ok());
    }

    #[test]
    fn compile_rejects_combinator_wrong_shape() {
        for key in ["allOf", "anyOf", "oneOf"] {
            let empty = CompiledSchema::compile(json!({ key: [] })).unwrap_err();
            assert!(matches!(empty, SchemaError::InvalidKeyword { keyword, .. } if keyword == key));
            let not_array = CompiledSchema::compile(json!({ key: "x" })).unwrap_err();
            assert!(
                matches!(not_array, SchemaError::InvalidKeyword { keyword, .. } if keyword == key)
            );
        }
    }

    #[test]
    fn compile_rejects_not_if_then_else_non_schema_element() {
        for key in ["not", "if", "then", "else"] {
            let err = CompiledSchema::compile(json!({ key: 5 })).unwrap_err();
            assert!(matches!(err, SchemaError::NotAnObjectOrBool { .. }));
        }
    }

    #[test]
    fn compile_rejects_dependent_required_wrong_shape() {
        let not_object = CompiledSchema::compile(json!({ "dependentRequired": "x" })).unwrap_err();
        assert!(matches!(
            not_object,
            SchemaError::InvalidKeyword {
                keyword: "dependentRequired",
                ..
            }
        ));

        let bad_values =
            CompiledSchema::compile(json!({ "dependentRequired": { "a": "b" } })).unwrap_err();
        assert!(matches!(
            bad_values,
            SchemaError::InvalidKeyword {
                keyword: "dependentRequired",
                ..
            }
        ));
    }

    #[test]
    fn compile_rejects_dependent_schemas_properties_pattern_properties_wrong_shape() {
        for key in [
            "dependentSchemas",
            "properties",
            "patternProperties",
            "$defs",
            "definitions",
        ] {
            let err = CompiledSchema::compile(json!({ key: "x" })).unwrap_err();
            assert!(matches!(err, SchemaError::InvalidKeyword { keyword, .. } if keyword == key));
        }
    }

    #[test]
    fn compile_rejects_additional_properties_property_names_items_contains_non_schema() {
        for key in ["additionalProperties", "propertyNames", "items", "contains"] {
            let err = CompiledSchema::compile(json!({ key: 5 })).unwrap_err();
            assert!(matches!(err, SchemaError::NotAnObjectOrBool { .. }));
        }
    }

    #[test]
    fn compile_rejects_prefix_items_wrong_shape() {
        let err = CompiledSchema::compile(json!({ "prefixItems": "x" })).unwrap_err();
        assert!(matches!(
            err,
            SchemaError::InvalidKeyword {
                keyword: "prefixItems",
                ..
            }
        ));
    }

    #[test]
    fn compile_rejects_non_negative_integer_keywords_wrong_shape() {
        for key in [
            "minLength",
            "maxLength",
            "minItems",
            "maxItems",
            "minProperties",
            "maxProperties",
            "minContains",
            "maxContains",
        ] {
            let negative = CompiledSchema::compile(json!({ key: -1 })).unwrap_err();
            assert!(
                matches!(negative, SchemaError::InvalidKeyword { keyword, .. } if keyword == key)
            );
            let fractional = CompiledSchema::compile(json!({ key: 1.5 })).unwrap_err();
            assert!(
                matches!(fractional, SchemaError::InvalidKeyword { keyword, .. } if keyword == key)
            );
        }
        // 小数部分为零的浮点字面量放行，与运行期 integer 的容忍度一致。
        assert!(CompiledSchema::compile(json!({ "minLength": 2.0 })).is_ok());
    }

    #[test]
    fn compile_rejects_numeric_bound_keywords_wrong_shape() {
        for key in ["minimum", "maximum", "exclusiveMinimum", "exclusiveMaximum"] {
            let err = CompiledSchema::compile(json!({ key: "x" })).unwrap_err();
            assert!(matches!(err, SchemaError::InvalidKeyword { keyword, .. } if keyword == key));
        }
    }

    #[test]
    fn compile_rejects_multiple_of_not_positive() {
        let zero = CompiledSchema::compile(json!({ "multipleOf": 0 })).unwrap_err();
        assert!(matches!(
            zero,
            SchemaError::InvalidKeyword {
                keyword: "multipleOf",
                ..
            }
        ));
        let negative = CompiledSchema::compile(json!({ "multipleOf": -2 })).unwrap_err();
        assert!(matches!(
            negative,
            SchemaError::InvalidKeyword {
                keyword: "multipleOf",
                ..
            }
        ));
        assert!(CompiledSchema::compile(json!({ "multipleOf": 2 })).is_ok());
    }

    #[test]
    fn compile_rejects_unique_items_wrong_shape() {
        let err = CompiledSchema::compile(json!({ "uniqueItems": "x" })).unwrap_err();
        assert!(matches!(
            err,
            SchemaError::InvalidKeyword {
                keyword: "uniqueItems",
                ..
            }
        ));
    }

    #[test]
    fn compile_ignores_unknown_keyword() {
        // 未知 keyword（不在本引擎实现的名单里）一律忽略，不应该报错。
        assert!(
            CompiledSchema::compile(
                json!({ "type": "string", "$comment": "随便写点什么", "format": "email" })
            )
            .is_ok()
        );
    }

    #[test]
    fn cache_deduplicates_by_content_not_object_identity() {
        let cache = SchemaCache::new();
        let a = json!({ "type": "string" });
        // 字段顺序不同、内容相同的第二份 schema：必须命中同一个哈希桶。
        let b: Value = serde_json::from_str(r#"{"type":"string"}"#).unwrap();

        let compiled_a = cache.get_or_compile(&a).unwrap();
        let compiled_b = cache.get_or_compile(&b).unwrap();
        assert!(
            Arc::ptr_eq(&compiled_a, &compiled_b),
            "内容相同应该复用同一份编译结果"
        );
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn cache_recompiles_when_content_differs() {
        let cache = SchemaCache::new();
        let a = json!({ "type": "string" });
        let b = json!({ "type": "number" });
        cache.get_or_compile(&a).unwrap();
        cache.get_or_compile(&b).unwrap();
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn cache_clear_empties_entries() {
        let cache = SchemaCache::new();
        cache.get_or_compile(&json!({ "type": "string" })).unwrap();
        assert!(!cache.is_empty());
        cache.clear();
        assert!(cache.is_empty());
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn content_hash_ignores_object_key_insertion_order() {
        let a: Value = serde_json::from_str(r#"{"a":1,"b":2}"#).unwrap();
        let b: Value = serde_json::from_str(r#"{"b":2,"a":1}"#).unwrap();
        assert_eq!(content_hash(&a), content_hash(&b));
    }
}
