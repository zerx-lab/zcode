//! 从模型 id 里解析出族、版本号与变体标签。
//!
//! 只做**纯字符串**解析，不查目录、不做网络请求，也不缓存——参见 `mod.rs` 顶部关于
//! 无界 memo 的取舍说明。

use crate::identity::family::ModelFamily;

/// 剥掉 `provider/` 前缀，返回借用子串（零分配）。
///
/// 取最后一个 `/` 之后的部分：模型 id 本身也可能含 `/`（如厂商仓库风格的
/// `openrouter/anthropic/claude-opus-4-5`），只有最后一段才是真正的模型名。
/// 用 `str::rfind` 而非 `memchr::memrchr`——模型 id 通常只有几十字节，
/// SIMD 扫描在这个长度上赢不回引入依赖的代价，语义完全等价。
#[must_use]
pub fn bare_model_id(id: &str) -> &str {
    match id.rfind('/') {
        // `idx` 是 ASCII `/` 的字节下标，`idx + 1` 必然落在字符边界上，
        // `get` 仍用安全组合子而非直接索引，避免触发 indexing_slicing。
        Some(idx) => id.get(idx + 1..).unwrap_or(id),
        None => id,
    }
}

/// 剥掉方括号前缀/后缀（如 `[Kiro] claude-opus-4-5`、`gpt-4o [Free]`），返回借用子串。
///
/// 只处理位于**两端**的方括号标签，反复剥离直到两端都没有为止；剥空会丢失全部信息，
/// 因此每一步都要求剩余部分非空才生效。
#[must_use]
pub fn strip_bracket_tags(id: &str) -> &str {
    let mut s = id.trim();
    loop {
        let mut changed = false;
        if let Some(rest) = strip_leading_bracket(s) {
            s = rest;
            changed = true;
        }
        if let Some(rest) = strip_trailing_bracket(s) {
            s = rest;
            changed = true;
        }
        if !changed {
            break;
        }
    }
    s
}

/// 剥掉形如 `[tag] rest` 的前导方括号标签；剥空则不生效。
fn strip_leading_bracket(s: &str) -> Option<&str> {
    let after_open = s.strip_prefix('[')?;
    let close = after_open.find(']')?;
    // `close` 是 `]` 的字节下标，`close + 1` 落在其后的字符边界上。
    let rest = after_open.get(close + 1..)?.trim_start();
    (!rest.is_empty()).then_some(rest)
}

/// 剥掉形如 `rest [tag]` 的尾随方括号标签；剥空则不生效。
fn strip_trailing_bracket(s: &str) -> Option<&str> {
    let before_close = s.strip_suffix(']')?;
    let open = before_close.rfind('[')?;
    let rest = before_close.get(..open)?.trim_end();
    (!rest.is_empty()).then_some(rest)
}

/// 主版本.次版本，如 `claude-opus-4-5` → 4.5，`gpt-5.4` → 5.4。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SemVer {
    /// 主版本号。
    pub major: u16,
    /// 次版本号；id 里没有次版本段时记为 0（如 `claude-opus-5`）。
    pub minor: u16,
}

/// 解析出的模型身份。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedModel {
    /// 所属模型族。
    pub family: ModelFamily,
    /// 版本号；无法从 id 中提取数字版本段时为 `None`。
    pub version: Option<SemVer>,
    /// 变体标签，如 `opus`/`sonnet`/`mini`/`pro`；不存在时为 `None`。
    pub variant: Option<Box<str>>,
}

/// 按 gemini → anthropic → openai → glm 的顺序尝试；都不命中返回 `None`。
///
/// **顺序不可改**：上游把 GLM 放在最后是故意的，提前会改变歧义 id 的归属。
///
/// 输入应是已经过 [`bare_model_id`] / [`strip_bracket_tags`] 归一化的 id；
/// 内部会再统一转小写一次——目录里同一模型可能来自不同渠道，大小写不统一
/// （如厂商仓库路径 `GLM-4.6` vs 官方 `glm-4.6`），一次分配换取判定逻辑
/// 全程使用普通的 `starts_with`/`strip_prefix`，无需到处处理大小写分支。
#[must_use]
pub fn parse_known_model(id: &str) -> Option<ParsedModel> {
    let lower = id.to_ascii_lowercase();
    let id = lower.as_str();
    parse_gemini(id)
        .or_else(|| parse_anthropic(id))
        .or_else(|| parse_openai(id))
        .or_else(|| parse_glm(id))
}

