//! `glob` 工具：按 glob 模式在一个或多个目录下列出匹配的文件与目录。
//!
//! 遍历引擎是 `ignore::WalkBuilder` + `ignore::overrides::Override`——与
//! `zcode_text::grep` 内部用来编译 `include_globs`/`exclude_globs` 的是同一套机制
//! （见 `crates/text/src/grep.rs:453-479` 的 `build_overrides`），因此 `glob` 与
//! `grep` 对"什么算隐藏文件""什么算被 gitignore 排除"给出一致答案，不需要再造第二套
//! 遍历规则。glob 模式语法因此是 gitignore-glob（不带 `/` 的模式匹配任意深度），
//! 不是 shell glob——这是复用引擎换来的代价，已写进 `prompts/glob.md`。

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};

use async_trait::async_trait;
use ignore::WalkBuilder;
use ignore::overrides::OverrideBuilder;
use serde_json::{Value, json};
use zcode_agent::{ApprovalDecision, Concurrency, Tier, Tool, ToolContext, ToolError, ToolOutput};

use crate::config::ToolsConfig;
use crate::tools::output;
use crate::workspace::{PathError, Workspace};

/// glob 工具描述（发给模型的文本）。
const DESCRIPTION: &str = include_str!("prompts/glob.md");

/// 单次调用最多返回的文件数，同时也是未显式传 `limit` 时的默认值。
///
/// 抄自 oh-my-pi `packages/coding-agent/src/tools/glob.ts:54-55`
/// （`DEFAULT_LIMIT = MAX_LIMIT = 200`）；上游未给出这个数字本身的定量依据，只是经验值。
/// `limit` 参数只能把它往下调，不能突破——防止一次意外的大 `limit` 把整棵树的文件名灌回模型。
const DEFAULT_LIMIT: usize = 200;
/// 见 [`DEFAULT_LIMIT`]。
const MAX_LIMIT: usize = 200;

/// glob 遍历的墙钟超时。
///
/// 抄自 oh-my-pi `packages/coding-agent/src/tools/glob.ts:56`
/// （`DEFAULT_GLOB_TIMEOUT_MS = 5000`）；上游同样未给出该数字的定量依据，只是经验值，
/// 作用是"遍历卡在巨型目录树/网络挂载时的安全阀"，不是精确调优结果。
const GLOB_TIMEOUT: Duration = Duration::from_secs(5);

/// 一次匹配到的文件或目录，携带排序要用的 mtime。
struct GlobHit {
    /// 匹配到的绝对路径。
    path: PathBuf,
    /// 是否是目录（展示时补一个尾部 `/`）。
    is_dir: bool,
    /// 修改时间，用于按新旧排序。
    mtime: SystemTime,
}

/// 一次目录树遍历的结果。
struct WalkOutcome {
    /// 收集到的命中。
    hits: Vec<GlobHit>,
    /// 是否因触达 [`GLOB_TIMEOUT`] 而提前结束。
    timed_out: bool,
}

/// [`GlobTool::execute`] 解析完参数后的结构化视图。
struct ParsedArgs<'a> {
    /// glob 模式（已去除首尾空白，非空）。
    pattern: String,
    /// `path` 字段原文，未拆分；`None` 表示模型没传，等价于 `"."`。
    raw_path: &'a str,
    /// 是否包含隐藏文件。
    include_hidden: bool,
    /// 是否遵守 `.gitignore`。
    respect_gitignore: bool,
    /// 钳到 `[1, MAX_LIMIT]` 之后的结果数上限。
    effective_limit: usize,
}

/// 解析并校验 `glob` 工具的入参；不碰文件系统。
fn parse_args(args: &Value) -> Result<ParsedArgs<'_>, ToolError> {
    let pattern = args
        .get("pattern")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| output::error("`pattern` 不能为空"))?
        .to_owned();
    let raw_path = args.get("path").and_then(Value::as_str).unwrap_or(".");
    let include_hidden = args.get("hidden").and_then(Value::as_bool).unwrap_or(true);
    let respect_gitignore = args
        .get("gitignore")
        .and_then(Value::as_bool)
        .unwrap_or(true);

    let requested_limit = match args.get("limit") {
        None | Some(Value::Null) => None,
        Some(value) => {
            let n = value
                .as_u64()
                .ok_or_else(|| output::error("`limit` 必须是正整数"))?;
            if n == 0 {
                return Err(output::error("`limit` 必须是正整数"));
            }
            Some(usize::try_from(n).map_err(|_| output::error("`limit` 超出可表示范围"))?)
        }
    };

    Ok(ParsedArgs {
        pattern,
        raw_path,
        include_hidden,
        respect_gitignore,
        effective_limit: clamp_limit(requested_limit),
    })
}

