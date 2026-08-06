//! 单趟递归校验器：draft 2020-12 子集，容忍 LLM/OpenAPI 风格的非标写法。
//!
//! 内部实现，不对外导出——唯一入口是 [`validate_root`]，由
//! [`crate::compile::CompiledSchema`] 调用。校验期间只读 schema（`patterns` 里已经是
//! 编译好的 [`Regex`]），因此可以安全地重复调用同一份编译结果。
//!
//! # 容忍性
//!
//! - `nullable: true` 等价于把 `null` 并入 `type` 的允许集合（LLM/OpenAPI 风格常见写法）。
//! - `type: "integer"` 接受小数部分为零的浮点数（`2.0` 算 integer）。
//! - 未知 keyword 一律忽略，不产出 issue。
//! - `unevaluatedProperties`/`unevaluatedItems` 不实现，按"不存在"处理（宽松通过）；
//!   编译期已经把它们记进 [`crate::compile::CompiledSchema::unsupported_keywords`]，
//!   调用方能观测到这层降级。
//! - `pattern` 一律 fail-closed：`compile()` 遇到 `regex` crate 编译不了的 pattern
//!   （ECMA-262 的 look-around、反向引用等）直接拒绝整份 schema（见 [`crate::compile`]），
//!   因此这里能拿到的 `patterns` 里每一条都保证是可用的 [`Regex`]。

use std::ptr;

use regex::Regex;
use serde_json::{Map, Value};
use std::collections::HashMap;

use crate::error::{JsonType, PathSegment, ValidationIssue};

/// `$ref` 解析深度的保险丝。
///
/// 两处使用：运行期 `($ref, 实例节点身份)` 配对去环失效时的兜底计数
/// （标量/重复节点极端情况下的防御，正常情况下配对去环已经足够），以及编译期探测
/// "纯引用"链（`{"$ref": ...}` 首尾相连、不带任何其他结构）退化成死循环（见
/// [`crate::compile`]）。64 不是性能参数，只是"任何合理的 `$defs` 嵌套深度都到不了这里"
/// 的一个宽裕上限。
pub(crate) const MAX_REF_DEPTH: usize = 64;

/// 不随递归下降而变化的只读上下文。
struct Ctx<'a> {
    /// 整份 schema；`$ref` 一律相对它解析。
    root: &'a Value,
    /// 编译期预编译好的 `pattern` 正则；`compile()` 已保证这里出现的每个 pattern 都能
    /// 成功编译（编译不了会让整个 `compile()` 失败，而不是把坏 pattern 悄悄放进这张表）。
    patterns: &'a HashMap<Box<str>, Regex>,
}

/// 随递归下降/回升而变化的可变状态。
struct Frame {
    /// 当前节点相对实例根的路径。
    path: Vec<PathSegment>,
    /// 当前调用链上正在解析的 `($ref, 实例节点指针)` 配对，用于环检测。
    ///
    /// 用指针身份而非字符串/深度：同一个 `$ref` 从两个不相交的分支（如 `allOf` 的两个
    /// 成员）分别应用到同一个实例节点是合法的 DAG 共享，不是环——区分两者的关键就是
    /// "当前调用栈上是否已经在解析这个配对"，而不是"历史上是否解析过"。递归返回时必须
    /// 弹出，否则第二个分支会被误判成环而被跳过（真实缺陷会被静默吞掉）。
    ref_stack: Vec<(Box<str>, *const Value)>,
    /// 当前调用链上跟随 `$ref` 的层数，`ref_stack` 兜底失效时的最后一道防线。
    ref_depth: usize,
}

/// 从 schema 根开始校验一个实例，把所有失败点追加到 `out`。
pub(crate) fn validate_root(
    root: &Value,
    patterns: &HashMap<Box<str>, Regex>,
    instance: &Value,
    out: &mut Vec<ValidationIssue>,
) {
    let ctx = Ctx { root, patterns };
    let mut frame = Frame {
        path: Vec::new(),
        ref_stack: Vec::new(),
        ref_depth: 0,
    };
    validate_node(root, instance, &ctx, &mut frame, out);
}