/// 从字符串开头解析 `<major>` 或 `<major>.<minor>` / `<major>-<minor>` 版本号
/// （两种分隔符都认），返回版本号与解析后剩余的尾部。
///
/// 尾部可能是空串，也可能以分隔符之外的字符开头（如 `"4o"` 中解析出 major=4
/// 后剩下的 `"o"`，或 `"4-mini"` 中次版本段解析失败后剩下的 `"-mini"`）。
/// 开头不是数字时返回 `None`。
fn parse_leading_version(s: &str) -> Option<(SemVer, &str)> {
    let major_len = s.bytes().take_while(u8::is_ascii_digit).count();
    if major_len == 0 {
        return None;
    }
    let major: u16 = s.get(..major_len)?.parse().ok()?;
    let tail = s.get(major_len..)?;

    if let Some(sep_rest) = tail.strip_prefix('.').or_else(|| tail.strip_prefix('-')) {
        let minor_len = sep_rest.bytes().take_while(u8::is_ascii_digit).count();
        if minor_len > 0 {
            let minor: u16 = sep_rest.get(..minor_len)?.parse().ok()?;
            let rest = sep_rest.get(minor_len..)?;
            return Some((SemVer { major, minor }, rest));
        }
    }
    Some((SemVer { major, minor: 0 }, tail))
}

/// 剥掉尾部残留分隔符后，把非空剩余部分包成变体标签。
fn tail_variant(tail: &str) -> Option<Box<str>> {
    let trimmed = tail.strip_prefix('-').unwrap_or(tail);
    (!trimmed.is_empty()).then(|| trimmed.into())
}

/// 非空字符串转 `Box<str>`，空串记为 `None`。
fn non_empty_box(s: &str) -> Option<Box<str>> {
    (!s.is_empty()).then(|| s.into())
}

/// Gemini：`gemini-<major>.<minor>-<variant>`，例 `gemini-2.5-pro`。
fn parse_gemini(id: &str) -> Option<ParsedModel> {
    let rest = id.strip_prefix("gemini-")?;
    let (version, tail) = match parse_leading_version(rest) {
        Some((v, t)) => (Some(v), t),
        None => (None, rest),
    };
    Some(ParsedModel {
        family: ModelFamily::Gemini,
        version,
        variant: tail_variant(tail),
    })
}

/// 剥掉 Anthropic id 尾部与版本无关的噪声后缀：`-latest`、`-thinking`/`-think`、
/// 以及 8 位日期戳（如 `-20251101`）。可能叠加出现（如日期后再跟 `-thinking`），
/// 因此反复剥离直到没有变化为止。
fn strip_anthropic_suffix(s: &str) -> &str {
    let mut s = s;
    loop {
        if let Some(r) = s.strip_suffix("-latest") {
            s = r;
            continue;
        }
        if let Some(r) = s.strip_suffix("-thinking") {
            s = r;
            continue;
        }
        if let Some(r) = s.strip_suffix("-think") {
            s = r;
            continue;
        }
        if let Some(r) = strip_date_suffix(s) {
            s = r;
            continue;
        }
        break;
    }
    s
}

/// 剥掉形如 `-YYYYMMDD` 的 8 位数字日期后缀。
fn strip_date_suffix(s: &str) -> Option<&str> {
    let (head, tail) = s.rsplit_once('-')?;
    (tail.len() == 8 && tail.bytes().all(|b| b.is_ascii_digit())).then_some(head)
}

/// 从剥掉噪声后缀的 Anthropic 主体里解出版本号与变体（kind）标签。
///
/// 支持三种形状：
/// - kind-first 三段：`<kind>-<major>-<minor>`（如 `opus-4-5`）；
/// - version-first 三段：`<major>-<minor>-<kind>`（如 `3-5-sonnet`）；
/// - 两段且无次版本号：`<kind>-<major>` 或 `<major>-<kind>`（如 `sonnet-5`）；
/// - 单段：纯数字记为 `{major, minor:0}`，否则整体作为 kind。
fn parse_claude_version(rest: &str) -> (Option<SemVer>, Option<Box<str>>) {
    let mut it = rest.rsplitn(3, '-');
    let last = it.next();
    let mid = it.next();
    let head = it.next();

    match (last, mid, head) {
        (Some(a), Some(b), Some(c)) => {
            if let (Ok(minor), Ok(major)) = (a.parse::<u16>(), b.parse::<u16>()) {
                (Some(SemVer { major, minor }), non_empty_box(c))
            } else if let (Ok(minor), Ok(major)) = (b.parse::<u16>(), c.parse::<u16>()) {
                (Some(SemVer { major, minor }), non_empty_box(a))
            } else {
                (None, non_empty_box(rest))
            }
        }
        (Some(a), Some(b), None) => {
            if let Ok(major) = a.parse::<u16>() {
                (Some(SemVer { major, minor: 0 }), non_empty_box(b))
            } else if let Ok(major) = b.parse::<u16>() {
                (Some(SemVer { major, minor: 0 }), non_empty_box(a))
            } else {
                (None, non_empty_box(rest))
            }
        }
        (Some(a), None, None) => {
            if let Ok(major) = a.parse::<u16>() {
                (Some(SemVer { major, minor: 0 }), None)
            } else {
                (None, non_empty_box(a))
            }
        }
        _ => (None, None),
    }
}

