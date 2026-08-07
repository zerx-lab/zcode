//! `grep` 工具：用 `zcode_text::grep` 的进程内引擎搜索文件内容。
//!
//! 不 fork `rg` 子进程——理由与取舍已经在 `crates/text/src/grep.rs:1-8` 写清楚，本文件只
//! 负责把它接成一个 [`zcode_agent::tool::Tool`]：解析参数、把同步阻塞的
//! [`zcode_text::grep::grep`] 派发到 [`tokio::task::spawn_blocking`]、桥接硬取消、
//! 做基于文件数的分页。

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use serde_json::{Value, json};
use zcode_agent::{ApprovalDecision, Concurrency, Tier, Tool, ToolContext, ToolError, ToolOutput};
use zcode_text::{CaseMode, FileMatches, GrepLimits, GrepRequest, MatcherKind};

use crate::config::ToolsConfig;
use crate::tools::output;
use crate::tools::read::{self, LineRange, Selector};
use crate::workspace::{PathError, Workspace};

/// grep 工具描述（发给模型的文本）。
const DESCRIPTION: &str = include_str!("prompts/grep.md");

/// 一页最多展示的命中文件数，同时也是 [`GrepLimits::default`] 的 `max_files`
/// （`crates/text/src/grep.rs:77`）。分页时把它当作页大小复用，翻页语义因此与
/// "不分页时的默认行为"完全一致，不需要另起一个数字。
const FILE_WINDOW: usize = 20;

/// [`GrepTool::execute`] 解析完 `pattern`/`case`/`skip` 之后的结构化视图。
struct ParsedArgs {
    /// 正则模式（已去除首尾空白，非空）。
    pattern: String,
    /// 大小写匹配模式。
    case: CaseMode,
    /// 跳过的文件数。
    skip: usize,
}

/// 解析并校验 `grep` 工具除 `path` 外的入参；不碰文件系统。
fn parse_args(args: &Value) -> Result<ParsedArgs, ToolError> {
    let pattern = args
        .get("pattern")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| output::error("`pattern` 不能为空"))?
        .to_owned();

    let case = match args.get("case").and_then(Value::as_bool) {
        Some(true) => CaseMode::Sensitive,
        Some(false) => CaseMode::Insensitive,
        // 省略时按智能大小写（ripgrep `--smart-case` 语义，见
        // `crates/text/src/grep.rs:39` 的 `CaseMode::Smart` 文档）：对一个默认搜索工具而言，
        // 这比"总是区分"或"总是忽略"都更贴近直觉预期。
        None => CaseMode::Smart,
    };

    let skip = match args.get("skip") {
        None | Some(Value::Null) => 0usize,
        Some(value) => {
            let n = value
                .as_u64()
                .ok_or_else(|| output::error("`skip` 必须是非负整数"))?;
            usize::try_from(n).map_err(|_| output::error("`skip` 超出可表示范围"))?
        }
    };

    Ok(ParsedArgs {
        pattern,
        case,
        skip,
    })
}

/// 一次路径参数解析出的搜索目标。
struct PathSpec {
    /// 用户原始写法（不含选择器后缀），报错/展示时用。
    raw: String,
    /// [`Workspace::resolve`] 之后的绝对路径。
    resolved: PathBuf,
    /// 若原始写法携带了 read 风格的行区间选择器，这里是解析后的限制；`None` 表示不限制。
    ranges: Option<Vec<LineRange>>,
}

/// 把 [`PathError`] 翻译成面向模型的中文说明。
fn describe_path_error(err: &PathError, raw: &str) -> String {
    match err {
        PathError::Empty => "路径不能为空".to_owned(),
        PathError::NotUtf8 => format!("路径 `{raw}` 归一化后不是合法 UTF-8"),
    }
}

