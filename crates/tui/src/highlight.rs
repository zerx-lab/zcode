//! syntect 语法高亮：源码 → 按主题 11 个 `syntax*`/`diff*` 字段上色的 `Line`。
//!
//! 只做「解析 scope + 上色」，不碰宽度/换行/tab——那些是调用方（[`crate::markdown`]）
//! 的职责，交由 [`crate::wrap`] 与 `zcode_text::width` 统一处理
//! （见 `rule://zcode-architecture`「TUI 输出清理」）。
//!
//! # 为什么不用 `syntect::easy::HighlightLines`
//!
//! 那条路径要一份 `.tmTheme`，本仓刻意不开 syntect 的 `default-themes`
//! feature（配色来自本仓主题的 11 个 `syntax*` 键，`.tmTheme` 一个都用不上，
//! 见 `Cargo.toml` 里 `syntect` 依赖上的注释）。这里直接用 `ParseState` 推进
//! 语法状态机拿到裸的 `(字节偏移, ScopeStackOp)` 流，自己套主题色——算法照抄
//! syntect 自带的 `RangedHighlightIterator`（`syntect-5.3.0/src/highlighting/highlighter.rs:148-206`）：
//! 每个 op 之前的文本用「应用该 op 之前」的作用域栈上色，再应用 op、推进位置。
//!
//! # `default-syntaxes` 与 bincode
//!
//! 内置语法集是一份 bincode 序列化的 dump，于是 `syntect` 把 bincode 1.3.3 拖了
//! 进来——它已停止维护（`RUSTSEC-2025-0141`，是 unmaintained 通告，不是漏洞）。
//! 输入是编译进二进制的静态字节、不接触任何外部数据，攻击面为零；豁免与复查条件
//! 写在 `deny.toml` 的 `advisories.ignore` 里。
//!
//! 唯一的规避方式是关掉 `default-syntaxes`、改成启动时解析 YAML 语法定义，那会把
//! 首次高亮的延迟从微秒级推到数百毫秒——TUI 的一帧预算只有 33 ms，付不起。

use std::collections::HashMap;
use std::sync::OnceLock;

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use syntect::parsing::{Scope, ScopeStack, ScopeStackOp, SyntaxReference, SyntaxSet};

use crate::theme::Theme;

/// 语法高亮器：懒加载 syntect 默认语法集，线程安全只读共享。
#[derive(Debug)]
pub struct Highlighter {
    syntax_set: SyntaxSet,
}