/// 把 `path` 字段拆成搜索根：解析、根目录防护、存在性探测都在这里做完。
///
/// 多路径调用里不存在的基目录被跳过（返回在第二个元素里，供调用方展示）；单路径调用保持
/// 原 ENOENT 语义，直接失败——用户明确问了那一个，静默返回空结果是误导（见模块任务文档）。
async fn resolve_roots(
    workspace: &Workspace,
    raw_path: &str,
) -> Result<(Vec<PathBuf>, Vec<String>), ToolError> {
    let raw_entries: Vec<&str> = raw_path
        .split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    let raw_entries: Vec<&str> = if raw_entries.is_empty() {
        vec!["."]
    } else {
        raw_entries
    };
    let is_single = raw_entries.len() == 1;

    let mut roots: Vec<PathBuf> = Vec::with_capacity(raw_entries.len());
    let mut missing: Vec<String> = Vec::new();
    for raw in raw_entries {
        let aliased = normalize_root_alias(raw);
        let resolved = workspace
            .resolve(aliased)
            .map_err(|err| output::error(describe_path_error(&err, raw)))?;
        if is_filesystem_root(&resolved.path) {
            return Err(output::error(
                "不允许从文件系统根目录搜索，请指定更具体的子目录",
            ));
        }
        if tokio::fs::metadata(&resolved.path).await.is_err() {
            if is_single {
                return Err(output::error(format!(
                    "路径不存在：{}",
                    workspace.display(&resolved.path)
                )));
            }
            missing.push(raw.to_owned());
            continue;
        }
        roots.push(resolved.path);
    }
    if roots.is_empty() {
        return Err(output::error(format!("路径不存在：{}", missing.join(", "))));
    }
    Ok((roots, missing))
}

/// 把「纯斜杠」输入（`/`、`//`、……）重写成 `.`。
///
/// 用户常用前导 `/` 表达"从这里开始搜"，而不是真的想扫描整个文件系统根
/// （oh-my-pi `glob.ts:167-172` + `path-utils.ts:509-520` 的用户习惯依据）。真正的根目录
/// 逃逸由 [`is_filesystem_root`] 在路径解析之后统一兜底——本仓只保留这一处判定，
/// 不像上游把同一语义拆成三处分别写。
fn normalize_root_alias(raw: &str) -> &str {
    if !raw.is_empty() && raw.bytes().all(|b| b == b'/') {
        "."
    } else {
        raw
    }
}

/// 一个已解析的绝对路径是否恰好是文件系统根（或 Windows 的盘符根）。
///
/// 根路径没有父路径，这是 `std::path` 跨平台都成立的性质，不需要按 `cfg(windows)` 分别判断。
fn is_filesystem_root(path: &Path) -> bool {
    path.parent().is_none()
}

/// 把用户传入的 `limit` 钳到 `[1, MAX_LIMIT]`；未传时取 [`DEFAULT_LIMIT`]。
fn clamp_limit(requested: Option<usize>) -> usize {
    requested.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT)
}

/// 把 [`PathError`] 翻译成面向模型的中文说明。
fn describe_path_error(err: &PathError, raw: &str) -> String {
    match err {
        PathError::Empty => "路径不能为空".to_owned(),
        PathError::NotUtf8 => format!("路径 `{raw}` 归一化后不是合法 UTF-8"),
    }
}