/// 探测 `raw` 整段是否恰好是磁盘上一个真实存在的路径。
///
/// 字面文件名压过选择器语法（oh-my-pi issue #4618，`ToolsReadLs` 落盘 `read.rs` 时的约定）：
/// 真实存在的 `foo:bar` 文件不该被误当成"文件 `foo` 加选择器 `bar`"。这几行是"探测"逻辑，不是
/// 选择器语法本身，`crate::tools::read` 没有把它做成公共函数（探测方式本就允许两边不同），
/// 这里按同一判据独立复刻，不强行依赖对方私有实现。
async fn literal_path_exists(workspace: &Workspace, raw: &str) -> bool {
    match workspace.resolve(raw) {
        Ok(resolved) => tokio::fs::metadata(&resolved.path).await.is_ok(),
        Err(_) => false,
    }
}

/// 解析 `path` 字段：按 `;` 拆分多目标，每个目标独立探测字面路径优先级，
/// 剩余部分交给 `crate::tools::read` 的选择器语法解析。
async fn parse_path_specs(
    workspace: &Workspace,
    raw_path_field: &str,
) -> Result<Vec<PathSpec>, ToolError> {
    let entries: Vec<&str> = raw_path_field
        .split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    let entries: Vec<&str> = if entries.is_empty() {
        vec!["."]
    } else {
        entries
    };

    let mut specs = Vec::with_capacity(entries.len());
    for raw in entries {
        let (path_part, ranges) = if literal_path_exists(workspace, raw).await {
            (raw, None)
        } else {
            let (path_part, selector) = read::split_path_and_selector(raw);
            let ranges = match selector {
                None => None,
                Some(selector_text) => match read::parse_selector(selector_text) {
                    Ok(Selector::Lines { ranges, .. }) => Some(ranges),
                    Ok(Selector::None | Selector::Raw) => None,
                    Err(err) => return Err(output::error(format!("路径选择器无效：{err}"))),
                },
            };
            (path_part, ranges)
        };
        let resolved = workspace
            .resolve(path_part)
            .map_err(|err| output::error(describe_path_error(&err, raw)))?;
        specs.push(PathSpec {
            raw: raw.to_owned(),
            resolved: resolved.path,
            ranges,
        });
    }
    Ok(specs)
}

/// 把解析出的 [`PathSpec`] 列表落成引擎能吃的搜索根：探测存在性、拆出行区间限制表、
/// 收集缺失路径。多路径调用容错跳过缺失目标，单路径调用保持原 ENOENT 语义。
async fn resolve_search_roots(
    workspace: &Workspace,
    raw_path: &str,
) -> Result<(Vec<PathBuf>, HashMap<PathBuf, Vec<LineRange>>, Vec<String>), ToolError> {
    let specs = parse_path_specs(workspace, raw_path).await?;
    let is_single = specs.len() == 1;

    let mut roots: Vec<PathBuf> = Vec::with_capacity(specs.len());
    let mut range_map: HashMap<PathBuf, Vec<LineRange>> = HashMap::new();
    let mut missing: Vec<String> = Vec::new();
    for spec in specs {
        if tokio::fs::metadata(&spec.resolved).await.is_err() {
            if is_single {
                return Err(output::error(format!(
                    "路径不存在：{}",
                    workspace.display(&spec.resolved)
                )));
            }
            missing.push(spec.raw);
            continue;
        }
        if let Some(ranges) = spec.ranges {
            range_map.insert(spec.resolved.clone(), ranges);
        }
        roots.push(spec.resolved);
    }
    if roots.is_empty() {
        return Err(output::error(format!("路径不存在：{}", missing.join(", "))));
    }
    Ok((roots, range_map, missing))
}

/// 一行是否落在 `ranges` 描述的任一区间内（1-indexed、含两端，`end == None` 表示开放到文件末尾）。
fn line_in_ranges(line_number: u64, ranges: &[LineRange]) -> bool {
    let Ok(line) = usize::try_from(line_number) else {
        return false;
    };
    ranges
        .iter()
        .any(|range| line >= range.start && range.end.is_none_or(|end| line <= end))
}