impl Highlighter {
    /// 进程级单例。语法集加载（解析全部内置 `.sublime-syntax`）有实测可观耗时，
    /// 每次高亮都重建会在 TUI 的重绘热路径上反复吃这份成本。
    #[must_use]
    pub fn shared() -> &'static Highlighter {
        static INSTANCE: OnceLock<Highlighter> = OnceLock::new();
        INSTANCE.get_or_init(|| Highlighter {
            syntax_set: SyntaxSet::load_defaults_newlines(),
        })
    }

    /// `lang` 能否解析出已知语法（不含纯文本兜底）。
    #[must_use]
    pub fn supports(&self, lang: &str) -> bool {
        self.resolve_syntax(lang).is_some()
    }

    /// 高亮一整段代码，按 `\n` 拆行。
    ///
    /// - `lang` 为 `None` 或解析不出已知语法时，整段按 `fallback` 单色输出，
    ///   不改变任何字符（含空白、大小写、行尾）。
    /// - **不变式**：本函数只加样式，不增删任何可见字符——把返回的全部 `Span`
    ///   内容按行拼接必须逐字节等于输入的对应行。测试 `highlight_is_lossless`
    ///   钉住这条。
    #[must_use]
    pub fn highlight(
        &self,
        code: &str,
        lang: Option<&str>,
        theme: &Theme,
        fallback: Style,
    ) -> Vec<Line<'static>> {
        let syntax = lang.and_then(|l| self.resolve_syntax(l));
        let Some(syntax) = syntax else {
            return plain_lines(code, fallback);
        };

        let mut parse_state = syntect::parsing::ParseState::new(syntax);
        let mut scope_stack = ScopeStack::new();
        let mut cache: ScopeClassCache = HashMap::new();
        let mut lines = Vec::new();

        // `SyntaxSet::load_defaults_newlines()` 要求逐行传入**带换行符**的文本
        // （见其文档："newlines" 变体的语法在末尾换行上匹配规则，比如注释延续）。
        // `ops` 里的字节偏移是相对这个带换行符的 `raw_line` 算的，所以取子串时也必须
        // 用 `raw_line`（而不是提前去掉换行符的版本）——否则偏移会指向裁剪后字符串的
        // 界外，`str::get` 返回 `None`，导致换行符之前的最后一段文本被悄悄吞掉。
        // 换行符本身只喂给解析器、不进最终的 `Span`，由 `render_ops_line` 在收尾时剥离。
        for raw_line in split_keep_newline(code) {
            let ops = parse_state.parse_line(raw_line, &self.syntax_set);
            let Ok(ops) = ops else {
                // 解析器状态机内部错误（配置问题，不是用户输入问题）：整行退化为
                // fallback 单色，绝不 panic，也绝不吞掉字符。
                lines.push(Line::from(Span::styled(
                    strip_line_ending(raw_line).to_owned(),
                    fallback,
                )));
                continue;
            };
            lines.push(render_ops_line(
                raw_line,
                &ops,
                &mut scope_stack,
                theme,
                fallback,
                &mut cache,
            ));
        }

        if lines.is_empty() {
            lines.push(Line::default());
        }
        lines
    }

    /// 语言解析顺序对应 oh-my-pi `crates/pi-natives/src/highlight.rs:358-373`：
    /// token → 扩展名 → 别名表 → 语法名。全不命中返回 `None`（调用方回落纯文本）。
    fn resolve_syntax(&self, lang: &str) -> Option<&SyntaxReference> {
        self.syntax_set
            .find_syntax_by_token(lang)
            .or_else(|| self.syntax_set.find_syntax_by_extension(lang))
            .or_else(|| {
                let canonical = resolve_alias(lang)?;
                self.syntax_set.find_syntax_by_name(canonical)
            })
            .or_else(|| self.syntax_set.find_syntax_by_name(lang))
    }
}

/// 语言别名 → syntect 默认语法集里的规范语法名（`SyntaxReference::name`）。
/// 子集覆盖 oh-my-pi `crates/pi-natives/src/highlight.rs:171-220`；右侧名字均已用
/// `cargo test -p zcode-tui --lib highlight::tests::default_syntax_set_has_expected_names`
/// 核对 syntect `default-syntaxes` 特性实际打包的语法名，没有凭空写。
/// `toml`/`go` 在本仓 syntect 默认集里不存在同名语法，因此不进表——按规范会诚实回落纯文本。
const LANG_ALIASES: &[(&str, &str)] = &[
    ("rs", "Rust"),
    ("rust", "Rust"),
    ("ts", "TypeScript"),
    ("tsx", "TSX"),
    ("js", "JavaScript"),
    ("jsx", "JavaScript (Babel)"),
    ("mjs", "JavaScript"),
    ("cjs", "JavaScript"),
    ("py", "Python"),
    ("sh", "Bourne Again Shell (bash)"),
    ("bash", "Bourne Again Shell (bash)"),
    ("zsh", "Bourne Again Shell (bash)"),
    ("shell", "Bourne Again Shell (bash)"),
    ("md", "Markdown"),
    ("yml", "YAML"),
    ("yaml", "YAML"),
    ("json", "JSON"),
    ("html", "HTML"),
    ("vue", "HTML"),
    ("svelte", "HTML"),
    ("astro", "HTML"),
    ("css", "CSS"),
    ("c", "C"),
    ("h", "C"),
    ("cpp", "C++"),
    ("cc", "C++"),
    ("hpp", "C++"),
    ("java", "Java"),
    ("rb", "Ruby"),
    ("ruby", "Ruby"),
    ("diff", "Diff"),
    ("patch", "Diff"),
];