/// Anthropic：kind-first（`claude-opus-4-5`）与 version-first（`claude-3-5-sonnet`）
/// 两种写法都要认，并容忍 `-latest`/`-thinking`/日期后缀。
fn parse_anthropic(id: &str) -> Option<ParsedModel> {
    let after_prefix = id.strip_prefix("claude-")?;
    let rest = strip_anthropic_suffix(after_prefix);
    if rest.is_empty() {
        return None;
    }
    let (version, variant) = parse_claude_version(rest);
    Some(ParsedModel {
        family: ModelFamily::Claude,
        version,
        variant,
    })
}

/// `o` 后紧跟一位数字才算 o-series（`o3`、`o4-mini`），借此和 `opus`/`openai` 区分。
fn strip_o_series_prefix(id: &str) -> Option<&str> {
    let rest = id.strip_prefix('o')?;
    rest.chars().next().filter(char::is_ascii_digit)?;
    Some(rest)
}

/// OpenAI 家族：`gpt-5.4`、`gpt-4o` 归 [`ModelFamily::Gpt`]；`o3`、`o4-mini` 归
/// [`ModelFamily::OSeries`]；`codex-*` 归 [`ModelFamily::Codex`]。
fn parse_openai(id: &str) -> Option<ParsedModel> {
    if let Some(rest) = id.strip_prefix("codex") {
        let rest = rest.strip_prefix('-').unwrap_or(rest);
        let (version, tail) = match parse_leading_version(rest) {
            Some((v, t)) => (Some(v), t),
            None => (None, rest),
        };
        return Some(ParsedModel {
            family: ModelFamily::Codex,
            version,
            variant: tail_variant(tail),
        });
    }
    if let Some(rest) = strip_o_series_prefix(id) {
        let (version, tail) = match parse_leading_version(rest) {
            Some((v, t)) => (Some(v), t),
            None => (None, rest),
        };
        return Some(ParsedModel {
            family: ModelFamily::OSeries,
            version,
            variant: tail_variant(tail),
        });
    }
    let rest = id.strip_prefix("gpt-").or_else(|| id.strip_prefix("gpt"))?;
    let (version, tail) = match parse_leading_version(rest) {
        Some((v, t)) => (Some(v), t),
        None => (None, rest),
    };
    Some(ParsedModel {
        family: ModelFamily::Gpt,
        version,
        variant: tail_variant(tail),
    })
}