/// 正则退化为字面量匹配时，附加在输出里的告知文案——静默降级会让模型误以为正则语义生效了。
fn literal_degrade_note(pattern: &str) -> String {
    format!(
        "正则 `{pattern}` 编译失败，已退化为按字面量子串匹配（不支持正则语法，如需正则请修正模式）"
    )
}

/// 渲染一页结果；`window_files` 是本次调用实际设置的 `max_files` 上限，用来判断
/// "已知的文件总数是精确值还是下界"。
#[allow(clippy::too_many_arguments)]
fn render_page(
    mut files: Vec<FileMatches>,
    skip: usize,
    window_files: usize,
    matcher: MatcherKind,
    timed_out: bool,
    timeout_secs: f64,
    missing: &[String],
    pattern: &str,
    workspace: &Workspace,
) -> ToolOutput {
    let total_known = files.len();
    let page = if skip >= total_known {
        Vec::new()
    } else {
        files.split_off(skip)
    };

    let mut notes = Vec::new();
    if matcher == MatcherKind::Literal {
        notes.push(literal_degrade_note(pattern));
    }
    if timed_out {
        notes.push(format!(
            "grep 在 {timeout_secs:.0}s 内未完成，本次结果不完整"
        ));
    }
    if !missing.is_empty() {
        notes.push(format!("已跳过不存在的路径：{}", missing.join(", ")));
    }

    if page.is_empty() {
        let mut lines = Vec::new();
        if skip == 0 {
            lines.push("没有匹配".to_owned());
        } else {
            lines.push(format!(
                "没有更多结果（共 {total_known} 个文件；skip={skip} 已越过末尾）"
            ));
        }
        lines.extend(notes);
        return output::finish(lines.join("\n"), format!("grep {pattern}")).mark_useless();
    }

    let shown_end = skip.saturating_add(page.len());
    let has_more = total_known >= window_files;
    let mut header = format!(
        "显示文件 {}-{shown_end} / 共 {total_known}{} 个文件",
        skip + 1,
        if has_more { "+" } else { "" }
    );
    if has_more {
        let _ = write!(header, "。用 skip={shown_end} 翻下一页");
    } else {
        header.push('。');
    }

    let mut body = String::new();
    for file in &page {
        body.push_str(&workspace.display(&file.path));
        body.push('\n');
        for m in &file.matches {
            let _ = writeln!(body, "  {}: {}", m.line_number, m.line);
        }
        if file.truncated {
            body.push_str("  …该文件命中过多，已截断\n");
        }
        body.push('\n');
    }
    body.push_str(&header);
    for note in notes {
        body.push('\n');
        body.push_str(&note);
    }

    output::finish(body, format!("grep {pattern}"))
}

/// `grep` 内置工具：用进程内引擎搜索文件内容，按文件数分页。
#[derive(Debug)]
pub(crate) struct GrepTool {
    /// 工作区路径解析器。
    workspace: Arc<Workspace>,
}