fn resolve_alias(lang: &str) -> Option<&'static str> {
    LANG_ALIASES
        .iter()
        .find(|(alias, _)| alias.eq_ignore_ascii_case(lang))
        .map(|(_, name)| *name)
}

/// 无已知语法或未指定语言时的单色兜底：逐行切分，不做任何解析。
/// 复用 [`split_keep_newline`] 的切行规则（而不是直接 `code.split('\n')`），保证
/// 「有已知语法」与「回落纯文本」两条路径对同一份输入产出相同的行数——`code.split('\n')`
/// 在结尾有换行符时会多出一个空字符串元素，跟 `split_keep_newline` 的约定不一致。
fn plain_lines(code: &str, fallback: Style) -> Vec<Line<'static>> {
    if code.is_empty() {
        return vec![Line::default()];
    }
    split_keep_newline(code)
        .map(|raw| Line::from(Span::styled(strip_line_ending(raw).to_owned(), fallback)))
        .collect()
}

/// 剥掉一行末尾的换行符（`"\r\n"` 或 `"\n"`），只处理一次——[`split_keep_newline`]
/// 保证每段至多带一个结尾换行，不会出现需要循环剥离的情形。
fn strip_line_ending(raw: &str) -> &str {
    raw.strip_suffix('\n')
        .map_or(raw, |s| s.strip_suffix('\r').unwrap_or(s))
}

/// 按 `\n` 切分但保留分隔符本身（syntect 的 `LinesWithEndings` 等价实现）：
/// 语法状态机需要看到行尾换行符才能正确处理「注释延续到行尾」一类规则。
/// 输入没有结尾换行符时最后一段也照常产出（不丢内容）。
fn split_keep_newline(s: &str) -> impl Iterator<Item = &str> {
    let mut rest = s;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let idx = rest.find('\n').map_or(rest.len(), |i| i + 1);
        let (line, tail) = rest.split_at(idx);
        rest = tail;
        Some(line)
    })
}

/// 把一行的 `(字节偏移, ScopeStackOp)` 流套色。算法见模块文档引用的
/// `RangedHighlightIterator`：每个 op 之前的文本段用「应用该 op 之前」的
/// 作用域栈上色，再应用 op、推进游标。
fn render_ops_line(
    line_text: &str,
    ops: &[(usize, ScopeStackOp)],
    scope_stack: &mut ScopeStack,
    theme: &Theme,
    fallback: Style,
    cache: &mut ScopeClassCache,
) -> Line<'static> {
    let mut spans = Vec::new();
    let mut pos = 0usize;

    for (offset, op) in ops {
        if *offset > pos
            && let Some(text) = line_text.get(pos..*offset)
        {
            push_span(&mut spans, text, scope_stack, theme, fallback, cache);
        }
        // `apply` 只在 `clear_scopes` 选择器语法错误时失败；解析器自身产生的 op 恒合法，
        // 失败时保守地维持当前栈不变（不 panic，宁可少上一段色也不中断整行渲染）。
        let _ = scope_stack.apply(op);
        pos = (*offset).max(pos);
    }
    if pos < line_text.len()
        && let Some(text) = line_text.get(pos..)
    {
        push_span(&mut spans, text, scope_stack, theme, fallback, cache);
    }
    // `line_text` 传入时带着喂给解析器用的结尾换行符（见调用方注释），这里在收尾时
    // 一次性剥掉，不让它进最终的 `Span`。
    strip_trailing_newline(&mut spans);
    if spans.is_empty() {
        spans.push(Span::styled(String::new(), fallback));
    }
    Line::from(spans)
}

/// 剥掉行尾 `Span` 里残留的换行符（`render_ops_line` 的 `line_text` 带着换行符喂给
/// 解析器，此前的字节偏移都是相对它算的，只有落笔前这一刻能安全地把换行符去掉）。
/// 极端情形下换行符会自成一个 span（scope 恰好在换行符处切换），此时整段丢弃后继续
/// 检查前一个 span，直到找到不以换行符结尾的那一个或 `spans` 耗尽。
fn strip_trailing_newline(spans: &mut Vec<Span<'static>>) {
    loop {
        let Some(last) = spans.last() else { return };
        let content = last.content.as_ref();
        let trimmed = strip_line_ending(content);
        if trimmed.len() == content.len() {
            return;
        }
        if trimmed.is_empty() {
            spans.pop();
            continue;
        }
        let owned = trimmed.to_owned();
        if let Some(last_mut) = spans.last_mut() {
            last_mut.content = owned.into();
        }
        return;
    }
}