/// GLM：`glm-4.6`、`glm-5.2`，变体可能紧贴版本号（如 `glm-4.5v` 的 `v`）。
fn parse_glm(id: &str) -> Option<ParsedModel> {
    let rest = id.strip_prefix("glm-").or_else(|| id.strip_prefix("glm"))?;
    let (version, tail) = match parse_leading_version(rest) {
        Some((v, t)) => (Some(v), t),
        None => (None, rest),
    };
    Some(ParsedModel {
        family: ModelFamily::Glm,
        version,
        variant: tail_variant(tail),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_model_id_strips_deepest_provider_segment() {
        assert_eq!(
            bare_model_id("openrouter/anthropic/claude-opus-4-5"),
            "claude-opus-4-5"
        );
        assert_eq!(bare_model_id("gpt-4o"), "gpt-4o");
        assert_eq!(bare_model_id(""), "");
    }

    #[test]
    fn strip_bracket_tags_strips_leading_and_trailing() {
        assert_eq!(
            strip_bracket_tags("[Kiro] claude-opus-4-5"),
            "claude-opus-4-5"
        );
        assert_eq!(strip_bracket_tags("gpt-4o [Free]"), "gpt-4o");
        assert_eq!(strip_bracket_tags("[A] gpt-4o [B]"), "gpt-4o");
        assert_eq!(strip_bracket_tags("claude-opus-4-5"), "claude-opus-4-5");
        // 剥空不生效：整串就是一个标签时原样返回。
        assert_eq!(strip_bracket_tags("[only-a-tag]"), "[only-a-tag]");
    }

    #[test]
    fn parses_anthropic_kind_first() {
        let parsed = parse_known_model("claude-opus-4-5").expect("claude-opus-4-5 应可解析");
        assert_eq!(parsed.family, ModelFamily::Claude);
        assert_eq!(parsed.version, Some(SemVer { major: 4, minor: 5 }));
        assert_eq!(parsed.variant.as_deref(), Some("opus"));
    }

    #[test]
    fn parses_anthropic_version_first() {
        let parsed = parse_known_model("claude-3-5-sonnet").expect("claude-3-5-sonnet 应可解析");
        assert_eq!(parsed.family, ModelFamily::Claude);
        assert_eq!(parsed.version, Some(SemVer { major: 3, minor: 5 }));
        assert_eq!(parsed.variant.as_deref(), Some("sonnet"));
    }

    #[test]
    fn parses_anthropic_with_date_and_thinking_suffix() {
        let parsed = parse_known_model("claude-opus-4-1-20250805-thinking")
            .expect("带日期与 thinking 后缀的 id 应可解析");
        assert_eq!(parsed.version, Some(SemVer { major: 4, minor: 1 }));
        assert_eq!(parsed.variant.as_deref(), Some("opus"));
    }

    #[test]
    fn parses_anthropic_single_number_version() {
        let parsed = parse_known_model("claude-sonnet-5").expect("claude-sonnet-5 应可解析");
        assert_eq!(parsed.version, Some(SemVer { major: 5, minor: 0 }));
        assert_eq!(parsed.variant.as_deref(), Some("sonnet"));
    }

    #[test]
    fn parses_gpt_dotted_version() {
        let parsed = parse_known_model("gpt-5.4").expect("gpt-5.4 应可解析");
        assert_eq!(parsed.family, ModelFamily::Gpt);
        assert_eq!(parsed.version, Some(SemVer { major: 5, minor: 4 }));
        assert_eq!(parsed.variant, None);
    }

    #[test]
    fn parses_gpt_letter_suffix_as_variant() {
        let parsed = parse_known_model("gpt-4o").expect("gpt-4o 应可解析");
        assert_eq!(parsed.version, Some(SemVer { major: 4, minor: 0 }));
        assert_eq!(parsed.variant.as_deref(), Some("o"));
    }

    #[test]
    fn parses_o_series_and_codex() {
        let o3 = parse_known_model("o3").expect("o3 应可解析");
        assert_eq!(o3.family, ModelFamily::OSeries);
        assert_eq!(o3.version, Some(SemVer { major: 3, minor: 0 }));

        let o4_mini = parse_known_model("o4-mini").expect("o4-mini 应可解析");
        assert_eq!(o4_mini.family, ModelFamily::OSeries);
        assert_eq!(o4_mini.variant.as_deref(), Some("mini"));

        let codex = parse_known_model("codex-mini").expect("codex-mini 应可解析");
        assert_eq!(codex.family, ModelFamily::Codex);
        assert_eq!(codex.variant.as_deref(), Some("mini"));
    }

    #[test]
    fn parses_gemini() {
        let parsed = parse_known_model("gemini-2.5-pro").expect("gemini-2.5-pro 应可解析");
        assert_eq!(parsed.family, ModelFamily::Gemini);
        assert_eq!(parsed.version, Some(SemVer { major: 2, minor: 5 }));
        assert_eq!(parsed.variant.as_deref(), Some("pro"));
    }

    #[test]
    fn glm_is_not_captured_by_openai_rule() {
        let parsed = parse_known_model("glm-4.6").expect("glm-4.6 应可解析");
        assert_eq!(parsed.family, ModelFamily::Glm);
        assert_eq!(parsed.version, Some(SemVer { major: 4, minor: 6 }));
        assert_eq!(parsed.variant, None);

        let air = parse_known_model("glm-4.5-air").expect("glm-4.5-air 应可解析");
        assert_eq!(air.family, ModelFamily::Glm);
        assert_eq!(air.variant.as_deref(), Some("air"));
    }

    #[test]
    fn unknown_id_returns_none() {
        assert_eq!(parse_known_model("some-totally-unknown-model"), None);
    }
}