/// 在单个根目录下按 `pattern` 遍历，返回命中列表与是否超时。
fn walk_one(
    root: &Path,
    pattern: &str,
    include_hidden: bool,
    respect_gitignore: bool,
    cancel: &AtomicBool,
    deadline: Instant,
) -> Result<WalkOutcome, ToolError> {
    let mut override_builder = OverrideBuilder::new(root);
    override_builder
        .add(pattern)
        .map_err(|source| output::error(format!("glob 模式 `{pattern}` 无效：{source}")))?;
    let overrides = override_builder
        .build()
        .map_err(|source| output::error(format!("glob 模式 `{pattern}` 无效：{source}")))?;

    let mut walk_builder = WalkBuilder::new(root);
    walk_builder
        .hidden(!include_hidden)
        .parents(respect_gitignore)
        .ignore(respect_gitignore)
        .git_ignore(respect_gitignore)
        .git_global(respect_gitignore)
        .git_exclude(respect_gitignore)
        // 任意目录树里都要生效，不要求存在真实 `.git`（否则从压缩包/worktree 拷贝出来的
        // 项目会静默失去 gitignore 支持）——与 `zcode_text::grep::grep` 的选择一致。
        .require_git(false)
        .overrides(overrides);

    let mut hits = Vec::new();
    let mut timed_out = false;
    for entry in walk_builder.build() {
        if cancel.load(Ordering::Acquire) {
            break;
        }
        if Instant::now() >= deadline {
            timed_out = true;
            break;
        }
        let Ok(entry) = entry else {
            // 单条遍历错误不致命（权限拒绝、竞态删除等），跳过继续，
            // 与 `zcode_text::grep::handle_walk_error` 对非致命错误的处理方向一致。
            continue;
        };
        if entry.depth() == 0 {
            // 根条目自身：它的相对路径是空串，正常不会匹配任何 glob，这里显式跳过更清楚。
            continue;
        }
        let is_dir = entry.file_type().is_some_and(|ft| ft.is_dir());
        let mtime = entry
            .metadata()
            .ok()
            .and_then(|meta| meta.modified().ok())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        hits.push(GlobHit {
            path: entry.into_path(),
            is_dir,
            mtime,
        });
    }
    Ok(WalkOutcome { hits, timed_out })
}

/// 依次遍历每个目标根目录；根之间不共享 walker（见模块文档）。
fn walk_all(
    roots: &[PathBuf],
    pattern: &str,
    include_hidden: bool,
    respect_gitignore: bool,
    cancel: &AtomicBool,
    deadline: Instant,
) -> Result<WalkOutcome, ToolError> {
    let mut hits = Vec::new();
    let mut timed_out = false;
    for root in roots {
        if cancel.load(Ordering::Acquire) || Instant::now() >= deadline {
            timed_out = true;
            break;
        }
        let mut outcome = walk_one(
            root,
            pattern,
            include_hidden,
            respect_gitignore,
            cancel,
            deadline,
        )?;
        hits.append(&mut outcome.hits);
        timed_out = timed_out || outcome.timed_out;
    }
    Ok(WalkOutcome { hits, timed_out })
}

/// 把遍历结果渲染成回给模型的正文；空结果与非空结果的措辞在这里分流。
fn render_hits(
    hits: Vec<GlobHit>,
    timed_out: bool,
    effective_limit: usize,
    missing: &[String],
    workspace: &Workspace,
    pattern: &str,
) -> ToolOutput {
    let missing_note =
        (!missing.is_empty()).then(|| format!("已跳过不存在的路径：{}", missing.join(", ")));

    if hits.is_empty() {
        let mut lines = Vec::new();
        if timed_out {
            // 超时的空结果是扫描未完成，不是"验证过没有匹配"——两句话互相矛盾，不能同时说。
            lines.push(format!(
                "glob 扫描在 {:.1}s 内未完成，结果不完整——不代表没有匹配文件，请缩小 path 后重试",
                GLOB_TIMEOUT.as_secs_f64()
            ));
        } else {
            lines.push("没有找到匹配的文件".to_owned());
        }
        lines.extend(missing_note);
        return output::finish(lines.join("\n"), format!("glob {pattern}")).mark_useless();
    }

    let total = hits.len();
    let limited: Vec<GlobHit> = hits.into_iter().take(effective_limit).collect();

    let mut body = String::new();
    for hit in &limited {
        let display = workspace.display(&hit.path);
        body.push_str(&display);
        if hit.is_dir && !display.ends_with('/') {
            body.push('/');
        }
        body.push('\n');
    }

    let mut trailer = Vec::new();
    if total > limited.len() {
        trailer.push(format!(
            "仅显示前 {} 个结果（按修改时间倒序；共找到至少 {total} 个），缩小 pattern 或 path 可看到更相关的文件",
            limited.len()
        ));
    }
    if timed_out {
        trailer.push(format!(
            "glob 扫描在 {:.1}s 内未完成，以上结果不完整——不代表没有更多匹配文件",
            GLOB_TIMEOUT.as_secs_f64()
        ));
    }
    trailer.extend(missing_note);
    if !trailer.is_empty() {
        body.push('\n');
        body.push_str(&trailer.join("\n"));
    }

    output::finish(body, format!("glob {pattern}"))
}