fn push_span(
    spans: &mut Vec<Span<'static>>,
    text: &str,
    scope_stack: &ScopeStack,
    theme: &Theme,
    fallback: Style,
    cache: &mut ScopeClassCache,
) {
    if text.is_empty() {
        return;
    }
    let style = classify_stack(scope_stack, cache)
        .map_or(fallback, |class| Style::new().fg(class.color(theme)));
    spans.push(Span::styled(text.to_owned(), style));
}

/// scope → 主题字段的语义分类，按下表优先级从 scope 栈**最内层往外**取第一个命中
/// （表与顺序照抄 oh-my-pi `crates/pi-natives/src/highlight.rs:243-337`）：
///
/// | 优先级 | 主题字段 | 匹配前缀 |
/// |---|---|---|
/// | 1 | `syntax_comment` | `comment` |
/// | 2 | `diff_added` | `markup.inserted` |
/// | 3 | `diff_removed` | `markup.deleted` |
/// | 4 | `syntax_keyword` | `meta.diff.header`, `meta.diff.range` |
/// | 5 | `syntax_string` | `string`, `constant.character`, `meta.string` |
/// | 6 | `syntax_number` | `constant.numeric`, `constant.integer` |
/// | 7 | `syntax_keyword` | `keyword`, `storage.type`, `storage.modifier` |
/// | 8 | `syntax_function` | `entity.name.function`, `support.function`, `meta.function-call`, `variable.function` |
/// | 9 | `syntax_type` | `entity.name.type/class/struct/enum/interface/trait`, `support.type`, `support.class` |
/// | 10 | `syntax_operator` | `keyword.operator`, `punctuation.accessor` |
/// | 11 | `syntax_punctuation` | `punctuation` |
/// | 12 | `syntax_variable` | `variable`, `entity.name`, `meta.path` |
/// | 13 | `syntax_number`（兜底） | `constant` |
///
/// 注意第 7 行的通用前缀 `keyword` 会先于第 10 行的 `keyword.operator` 命中——
/// 这是上游表本身的顺序（generic 分类排在更具体的 operator 分类之前），照抄不改。
#[derive(Debug, Clone, Copy)]
enum SemanticClass {
    /// 注释。
    Comment,
    /// diff 新增行。
    DiffAdded,
    /// diff 删除行。
    DiffRemoved,
    /// 关键字。
    Keyword,
    /// 字符串。
    String,
    /// 数字。
    Number,
    /// 函数名。
    Function,
    /// 类型名。
    Type,
    /// 操作符。
    Operator,
    /// 标点。
    Punctuation,
    /// 变量名。
    Variable,
}

impl SemanticClass {
    fn color(self, theme: &Theme) -> ratatui::style::Color {
        match self {
            Self::Comment => theme.colors.syntax_comment,
            Self::DiffAdded => theme.colors.diff_added,
            Self::DiffRemoved => theme.colors.diff_removed,
            Self::Keyword => theme.colors.syntax_keyword,
            Self::String => theme.colors.syntax_string,
            Self::Number => theme.colors.syntax_number,
            Self::Function => theme.colors.syntax_function,
            Self::Type => theme.colors.syntax_type,
            Self::Operator => theme.colors.syntax_operator,
            Self::Punctuation => theme.colors.syntax_punctuation,
            Self::Variable => theme.colors.syntax_variable,
        }
    }
}