/// 校验 `instance` 是否满足 `schema`（布尔 schema 或对象 schema），把失败点追加到 `out`。
fn validate_node(
    schema: &Value,
    instance: &Value,
    ctx: &Ctx<'_>,
    frame: &mut Frame,
    out: &mut Vec<ValidationIssue>,
) {
    match schema {
        Value::Bool(false) => out.push(ValidationIssue {
            path: frame.path.clone(),
            message: "该位置的 schema 恒为 false，任何取值都不满足".to_string(),
            keyword: "schema",
            expected_types: Vec::new(),
            from_union_branch: false,
        }),
        Value::Object(obj) => validate_object_schema(obj, instance, ctx, frame, out),
        // compile() 已保证 schema 树上每个节点都是对象或布尔值；到这里说明调用方传入了
        // 未经 compile() 校验的裸 Value，宽松地当作"无约束"处理，不产出虚假 issue。
        _ => {}
    }
}

/// 探测 `sub` 是否满足（不产出任何 issue，只看结果）；用于 `anyOf`/`oneOf`/`not`/`if` 的分支试探。
///
/// 试探必须共享外层的 `ref_stack`/`ref_depth`——同一个实例节点上的环检测在分支之间也要生效，
/// 环检测状态在 `validate_node` 内部随递归自行入栈/出栈，不需要这里额外处理。
fn branch_passes(sub: &Value, instance: &Value, ctx: &Ctx<'_>, frame: &mut Frame) -> bool {
    let mut scratch = Vec::new();
    validate_node(sub, instance, ctx, frame, &mut scratch);
    scratch.is_empty()
}

fn validate_object_schema(
    obj: &Map<String, Value>,
    instance: &Value,
    ctx: &Ctx<'_>,
    frame: &mut Frame,
    out: &mut Vec<ValidationIssue>,
) {
    check_type(obj, instance, frame, out);
    check_enum(obj, instance, frame, out);
    check_const(obj, instance, frame, out);
    check_ref(obj, instance, ctx, frame, out);

    check_all_of(obj, instance, ctx, frame, out);
    check_any_of(obj, instance, ctx, frame, out);
    check_one_of(obj, instance, ctx, frame, out);
    check_not(obj, instance, ctx, frame, out);
    check_if_then_else(obj, instance, ctx, frame, out);

    if instance.is_object() {
        check_object_keywords(obj, instance, ctx, frame, out);
    }
    if instance.is_array() {
        check_array_keywords(obj, instance, ctx, frame, out);
    }
    if let Some(s) = instance.as_str() {
        check_string_keywords(obj, s, ctx, frame, out);
    }
    if instance.is_number() {
        check_number_keywords(obj, instance, frame, out);
    }
}

// ── 通用 keyword ────────────────────────────────────────────────────────────

pub(crate) fn json_type_from_name(name: &str) -> Option<JsonType> {
    Some(match name {
        "null" => JsonType::Null,
        "boolean" => JsonType::Boolean,
        "object" => JsonType::Object,
        "array" => JsonType::Array,
        "number" => JsonType::Number,
        "integer" => JsonType::Integer,
        "string" => JsonType::String,
        _ => return None,
    })
}

fn parse_type_list(ty: &Value) -> Vec<JsonType> {
    if let Some(name) = ty.as_str() {
        return json_type_from_name(name).into_iter().collect();
    }
    ty.as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .filter_map(json_type_from_name)
                .collect()
        })
        .unwrap_or_default()
}

fn type_matches(expected: JsonType, instance: &Value) -> bool {
    match expected {
        // 小数部分为零的浮点数也算 integer——LLM 生成的 JSON 经常把整数写成 `2.0`。
        JsonType::Integer => instance
            .as_f64()
            .is_some_and(|n| n.is_finite() && n.fract() == 0.0),
        JsonType::Number => instance.is_number(),
        JsonType::Null => instance.is_null(),
        JsonType::Boolean => instance.is_boolean(),
        JsonType::Object => instance.is_object(),
        JsonType::Array => instance.is_array(),
        JsonType::String => instance.is_string(),
    }
}