/// `glob` 内置工具：按 glob 模式在一个或多个目录下列出匹配文件/目录。
#[derive(Debug)]
pub(crate) struct GlobTool {
    /// 工作区路径解析器。
    workspace: Arc<Workspace>,
}

impl GlobTool {
    /// 构造。`config` 当前不影响 glob 行为，仍按统一的工具构造签名接收。
    pub(crate) fn new(workspace: Arc<Workspace>, _config: &ToolsConfig) -> Self {
        Self { workspace }
    }
}

#[async_trait]
impl Tool for GlobTool {
    fn name(&self) -> &'static str {
        "glob"
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
                    "description": "gitignore 风格的 glob 模式，如 `**/*.rs`"
                },
                "path": {
                    "type": "string",
                    "description": "要搜索的一个或多个目录，用 `;` 分隔多个目标；省略时搜索工作区根目录"
                },
                "hidden": {
                    "type": "boolean",
                    "description": "是否包含点前缀的隐藏文件，默认 true"
                },
                "gitignore": {
                    "type": "boolean",
                    "description": "是否遵守 .gitignore，默认 true"
                },
                "limit": {
                    "type": "number",
                    "description": "最多返回的结果数，默认与上限都是 200，只能往下调"
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
        let (roots, missing) = resolve_roots(&self.workspace, parsed.raw_path).await?;

        // 硬取消桥接：InterruptSignal 只能异步等待，walk_all 在 spawn_blocking 里同步运行，
        // 用一个共享 AtomicBool 把"取消已到达"这件事从异步侧搬进同步侧，walker 每条 entry
        // 前检查一次即可及时打断——与 grep.rs 的取消桥接是同一套写法，各自成文件不算重复约定。
        let cancel_flag = Arc::new(AtomicBool::new(ctx.cancel.is_set()));
        let watcher_flag = Arc::clone(&cancel_flag);
        let watcher_signal = ctx.cancel.clone();
        let watcher = tokio::spawn(async move {
            watcher_signal.notified().await;
            watcher_flag.store(true, Ordering::Release);
        });

        let deadline = Instant::now() + GLOB_TIMEOUT;
        let walk_pattern = parsed.pattern.clone();
        let walk_cancel = Arc::clone(&cancel_flag);
        let include_hidden = parsed.include_hidden;
        let respect_gitignore = parsed.respect_gitignore;
        let joined = tokio::task::spawn_blocking(move || {
            walk_all(
                &roots,
                &walk_pattern,
                include_hidden,
                respect_gitignore,
                &walk_cancel,
                deadline,
            )
        })
        .await;
        watcher.abort();

        let outcome = joined.map_err(|_| output::error("glob 遍历任务异常终止"))??;

        if ctx.cancel.is_set() {
            return Err(ToolError::Cancelled);
        }

        let mut hits = outcome.hits;
        hits.sort_by(|a, b| b.mtime.cmp(&a.mtime).then_with(|| a.path.cmp(&b.path)));
        let mut seen = HashSet::new();
        hits.retain(|hit| seen.insert(hit.path.clone()));

        Ok(render_hits(
            hits,
            outcome.timed_out,
            parsed.effective_limit,
            &missing,
            &self.workspace,
            &parsed.pattern,
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::thread::sleep;

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

    #[test]
    fn pure_slash_inputs_are_rewritten_not_rejected() {
        assert_eq!(normalize_root_alias("/"), ".");
        assert_eq!(normalize_root_alias("//"), ".");
        assert_eq!(normalize_root_alias("///"), ".");
        assert_eq!(normalize_root_alias("src"), "src");
        assert_eq!(normalize_root_alias(""), "");
    }

    #[test]
    fn filesystem_root_has_no_parent() {
        assert!(is_filesystem_root(Path::new("/")));
        assert!(!is_filesystem_root(Path::new("/etc")));
        assert!(!is_filesystem_root(Path::new("relative")));
    }

    #[test]
    fn limit_can_only_be_lowered() {
        assert_eq!(clamp_limit(None), DEFAULT_LIMIT);
        assert_eq!(clamp_limit(Some(5)), 5);
        assert_eq!(clamp_limit(Some(10_000)), MAX_LIMIT);
        assert_eq!(clamp_limit(Some(1)), 1);
    }

    #[tokio::test]
    async fn multi_path_skips_missing_and_lists_them() {
        let dir = tempdir().expect("tempdir");
        std::fs::write(dir.path().join("keep.rs"), "fn main() {}").expect("write file");
        let workspace = Arc::new(Workspace::new(dir.path().to_path_buf()));
        let tool = GlobTool::new(Arc::clone(&workspace), &test_config());

        let args = json!({ "pattern": "*.rs", "path": "does-not-exist; ." });
        let out = tool
            .execute(args, test_ctx(dir.path().to_path_buf()))
            .await
            .expect("multi-path call with one missing base dir must succeed");
        let text = text_of(&out);
        assert!(
            text.contains("keep.rs"),
            "命中的文件必须出现在结果里: {text}"
        );
        assert!(
            text.contains("已跳过不存在的路径"),
            "缺失的目标必须被列出: {text}"
        );
        assert!(
            text.contains("does-not-exist"),
            "缺失路径名要出现在提示里: {text}"
        );
    }

    #[tokio::test]
    async fn single_missing_path_is_a_hard_error() {
        let dir = tempdir().expect("tempdir");
        let workspace = Arc::new(Workspace::new(dir.path().to_path_buf()));
        let tool = GlobTool::new(Arc::clone(&workspace), &test_config());

        let args = json!({ "pattern": "*.rs", "path": "does-not-exist" });
        let err = tool
            .execute(args, test_ctx(dir.path().to_path_buf()))
            .await
            .expect_err("单路径缺失必须保持 ENOENT 语义，不能静默返回空结果");
        assert!(matches!(err, ToolError::Failed(msg) if msg.contains("路径不存在")));
    }

    #[tokio::test]
    async fn empty_result_is_marked_useless() {
        let dir = tempdir().expect("tempdir");
        let workspace = Arc::new(Workspace::new(dir.path().to_path_buf()));
        let tool = GlobTool::new(Arc::clone(&workspace), &test_config());

        let args = json!({ "pattern": "*.nonexistent-ext" });
        let out = tool
            .execute(args, test_ctx(dir.path().to_path_buf()))
            .await
            .expect("zero matches is a success, not an error");
        assert!(out.useless, "零结果必须标记 useless");
        assert!(text_of(&out).contains("没有找到"));
    }

    #[tokio::test]
    async fn results_are_sorted_newest_first() {
        let dir = tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.txt"), "a").expect("write a");
        sleep(Duration::from_millis(50));
        std::fs::write(dir.path().join("b.txt"), "b").expect("write b");
        sleep(Duration::from_millis(50));
        std::fs::write(dir.path().join("c.txt"), "c").expect("write c");

        let workspace = Arc::new(Workspace::new(dir.path().to_path_buf()));
        let tool = GlobTool::new(Arc::clone(&workspace), &test_config());
        let args = json!({ "pattern": "*.txt" });
        let out = tool
            .execute(args, test_ctx(dir.path().to_path_buf()))
            .await
            .expect("glob must succeed");
        let text = text_of(&out);
        let pos_a = text.find("a.txt").expect("a.txt present");
        let pos_b = text.find("b.txt").expect("b.txt present");
        let pos_c = text.find("c.txt").expect("c.txt present");
        assert!(
            pos_c < pos_b && pos_b < pos_a,
            "必须按 mtime 倒序（最新在前）: {text}"
        );
    }
}