/// 上表按行顺序展开成可迭代的优先级列表；`find_map` 保证按序取第一个命中。
const PRIORITY_TABLE: &[(SemanticClass, &[&str])] = &[
    (SemanticClass::Comment, &["comment"]),
    (SemanticClass::DiffAdded, &["markup.inserted"]),
    (SemanticClass::DiffRemoved, &["markup.deleted"]),
    (
        SemanticClass::Keyword,
        &["meta.diff.header", "meta.diff.range"],
    ),
    (
        SemanticClass::String,
        &["string", "constant.character", "meta.string"],
    ),
    (
        SemanticClass::Number,
        &["constant.numeric", "constant.integer"],
    ),
    (
        SemanticClass::Keyword,
        &["keyword", "storage.type", "storage.modifier"],
    ),
    (
        SemanticClass::Function,
        &[
            "entity.name.function",
            "support.function",
            "meta.function-call",
            "variable.function",
        ],
    ),
    (
        SemanticClass::Type,
        &[
            "entity.name.type",
            "entity.name.class",
            "entity.name.struct",
            "entity.name.enum",
            "entity.name.interface",
            "entity.name.trait",
            "support.type",
            "support.class",
        ],
    ),
    (
        SemanticClass::Operator,
        &["keyword.operator", "punctuation.accessor"],
    ),
    (SemanticClass::Punctuation, &["punctuation"]),
    (
        SemanticClass::Variable,
        &["variable", "entity.name", "meta.path"],
    ),
    (SemanticClass::Number, &["constant"]),
];

/// `Scope` → 分类结果的线程内缓存。`Scope` 本身只是 16 字节的位打包整数，比较
/// 极快，但把它转回字符串（`build_string`）要锁一次全局字符串仓库
/// （`syntect::parsing::scope::lock_global_scope_repo`），逐 span 都做这件事在
/// 长代码块上很伤：oh-my-pi 那边实测过 100 行 ~26ms、150 行 ~40ms
/// （`packages/coding-agent/src/modes/theme/theme.ts:2942-2948`），足以吃掉一帧
/// 33ms 的预算。按 `Scope`（可 `Copy`/`Hash`）建缓存后，同一语法块内反复出现的
/// scope（绝大多数——同一 context 里的 token 类型高度重复）只解析一次。
type ScopeClassCache = HashMap<Scope, Option<SemanticClass>>;

fn classify_stack(stack: &ScopeStack, cache: &mut ScopeClassCache) -> Option<SemanticClass> {
    stack
        .scopes
        .iter()
        .rev()
        .find_map(|scope| classify_scope(*scope, cache))
}

fn classify_scope(scope: Scope, cache: &mut ScopeClassCache) -> Option<SemanticClass> {
    if let Some(cached) = cache.get(&scope) {
        return *cached;
    }
    let text = scope.build_string();
    let class = PRIORITY_TABLE.iter().find_map(|(class, prefixes)| {
        prefixes
            .iter()
            .any(|prefix| scope_has_prefix(&text, prefix))
            .then_some(*class)
    });
    cache.insert(scope, class);
    class
}

/// `TextMate` scope 前缀匹配：`prefix` 是 `text` 本身，或 `text` 在下一个 `.` 边界处
/// 截断后与之相等（避免 `"stringx"` 误配 `"string"`）。
fn scope_has_prefix(text: &str, prefix: &str) -> bool {
    text == prefix || text.starts_with(prefix) && text[prefix.len()..].starts_with('.')
}

#[cfg(test)]
mod tests {
    use ratatui::style::Color;
    use zcode_text::width::visible_width;

    use super::{Highlighter, scope_has_prefix};
    use crate::theme::{BuiltinTheme, ColorMode, SymbolPreset, Theme};

    fn dark_theme() -> Theme {
        BuiltinTheme::Dark
            .load(ColorMode::TrueColor, SymbolPreset::Unicode)
            .expect("内置暗色主题必须能解析")
    }

    /// 探测测试：把 syntect `default-syntaxes` 实际打包的语法名打印出来，供
    /// `LANG_ALIASES` 表核对，不能凭空写别名映射的右值。
    #[test]
    fn default_syntax_set_has_expected_names() {
        let highlighter = Highlighter::shared();
        let names: Vec<&str> = highlighter
            .syntax_set
            .syntaxes()
            .iter()
            .map(|s| s.name.as_str())
            .collect();
        for expected in [
            "Rust",
            "Python",
            "JSON",
            "YAML",
            "HTML",
            "CSS",
            "C",
            "C++",
            "Java",
            "Ruby",
            "Diff",
            "Markdown",
            "Bourne Again Shell (bash)",
        ] {
            assert!(
                names.contains(&expected),
                "syntect 默认语法集缺少 {expected:?}，实际语法名：{names:?}"
            );
        }
    }