fn actual_type_name(instance: &Value) -> &'static str {
    match instance {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn check_type(
    obj: &Map<String, Value>,
    instance: &Value,
    frame: &Frame,
    out: &mut Vec<ValidationIssue>,
) {
    let Some(ty) = obj.get("type") else { return };
    let mut expected = parse_type_list(ty);
    if expected.is_empty() {
        return;
    }
    // `nullable: true`（OpenAPI/LLM 风格）等价于把 null 并入允许集合。
    if obj.get("nullable").and_then(Value::as_bool) == Some(true)
        && !expected.contains(&JsonType::Null)
    {
        expected.push(JsonType::Null);
    }
    if expected.iter().any(|t| type_matches(*t, instance)) {
        return;
    }
    let expected_names = expected
        .iter()
        .copied()
        .map(JsonType::as_str)
        .collect::<Vec<_>>()
        .join(" | ");
    out.push(ValidationIssue {
        path: frame.path.clone(),
        message: format!(
            "期望类型为 {expected_names}，实际是 {}",
            actual_type_name(instance)
        ),
        keyword: "type",
        expected_types: expected,
        from_union_branch: false,
    });
}

fn check_enum(
    obj: &Map<String, Value>,
    instance: &Value,
    frame: &Frame,
    out: &mut Vec<ValidationIssue>,
) {
    let Some(variants) = obj.get("enum").and_then(Value::as_array) else {
        return;
    };
    if variants.iter().any(|v| v == instance) {
        return;
    }
    out.push(ValidationIssue {
        path: frame.path.clone(),
        message: "取值不在 enum 允许的集合内".to_string(),
        keyword: "enum",
        expected_types: Vec::new(),
        from_union_branch: false,
    });
}

fn check_const(
    obj: &Map<String, Value>,
    instance: &Value,
    frame: &Frame,
    out: &mut Vec<ValidationIssue>,
) {
    let Some(expected) = obj.get("const") else {
        return;
    };
    if expected == instance {
        return;
    }
    out.push(ValidationIssue {
        path: frame.path.clone(),
        message: "取值与 const 指定的常量不相等".to_string(),
        keyword: "const",
        expected_types: Vec::new(),
        from_union_branch: false,
    });
}

fn check_ref(
    obj: &Map<String, Value>,
    instance: &Value,
    ctx: &Ctx<'_>,
    frame: &mut Frame,
    out: &mut Vec<ValidationIssue>,
) {
    let Some(reference) = obj.get("$ref").and_then(Value::as_str) else {
        return;
    };
    if frame.ref_depth >= MAX_REF_DEPTH {
        // 深度保险丝：正常的配对去环会在第二次遇到同一个 (ref, 实例节点) 时截断，
        // 这里只是兜底，理论上不应该被触发。
        return;
    }
    // compile() 已经保证每个 `$ref` 都以 `#` 开头且能解析；这里仍然防御性地跳过而不是
    // panic，因为 validate_node 也可能被直接喂进未经 compile() 的裸 Value（如 `not`/`if`
    // 的子 schema 在测试中手写）。
    let Some(fragment) = reference.strip_prefix('#') else {
        return;
    };
    let Some(target) = ctx.root.pointer(fragment) else {
        return;
    };

    let identity = ptr::from_ref(instance);
    if frame
        .ref_stack
        .iter()
        .any(|(r, p)| r.as_ref() == reference && *p == identity)
    {
        // 环：当前调用栈上已经在用同一个 $ref 解析同一个实例节点，再展开只会无限递归。
        // 静默截断（不产出 issue）——这是一处结构性死循环，不是实例本身的错误。
        return;
    }

    frame.ref_stack.push((Box::from(reference), identity));
    frame.ref_depth += 1;
    validate_node(target, instance, ctx, frame, out);
    frame.ref_depth -= 1;
    frame.ref_stack.pop();
}

// ── 组合子 ──────────────────────────────────────────────────────────────────

fn check_all_of(
    obj: &Map<String, Value>,
    instance: &Value,
    ctx: &Ctx<'_>,
    frame: &mut Frame,
    out: &mut Vec<ValidationIssue>,
) {
    let Some(arr) = obj.get("allOf").and_then(Value::as_array) else {
        return;
    };
    for sub in arr {
        validate_node(sub, instance, ctx, frame, out);
    }
}

fn check_any_of(
    obj: &Map<String, Value>,
    instance: &Value,
    ctx: &Ctx<'_>,
    frame: &mut Frame,
    out: &mut Vec<ValidationIssue>,
) {
    let Some(arr) = obj.get("anyOf").and_then(Value::as_array) else {
        return;
    };
    if arr
        .iter()
        .any(|sub| branch_passes(sub, instance, ctx, frame))
    {
        return;
    }
    out.push(ValidationIssue {
        path: frame.path.clone(),
        message: format!("不满足 anyOf 中 {} 个分支的任意一个", arr.len()),
        keyword: "anyOf",
        expected_types: Vec::new(),
        from_union_branch: true,
    });
}

fn check_one_of(
    obj: &Map<String, Value>,
    instance: &Value,
    ctx: &Ctx<'_>,
    frame: &mut Frame,
    out: &mut Vec<ValidationIssue>,
) {
    let Some(arr) = obj.get("oneOf").and_then(Value::as_array) else {
        return;
    };
    let passed = arr
        .iter()
        .filter(|sub| branch_passes(sub, instance, ctx, frame))
        .count();
    if passed == 1 {
        return;
    }
    let message = if passed == 0 {
        format!("不满足 oneOf 中 {} 个分支的任何一个", arr.len())
    } else {
        format!("同时满足 oneOf 中 {passed} 个分支（应恰好一个）")
    };
    out.push(ValidationIssue {
        path: frame.path.clone(),
        message,
        keyword: "oneOf",
        expected_types: Vec::new(),
        from_union_branch: true,
    });
}

fn check_not(
    obj: &Map<String, Value>,
    instance: &Value,
    ctx: &Ctx<'_>,
    frame: &mut Frame,
    out: &mut Vec<ValidationIssue>,
) {
    let Some(sub) = obj.get("not") else { return };
    if branch_passes(sub, instance, ctx, frame) {
        out.push(ValidationIssue {
            path: frame.path.clone(),
            message: "取值不应满足 not 指定的 schema".to_string(),
            keyword: "not",
            expected_types: Vec::new(),
            from_union_branch: false,
        });
    }
}

fn check_if_then_else(
    obj: &Map<String, Value>,
    instance: &Value,
    ctx: &Ctx<'_>,
    frame: &mut Frame,
    out: &mut Vec<ValidationIssue>,
) {
    let Some(if_schema) = obj.get("if") else {
        return;
    };
    if branch_passes(if_schema, instance, ctx, frame) {
        if let Some(then_schema) = obj.get("then") {
            validate_node(then_schema, instance, ctx, frame, out);
        }
    } else if let Some(else_schema) = obj.get("else") {
        validate_node(else_schema, instance, ctx, frame, out);
    }
}

// ── 对象 keyword ────────────────────────────────────────────────────────────

fn pattern_property_matches(ctx: &Ctx<'_>, pattern: &str, text: &str) -> bool {
    // `compile()` 保证 schema 里出现的每个 pattern 都在这张表里；`None` 只会出现在
    // validate_root 被绕过 compile() 直接调用的场景（如本模块自己的单元测试），
    // 防御性地当作"不匹配"处理，不 panic。
    ctx.patterns
        .get(pattern)
        .is_some_and(|re| re.is_match(text))
}

#[allow(clippy::too_many_lines)] // 对象 keyword 多且彼此独立，拆成更小的函数只会把状态搬来搬去
fn check_object_keywords(
    obj: &Map<String, Value>,
    instance: &Value,
    ctx: &Ctx<'_>,
    frame: &mut Frame,
    out: &mut Vec<ValidationIssue>,
) {
    let Some(inst_obj) = instance.as_object() else {
        return;
    };

    let properties = obj.get("properties").and_then(Value::as_object);
    let pattern_properties = obj.get("patternProperties").and_then(Value::as_object);

    if let Some(props) = properties {
        for (name, sub) in props {
            if let Some(val) = inst_obj.get(name) {
                frame.path.push(PathSegment::Key(name.as_str().into()));
                validate_node(sub, val, ctx, frame, out);
                frame.path.pop();
            }
        }
    }

    if let Some(pp) = pattern_properties {
        for (name, val) in inst_obj {
            for (pattern, sub) in pp {
                if pattern_property_matches(ctx, pattern, name) {
                    frame.path.push(PathSegment::Key(name.as_str().into()));
                    validate_node(sub, val, ctx, frame, out);
                    frame.path.pop();
                }
            }
        }
    }

    if let Some(additional) = obj.get("additionalProperties") {
        for (name, val) in inst_obj {
            let covered = properties.is_some_and(|p| p.contains_key(name))
                || pattern_properties.is_some_and(|pp| {
                    pp.keys()
                        .any(|pat| pattern_property_matches(ctx, pat, name))
                });
            if covered {
                continue;
            }
            match additional {
                Value::Bool(false) => {
                    let mut path = frame.path.clone();
                    path.push(PathSegment::Key(name.as_str().into()));
                    out.push(ValidationIssue {
                        path,
                        message: format!("不允许额外属性 \"{name}\""),
                        keyword: "additionalProperties",
                        expected_types: Vec::new(),
                        from_union_branch: false,
                    });
                }
                Value::Bool(true) => {}
                sub => {
                    frame.path.push(PathSegment::Key(name.as_str().into()));
                    validate_node(sub, val, ctx, frame, out);
                    frame.path.pop();
                }
            }
        }
    }

    if let Some(required) = obj.get("required").and_then(Value::as_array) {
        for name in required.iter().filter_map(Value::as_str) {
            if !inst_obj.contains_key(name) {
                out.push(ValidationIssue {
                    path: frame.path.clone(),
                    message: format!("缺少必需属性 \"{name}\""),
                    keyword: "required",
                    expected_types: Vec::new(),
                    from_union_branch: false,
                });
            }
        }
    }

    check_min_max(
        obj,
        "minProperties",
        "maxProperties",
        inst_obj.len(),
        frame,
        out,
    );

    if let Some(names_schema) = obj.get("propertyNames") {
        for name in inst_obj.keys() {
            frame.path.push(PathSegment::Key(name.as_str().into()));
            validate_node(names_schema, &Value::String(name.clone()), ctx, frame, out);
            frame.path.pop();
        }
    }

    if let Some(dep_required) = obj.get("dependentRequired").and_then(Value::as_object) {
        for (trigger, siblings) in dep_required {
            if !inst_obj.contains_key(trigger) {
                continue;
            }
            let Some(siblings) = siblings.as_array() else {
                continue;
            };
            for sibling in siblings.iter().filter_map(Value::as_str) {
                if !inst_obj.contains_key(sibling) {
                    out.push(ValidationIssue {
                        path: frame.path.clone(),
                        message: format!("属性 \"{trigger}\" 存在时必须同时提供 \"{sibling}\""),
                        keyword: "dependentRequired",
                        expected_types: Vec::new(),
                        from_union_branch: false,
                    });
                }
            }
        }
    }

    if let Some(dep_schemas) = obj.get("dependentSchemas").and_then(Value::as_object) {
        for (trigger, sub) in dep_schemas {
            if inst_obj.contains_key(trigger) {
                validate_node(sub, instance, ctx, frame, out);
            }
        }
    }
}

fn check_min_max(
    obj: &Map<String, Value>,
    min_key: &'static str,
    max_key: &'static str,
    actual: usize,
    frame: &Frame,
    out: &mut Vec<ValidationIssue>,
) {
    if let Some(min) = obj
        .get(min_key)
        .and_then(Value::as_u64)
        .and_then(|n| usize::try_from(n).ok())
        && actual < min
    {
        out.push(ValidationIssue {
            path: frame.path.clone(),
            message: format!("{min_key} 要求至少 {min} 个，实际 {actual} 个"),
            keyword: min_key,
            expected_types: Vec::new(),
            from_union_branch: false,
        });
    }
    if let Some(max) = obj
        .get(max_key)
        .and_then(Value::as_u64)
        .and_then(|n| usize::try_from(n).ok())
        && actual > max
    {
        out.push(ValidationIssue {
            path: frame.path.clone(),
            message: format!("{max_key} 要求至多 {max} 个，实际 {actual} 个"),
            keyword: max_key,
            expected_types: Vec::new(),
            from_union_branch: false,
        });
    }
}

// ── 数组 keyword ────────────────────────────────────────────────────────────

fn check_array_keywords(
    obj: &Map<String, Value>,
    instance: &Value,
    ctx: &Ctx<'_>,
    frame: &mut Frame,
    out: &mut Vec<ValidationIssue>,
) {
    let Some(items_arr) = instance.as_array() else {
        return;
    };

    let prefix_items = obj.get("prefixItems").and_then(Value::as_array);
    let prefix_len = prefix_items.map_or(0, Vec::len);

    if let Some(prefix) = prefix_items {
        for (index, sub) in prefix.iter().enumerate() {
            if let Some(val) = items_arr.get(index) {
                frame.path.push(PathSegment::Index(index));
                validate_node(sub, val, ctx, frame, out);
                frame.path.pop();
            }
        }
    }

    // 2020-12 语义：`items` 只应用到 `prefixItems` 没有覆盖到的下标（没有 `prefixItems`
    // 时就是全部下标）。
    if let Some(items_schema) = obj.get("items") {
        for (index, val) in items_arr.iter().enumerate().skip(prefix_len) {
            frame.path.push(PathSegment::Index(index));
            validate_node(items_schema, val, ctx, frame, out);
            frame.path.pop();
        }
    }

    check_min_max(obj, "minItems", "maxItems", items_arr.len(), frame, out);

    if obj.get("uniqueItems").and_then(Value::as_bool) == Some(true) {
        let has_duplicate = items_arr
            .iter()
            .enumerate()
            .any(|(i, a)| items_arr.iter().skip(i + 1).any(|b| a == b));
        if has_duplicate {
            out.push(ValidationIssue {
                path: frame.path.clone(),
                message: "数组元素必须互不相同".to_string(),
                keyword: "uniqueItems",
                expected_types: Vec::new(),
                from_union_branch: false,
            });
        }
    }

    if let Some(contains_schema) = obj.get("contains") {
        let matched = items_arr
            .iter()
            .filter(|v| branch_passes(contains_schema, v, ctx, frame))
            .count();
        let min_contains = obj
            .get("minContains")
            .and_then(Value::as_u64)
            .and_then(|n| usize::try_from(n).ok())
            .unwrap_or(1);
        let max_contains = obj
            .get("maxContains")
            .and_then(Value::as_u64)
            .and_then(|n| usize::try_from(n).ok());
        let violates = matched < min_contains || max_contains.is_some_and(|max| matched > max);
        if violates {
            let max_desc = max_contains.map_or_else(|| "不限".to_string(), |m| m.to_string());
            out.push(ValidationIssue {
                path: frame.path.clone(),
                message: format!(
                    "contains 要求匹配 {min_contains} 到 {max_desc} 次，实际匹配 {matched} 次"
                ),
                keyword: "contains",
                expected_types: Vec::new(),
                from_union_branch: false,
            });
        }
    }
}

// ── 字符串 keyword ──────────────────────────────────────────────────────────

fn check_string_keywords(
    obj: &Map<String, Value>,
    s: &str,
    ctx: &Ctx<'_>,
    frame: &Frame,
    out: &mut Vec<ValidationIssue>,
) {
    // 按 Unicode 码点数计长，不是字节数——CJK/emoji 每个字符占多个字节但只算一个码点。
    let char_len = s.chars().count();

    if let Some(min) = obj
        .get("minLength")
        .and_then(Value::as_u64)
        .and_then(|n| usize::try_from(n).ok())
        && char_len < min
    {
        out.push(ValidationIssue {
            path: frame.path.clone(),
            message: format!("字符串长度至少 {min} 个码点，实际 {char_len} 个"),
            keyword: "minLength",
            expected_types: Vec::new(),
            from_union_branch: false,
        });
    }
    if let Some(max) = obj
        .get("maxLength")
        .and_then(Value::as_u64)
        .and_then(|n| usize::try_from(n).ok())
        && char_len > max
    {
        out.push(ValidationIssue {
            path: frame.path.clone(),
            message: format!("字符串长度至多 {max} 个码点，实际 {char_len} 个"),
            keyword: "maxLength",
            expected_types: Vec::new(),
            from_union_branch: false,
        });
    }
    if let Some(pattern) = obj.get("pattern").and_then(Value::as_str) {
        // 未收录只会出现在绕过 compile() 直接调用 validate_root 的场景（见 pattern_property_matches
        // 的注释）；防御性跳过而不是 panic。
        if let Some(re) = ctx.patterns.get(pattern)
            && !re.is_match(s)
        {
            out.push(ValidationIssue {
                path: frame.path.clone(),
                message: format!("字符串不匹配 pattern `{pattern}`"),
                keyword: "pattern",
                expected_types: Vec::new(),
                from_union_branch: false,
            });
        }
    }
}

// ── 数值 keyword ────────────────────────────────────────────────────────────

fn push_number_issue(
    frame: &Frame,
    out: &mut Vec<ValidationIssue>,
    keyword: &'static str,
    message: String,
) {
    out.push(ValidationIssue {
        path: frame.path.clone(),
        message,
        keyword,
        expected_types: Vec::new(),
        from_union_branch: false,
    });
}

fn check_number_keywords(
    obj: &Map<String, Value>,
    instance: &Value,
    frame: &Frame,
    out: &mut Vec<ValidationIssue>,
) {
    let Some(n) = instance.as_f64() else { return };

    if let Some(min) = obj.get("minimum").and_then(Value::as_f64)
        && n < min
    {
        push_number_issue(
            frame,
            out,
            "minimum",
            format!("取值必须 ≥ {min}，实际是 {n}"),
        );
    }
    if let Some(max) = obj.get("maximum").and_then(Value::as_f64)
        && n > max
    {
        push_number_issue(
            frame,
            out,
            "maximum",
            format!("取值必须 ≤ {max}，实际是 {n}"),
        );
    }
    // 2020-12 语义：exclusiveMinimum/exclusiveMaximum 是数字，与 minimum/maximum 相互独立
    // （不像 draft-04 那样是配对 minimum 用的布尔开关）。
    if let Some(min) = obj.get("exclusiveMinimum").and_then(Value::as_f64)
        && n <= min
    {
        push_number_issue(
            frame,
            out,
            "exclusiveMinimum",
            format!("取值必须 > {min}，实际是 {n}"),
        );
    }
    if let Some(max) = obj.get("exclusiveMaximum").and_then(Value::as_f64)
        && n >= max
    {
        push_number_issue(
            frame,
            out,
            "exclusiveMaximum",
            format!("取值必须 < {max}，实际是 {n}"),
        );
    }
    if let Some(multiple) = obj.get("multipleOf").and_then(Value::as_f64)
        && multiple > 0.0
    {
        let quotient = n / multiple;
        // 浮点除法允许极小误差；1e-9 远细于任何业务场景会关心的精度。
        if (quotient - quotient.round()).abs() > 1e-9 {
            push_number_issue(
                frame,
                out,
                "multipleOf",
                format!("取值必须是 {multiple} 的整数倍，实际是 {n}"),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn issues_for(schema: &Value, instance: &Value) -> Vec<ValidationIssue> {
        let patterns = HashMap::new();
        let mut out = Vec::new();
        validate_root(schema, &patterns, instance, &mut out);
        out
    }

    #[test]
    fn type_check_accepts_matching_and_rejects_mismatched() {
        let schema = json!({ "type": "string" });
        assert!(issues_for(&schema, &json!("hi")).is_empty());
        let issues = issues_for(&schema, &json!(1));
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].keyword, "type");
        assert_eq!(issues[0].expected_types, vec![JsonType::String]);
    }

    #[test]
    fn nullable_true_unions_null_into_type() {
        let schema = json!({ "type": "string", "nullable": true });
        assert!(issues_for(&schema, &Value::Null).is_empty());
        assert!(issues_for(&schema, &json!("ok")).is_empty());
        assert_eq!(issues_for(&schema, &json!(1)).len(), 1);
    }

    #[test]
    fn integer_type_accepts_zero_fraction_float() {
        let schema = json!({ "type": "integer" });
        assert!(issues_for(&schema, &json!(2.0)).is_empty());
        assert_eq!(issues_for(&schema, &json!(2.5)).len(), 1);
    }

    #[test]
    fn string_length_counts_unicode_codepoints_not_bytes() {
        // "你好" 是 2 个码点、6 个字节；一个 emoji 常是 1 个码点、4 个字节。
        let schema = json!({ "minLength": 2, "maxLength": 2 });
        assert!(issues_for(&schema, &json!("你好")).is_empty());
        assert!(issues_for(&schema, &json!("🎉🎉")).is_empty());
        assert_eq!(issues_for(&schema, &json!("你好吗")).len(), 1);
    }

    #[test]
    fn one_of_exactly_one_branch_passes() {
        let schema = json!({ "oneOf": [{ "type": "string" }, { "type": "number" }] });
        assert!(issues_for(&schema, &json!("hi")).is_empty());
        assert!(issues_for(&schema, &json!(1)).is_empty());
    }

    #[test]
    fn one_of_reports_zero_matches() {
        // 两个分支都显式约束了 type，对布尔实例都不适用——真正的"零匹配"。
        let schema = json!({ "oneOf": [{ "type": "string" }, { "type": "number" }] });
        let issues = issues_for(&schema, &json!(true));
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].keyword, "oneOf");
        assert!(issues[0].from_union_branch);
    }

    #[test]
    fn one_of_reports_multiple_matches() {
        // 第二个分支没有 `type` 约束，对字符串实例而言 minLength 之外没有别的限制，
        // 因此两个分支都会满足——真正的"多重匹配"。
        let schema = json!({ "oneOf": [{ "type": "string" }, { "minLength": 0 }] });
        let issues = issues_for(&schema, &json!("hi"));
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].keyword, "oneOf");
        assert!(issues[0].from_union_branch);
    }

    #[test]
    fn from_union_branch_only_set_on_combinator_own_path() {
        let schema = json!({
            "properties": { "a": { "anyOf": [{ "type": "string" }, { "type": "number" }] } }
        });
        let issues = issues_for(&schema, &json!({ "a": true }));
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].display_path(), "a");
        assert_eq!(issues[0].keyword, "anyOf");
        assert!(issues[0].from_union_branch);
    }

    #[test]
    fn all_of_issues_never_marked_as_union_branch() {
        let schema = json!({ "allOf": [{ "properties": { "x": { "type": "string" } } }] });
        let issues = issues_for(&schema, &json!({ "x": 1 }));
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].display_path(), "x");
        assert_eq!(issues[0].keyword, "type");
        assert!(!issues[0].from_union_branch);
    }

    #[test]
    fn self_referential_ref_terminates_instead_of_looping() {
        // `{"allOf": [{"$ref": "#"}]}`：$ref 指回根节点本身，且没有任何结构下降实例。
        // 环检测必须在第二次遇到 (「#」, 同一个实例指针) 时截断，否则会无限递归。
        let schema = json!({ "allOf": [{ "$ref": "#" }] });
        let issues = issues_for(&schema, &json!({ "any": "thing" }));
        assert!(issues.is_empty());
    }

    #[test]
    fn shared_ref_across_sibling_branches_is_not_a_false_cycle() {
        // allOf 的两个成员各自引用同一个 $defs 条目、校验同一个（未变化的）实例——
        // 这是合法的 DAG 共享，不是环；两个分支必须各自独立产出自己的 issue。
        let schema = json!({
            "$defs": { "pos": { "minimum": 10 } },
            "allOf": [{ "$ref": "#/$defs/pos" }, { "$ref": "#/$defs/pos" }]
        });
        let issues = issues_for(&schema, &json!(5));
        assert_eq!(
            issues.len(),
            2,
            "两个 allOf 分支都应该各自报告违反 minimum，而不是第二个被误判为环而跳过"
        );
    }

    #[test]
    fn dag_shared_ref_valid_instance_passes_both_branches() {
        let schema = json!({
            "$defs": { "pos": { "minimum": 10 } },
            "allOf": [{ "$ref": "#/$defs/pos" }, { "$ref": "#/$defs/pos" }]
        });
        assert!(issues_for(&schema, &json!(20)).is_empty());
    }

    #[test]
    fn required_reports_each_missing_property() {
        let schema = json!({ "required": ["a", "b"] });
        let issues = issues_for(&schema, &json!({}));
        assert_eq!(issues.len(), 2);
        assert!(issues.iter().all(|i| i.keyword == "required"));
    }

    #[test]
    fn additional_properties_false_flags_extra_keys() {
        let schema = json!({ "properties": { "a": true }, "additionalProperties": false });
        assert!(issues_for(&schema, &json!({ "a": 1 })).is_empty());
        let issues = issues_for(&schema, &json!({ "a": 1, "b": 2 }));
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].display_path(), "b");
        assert_eq!(issues[0].keyword, "additionalProperties");
    }

    #[test]
    fn contains_respects_min_and_max() {
        let schema =
            json!({ "contains": { "type": "number" }, "minContains": 2, "maxContains": 3 });
        assert!(issues_for(&schema, &json!([1, 2])).is_empty());
        assert_eq!(issues_for(&schema, &json!([1])).len(), 1);
        assert_eq!(issues_for(&schema, &json!([1, 2, 3, 4])).len(), 1);
    }
}