impl GrepTool {
    /// 构造。`config` 当前不影响 grep 行为，仍按统一的工具构造签名接收。
    pub(crate) fn new(workspace: Arc<Workspace>, _config: &ToolsConfig) -> Self {
        Self { workspace }
    }
}

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &'static str {
        "grep"
    }

    fn description(&self) -> &'static str {
        DESCRIPTION
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "正则模式；编译失败时退化为字面量子串匹配，响应会明确告知"
                },
                "path": {
                    "type": "string",
                    "description": "要搜索的一个或多个文件/目录，用 `;` 分隔多个目标；单个文件可带 read 风格的行区间选择器（如 `src/foo.rs:50-100`）；省略时搜索工作区根目录"
                },
                "case": {
                    "type": "boolean",
                    "description": "true 强制区分大小写，false 强制忽略大小写；省略时按智能大小写（模式全小写才忽略）"
                },
                "skip": {
                    "type": "number",
                    "description": "跳过前面已经看过的文件数，用上次响应给出的 skip=<N> 翻页"
                }
            },
            "required": ["pattern"]
        })
    }

    fn approval(&self, _args: &Value) -> ApprovalDecision {
        ApprovalDecision::tier(Tier::Read)
    }

    fn concurrency(&self, _args: &Value) -> Concurrency {
        Concurrency::Shared
    }

    async fn execute(&self, args: Value, ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        let parsed = parse_args(&args)?;
        let raw_path = args.get("path").and_then(Value::as_str).unwrap_or(".");
        let (roots, range_map, missing) = resolve_search_roots(&self.workspace, raw_path).await?;

        // 分页要求把 [0, skip+FILE_WINDOW) 范围内的命中文件全部收进结果集，否则无法在 skip
        // 之后正确切出这一页；`max_total_matches` 按同等比例放大，避免一部分文件因为总量预算
        // 先耗尽而被静默截断，导致本该显示的这一页缺行（同 oh-my-pi grep.ts 里
        // `INTERNAL_TOTAL_CAP` 的分页动机，但改成按 window_files 动态计算而不是写死一个常数——
        // 写死的常数只覆盖前几页，深翻页会重现同一个坑）。
        let default_limits = GrepLimits::default();
        let window_files = parsed.skip.saturating_add(FILE_WINDOW);
        let limits = GrepLimits {
            max_files: window_files,
            max_total_matches: window_files.saturating_mul(default_limits.max_matches_per_file),
            ..default_limits
        };
        let timeout_secs = limits.timeout.as_secs_f64();

        // 硬取消桥接：InterruptSignal 只能异步等待，`zcode_text::grep::grep` 在
        // spawn_blocking 里同步运行、每进入一个文件前检查一次 `Option<&AtomicBool>`。
        // 用一个共享 AtomicBool 把"取消已到达"从异步侧搬进同步侧。
        let cancel_flag = Arc::new(AtomicBool::new(ctx.cancel.is_set()));
        let watcher_flag = Arc::clone(&cancel_flag);
        let watcher_signal = ctx.cancel.clone();
        let watcher = tokio::spawn(async move {
            watcher_signal.notified().await;
            watcher_flag.store(true, Ordering::Release);
        });

        let search_pattern = parsed.pattern.clone();
        let search_cancel = Arc::clone(&cancel_flag);
        let case = parsed.case;
        let joined = tokio::task::spawn_blocking(move || {
            let request = GrepRequest {
                pattern: &search_pattern,
                roots: &roots,
                include_globs: &[],
                exclude_globs: &[],
                case,
                multiline: false,
                respect_gitignore: true,
                include_hidden: true,
                limits,
                cancel: Some(search_cancel.as_ref()),
            };
            zcode_text::grep::grep(&request)
        })
        .await;
        watcher.abort();

        let outcome = joined.map_err(|_| output::error("grep 搜索任务异常终止"))?;
        let outcome = outcome.map_err(|err| output::error(format!("grep 搜索失败：{err}")))?;

        if ctx.cancel.is_set() {
            return Err(ToolError::Cancelled);
        }

        let mut files = outcome.files;
        if !range_map.is_empty() {
            for file in &mut files {
                if let Some(ranges) = range_map.get(&file.path) {
                    file.matches
                        .retain(|m| line_in_ranges(m.line_number, ranges));
                }
            }
            files.retain(|file| !file.matches.is_empty());
        }

        Ok(render_page(
            files,
            parsed.skip,
            window_files,
            outcome.matcher,
            outcome.timed_out,
            timeout_secs,
            &missing,
            &parsed.pattern,
            &self.workspace,
        ))
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;
    use tokio::sync::mpsc;
    use zcode_agent::{EntryId, InterruptSignal, SessionId};

    use super::*;
    use crate::config::ToolsConfig;

    fn test_config() -> ToolsConfig {
        ToolsConfig {
            disabled: Vec::new(),
            bash_timeout_secs: 30,
            read_max_lines: 2000,
        }
    }

    fn test_ctx(cwd: PathBuf) -> ToolContext {
        let (progress, _rx) = mpsc::unbounded_channel();
        ToolContext {
            session_id: SessionId::generate(),
            entry_id: EntryId::generate(),
            call_id: "call_1".to_owned(),
            cwd,
            cancel: InterruptSignal::new(),
            steering: InterruptSignal::new(),
            progress,
        }
    }

    fn text_of(output: &ToolOutput) -> String {
        output
            .content
            .iter()
            .map(|block| match block {
                zcode_agent::StoredToolResultContent::Text { text } => text.clone(),
                zcode_agent::StoredToolResultContent::Image { .. } => String::new(),
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[tokio::test]
    async fn finds_a_hit() {
        let dir = tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("main.rs"),
            "fn main() {\n    needle_here();\n}\n",
        )
        .expect("write file");
        let workspace = Arc::new(Workspace::new(dir.path().to_path_buf()));
        let tool = GrepTool::new(Arc::clone(&workspace), &test_config());

        let out = tool
            .execute(
                json!({ "pattern": "needle_here" }),
                test_ctx(dir.path().to_path_buf()),
            )
            .await
            .expect("grep must succeed");
        let text = text_of(&out);
        assert!(text.contains("main.rs"), "命中文件必须出现: {text}");
        assert!(text.contains("needle_here"), "命中行必须出现: {text}");
        assert!(!out.useless);
    }

    #[tokio::test]
    async fn reports_no_match() {
        let dir = tempdir().expect("tempdir");
        std::fs::write(dir.path().join("main.rs"), "fn main() {}\n").expect("write file");
        let workspace = Arc::new(Workspace::new(dir.path().to_path_buf()));
        let tool = GrepTool::new(Arc::clone(&workspace), &test_config());

        let out = tool
            .execute(
                json!({ "pattern": "nonexistent_needle" }),
                test_ctx(dir.path().to_path_buf()),
            )
            .await
            .expect("zero matches is a success, not an error");
        assert!(out.useless, "零命中必须标记 useless");
        assert!(text_of(&out).contains("没有匹配"));
    }

    #[tokio::test]
    async fn pagination_past_the_end_has_distinct_wording() {
        let dir = tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.rs"), "needle\n").expect("write a");
        std::fs::write(dir.path().join("b.rs"), "needle\n").expect("write b");
        let workspace = Arc::new(Workspace::new(dir.path().to_path_buf()));
        let tool = GrepTool::new(Arc::clone(&workspace), &test_config());

        let out = tool
            .execute(
                json!({ "pattern": "needle", "skip": 5 }),
                test_ctx(dir.path().to_path_buf()),
            )
            .await
            .expect("skip past the end is still a success");
        let text = text_of(&out);
        assert!(out.useless);
        assert!(
            text.contains("没有更多结果"),
            "越界翻页文案必须和零命中区分开: {text}"
        );
        assert!(
            text.contains("共 2 个文件"),
            "必须报告已知的文件总数: {text}"
        );
        assert!(text.contains("skip=5"), "必须回显请求的 skip 值: {text}");
    }

    #[tokio::test]
    async fn invalid_regex_degrades_to_literal_and_says_so() {
        let dir = tempdir().expect("tempdir");
        // `(` 后面没有匹配的 `)`，是一个非法的正则；但作为字面子串它出现在文件里。
        std::fs::write(
            dir.path().join("weird.txt"),
            "prefix (unterminated suffix\n",
        )
        .expect("write file");
        let workspace = Arc::new(Workspace::new(dir.path().to_path_buf()));
        let tool = GrepTool::new(Arc::clone(&workspace), &test_config());

        let out = tool
            .execute(
                json!({ "pattern": "(unterminated" }),
                test_ctx(dir.path().to_path_buf()),
            )
            .await
            .expect("非法正则必须退化为字面量而不是报错");
        let text = text_of(&out);
        assert!(text.contains("weird.txt"), "字面量匹配必须真的生效: {text}");
        assert!(
            text.contains("已退化为按字面量子串匹配"),
            "必须明确告知降级，不能静默: {text}"
        );
    }
}