    #[test]
    fn unknown_language_falls_back_to_plain_style() {
        let theme = dark_theme();
        let fallback = theme.text();
        let lines = Highlighter::shared().highlight(
            "let x = 1;",
            Some("not-a-real-lang"),
            &theme,
            fallback,
        );
        assert_eq!(lines.len(), 1);
        for span in &lines[0].spans {
            assert_eq!(span.style, fallback);
        }
    }

    #[test]
    fn none_language_falls_back_to_plain_style_without_touching_bytes() {
        let theme = dark_theme();
        let fallback = theme.text();
        let code = "line one\nline two\n";
        let lines = Highlighter::shared().highlight(code, None, &theme, fallback);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].spans[0].content.as_ref(), "line one");
        assert_eq!(lines[1].spans[0].content.as_ref(), "line two");
    }

    /// 不变式：高亮只加样式，不增删任何可见字符。逐行把 span 拼回去必须等于输入。
    #[test]
    fn highlight_is_lossless_for_known_language() {
        let theme = dark_theme();
        let fallback = theme.text();
        let code = "fn main() {\n    let x: u32 = 1 + 2; // comment\n    println!(\"{x}\");\n}\n";
        let lines = Highlighter::shared().highlight(code, Some("rust"), &theme, fallback);
        let rebuilt: String = lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        let expected = code.strip_suffix('\n').unwrap_or(code);
        // `highlight` 按 `split_keep_newline` 切行：结尾换行符只用于喂给解析器，不产出
        // 额外的空行，因此把每行拼回去、用 `\n` 相接，应当精确等于「去掉源码末尾换行符」
        // 之后的原文——这正是"只加样式、不增删可见字符"的不变式。
        assert_eq!(rebuilt, expected);
    }

    #[test]
    fn rust_keyword_and_string_get_distinct_colors() {
        let theme = dark_theme();
        let fallback = theme.text();
        let lines =
            Highlighter::shared().highlight("let s = \"hi\";", Some("rust"), &theme, fallback);
        assert_eq!(lines.len(), 1);
        let colors: Vec<Color> = lines[0]
            .spans
            .iter()
            .map(|s| s.style.fg.unwrap_or(Color::Reset))
            .collect();
        // `let` 关键字应上 syntax_keyword 色，字符串字面量应上 syntax_string 色，
        // 且两者不同（回归主题里 keyword != string 的前提，若某主题两色相同这条测试需要换主题验证而非放宽）。
        assert!(colors.contains(&theme.colors.syntax_keyword));
        assert!(colors.contains(&theme.colors.syntax_string));
        assert_ne!(theme.colors.syntax_keyword, theme.colors.syntax_string);
    }

    #[test]
    fn scope_prefix_matches_atom_boundary_not_substring() {
        assert!(scope_has_prefix("string.quoted.double.rust", "string"));
        assert!(scope_has_prefix("string", "string"));
        assert!(!scope_has_prefix("stringx.foo", "string"));
    }

    #[test]
    fn supports_reports_known_and_unknown_languages() {
        let highlighter = Highlighter::shared();
        assert!(highlighter.supports("rust"));
        assert!(highlighter.supports("py"));
        assert!(!highlighter.supports("not-a-real-lang"));
    }

    #[test]
    fn highlighted_lines_have_expected_visible_width() {
        let theme = dark_theme();
        let fallback = theme.text();
        let code = "let total = 1 + 2;";
        let lines = Highlighter::shared().highlight(code, Some("rust"), &theme, fallback);
        assert_eq!(lines.len(), 1);
        let width: usize = lines[0]
            .spans
            .iter()
            .map(|s| visible_width(&s.content))
            .sum();
        assert_eq!(width, visible_width(code));
    }
}
