//! `ls` 工具：递归目录树，默认尊重 `.gitignore`。
//!
//! [`walk`] 是全仓唯一的目录遍历实现——`read` 工具的目录浅层摘要分支直接调用它（更小的
//! `max_depth`/`per_dir_limit`），不另写一份遍历逻辑，两者的目录遍历行为（`.gitignore`
//! 尊重、符号链接处理、排序）因此天然保持一致。

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};
use zcode_agent::{ApprovalDecision, Concurrency, Tier, Tool, ToolContext, ToolError, ToolOutput};

use crate::config::ToolsConfig;
use crate::tools::output;
use crate::tools::read::path_error_to_tool_error;
use crate::workspace::Workspace;

/// `ls` 默认最大递归深度。
///
/// 移植自 jcode `crates/jcode-app-core/src/tool/ls.rs:183`（递归条件 `depth < 5`）；
/// 本仓改用 `ignore::WalkBuilder::max_depth`，其语义是"根为深度 0，条目最大深度即该值"，
/// 直接传 5 就精确得到"根的 5 层子孙"，不必复刻 jcode 手写递归里那个容易读错的
/// 计数偏移。
const LS_MAX_DEPTH: usize = 5;

/// `ls` 默认总条目数上限。
///
/// 移植自 jcode `crates/jcode-app-core/src/tool/ls.rs:8`（`MAX_ENTRIES = 100`）。
const LS_MAX_ENTRIES: usize = 100;

/// 目录遍历参数。`read` 的浅层摘要分支与 `ls` 工具本体共用同一个 [`walk`]，各自传不同的值。
#[derive(Debug, Clone, Copy)]
pub(crate) struct WalkOptions {
    /// 最大递归深度；根自身深度为 0，直接子项深度为 1。
    pub(crate) max_depth: usize,
    /// 每个目录展开的直接子项上限，超出的子项（及其全部后代）被跳过。
    pub(crate) per_dir_limit: usize,
    /// 全部收集到的条目数总上限；达到后停止遍历。
    pub(crate) max_entries: usize,
    /// 是否尊重 `.gitignore`（含全局与本地 `.git/info/exclude`）以及隐藏文件过滤；
    /// 关闭后两者一起关（对应 `ls` 工具的 `all` 参数）。
    pub(crate) respect_gitignore: bool,
}

/// 一条遍历结果：相对根的路径、深度（根的直接子项 = 1）、是否目录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WalkEntry {
    /// 相对遍历根的路径。
    pub(crate) relative: PathBuf,
    /// 深度，根的直接子项为 1。
    pub(crate) depth: usize,
    /// 是否目录（符号链接按其指向的目标类型判定，见 [`entry_is_dir`]）。
    pub(crate) is_dir: bool,
}

/// [`walk`] 的结果。
#[derive(Debug, Clone)]
pub(crate) struct WalkResult {
    /// 收集到的条目，深度优先、每层按文件名排序。
    pub(crate) entries: Vec<WalkEntry>,
    /// 是否因 `max_entries` 或某个目录的 `per_dir_limit` 而被截断。
    pub(crate) truncated: bool,
}

/// 递归遍历 `root`，深度与每目录条目数受 `options` 限制。
///
/// 用 `ignore::WalkBuilder` 驱动（不自己写遍历器），默认尊重 `.gitignore`；符号链接不跟随
/// 递归（防环，`follow_links(false)`），但会 stat 目标以判断展示后缀是否该带 `/`
/// （opencode `packages/opencode/src/tool/read.ts:106-111`）。
///
/// # 已知限制
///
/// `per_dir_limit` 的裁剪只在收集结果时过滤：一个目录里排在上限之后的条目仍会被
/// 底层的串行 `Walk` 迭代器展开一次（`ignore` crate 只有并行遍历器支持 `filter_entry`
/// 剪枝下降）。深度与 `max_entries` 仍然给出硬上限，未剪枝的展开成本因此有界，但在
/// `respect_gitignore = false` 且目录分支极宽（例如未被 `.gitignore` 排除的
/// `node_modules`）时会比理论最优慢。这是明确接受的取舍，不是遗漏。
#[must_use]
pub(crate) fn walk(root: &Path, options: WalkOptions) -> WalkResult {
    let mut builder = ignore::WalkBuilder::new(root);
    builder
        .max_depth(Some(options.max_depth))
        .follow_links(false)
        .hidden(options.respect_gitignore)
        .ignore(options.respect_gitignore)
        .git_ignore(options.respect_gitignore)
        .git_global(options.respect_gitignore)
        .git_exclude(options.respect_gitignore)
        .sort_by_file_name(<std::ffi::OsStr as Ord>::cmp);

    let mut entries = Vec::new();
    let mut per_dir_count: HashMap<PathBuf, usize> = HashMap::new();
    let mut skip_roots: Vec<PathBuf> = Vec::new();
    let mut truncated = false;

    for result in builder.build() {
        let entry = match result {
            Ok(entry) => entry,
            Err(err) => {
                tracing::warn!(error = %err, "ls: 跳过一个无法访问的目录项");
                continue;
            }
        };
        let path = entry.path();
        if path == root {
            continue;
        }
        if entries.len() >= options.max_entries {
            truncated = true;
            break;
        }
        if skip_roots.iter().any(|skipped| path.starts_with(skipped)) {
            continue;
        }
        let Some(parent) = path.parent() else {
            continue;
        };
        let Ok(relative) = path.strip_prefix(root) else {
            continue;
        };

        let count = per_dir_count.entry(parent.to_path_buf()).or_insert(0);
        let is_dir = entry_is_dir(&entry);
        if *count >= options.per_dir_limit {
            truncated = true;
            if is_dir {
                skip_roots.push(path.to_path_buf());
            }
            continue;
        }
        *count += 1;

        entries.push(WalkEntry {
            relative: relative.to_path_buf(),
            depth: entry.depth(),
            is_dir,
        });
    }

    WalkResult { entries, truncated }
}

/// 判定一条遍历结果是否应按目录展示：普通目录直接判定；符号链接 stat 目标类型
/// （目标不可达时按非目录处理）。
fn entry_is_dir(entry: &ignore::DirEntry) -> bool {
    let Some(file_type) = entry.file_type() else {
        return false;
    };
    if file_type.is_symlink() {
        return std::fs::metadata(entry.path()).is_ok_and(|meta| meta.is_dir());
    }
    file_type.is_dir()
}

/// 把 [`WalkResult`] 渲染成缩进树文本：目录后缀 `/`，末尾附条目统计与截断提示。
/// `read` 的目录分支与 `ls` 工具本体共用这份渲染，行为保证一致。
#[must_use]
pub(crate) fn render(display_root: &str, result: &WalkResult) -> String {
    let mut body = format!("{display_root}/\n");

    if result.entries.is_empty() {
        body.push_str("(空目录)");
        return body;
    }

    let mut file_count = 0_usize;
    let mut dir_count = 0_usize;
    for entry in &result.entries {
        let indent = "  ".repeat(entry.depth.saturating_sub(1));
        let name = entry
            .relative
            .file_name()
            .map_or_else(|| entry.relative.to_string_lossy(), |n| n.to_string_lossy());
        let suffix = if entry.is_dir { "/" } else { "" };
        let _ = writeln!(body, "{indent}{name}{suffix}");
        if entry.is_dir {
            dir_count += 1;
        } else {
            file_count += 1;
        }
    }

    if result.truncated {
        let _ = write!(
            body,
            "\n(已截断：最多显示 {LS_MAX_ENTRIES} 条，每个目录最多展开若干条子项)\n"
        );
    }
    let _ = write!(body, "\n{file_count} 个文件，{dir_count} 个目录");
    body
}

/// `ls` 工具。
#[derive(Debug)]
pub(crate) struct LsTool {
    workspace: Arc<Workspace>,
}

impl LsTool {
    /// 构造 `ls` 工具。当前不需要 `config` 里的任何字段，但按统一构造签名收下。
    pub(crate) fn new(workspace: Arc<Workspace>, _config: &ToolsConfig) -> Self {
        Self { workspace }
    }
}

#[async_trait]
impl Tool for LsTool {
    fn name(&self) -> &'static str {
        "ls"
    }

    fn description(&self) -> &'static str {
        include_str!("./prompts/ls.md")
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "目录路径，默认为当前工作目录。"
                },
                "all": {
                    "type": "boolean",
                    "description": "为 true 时忽略 .gitignore 并列出隐藏条目；默认 false，即遵守 .gitignore。"
                }
            },
            "additionalProperties": false
        })
    }

    fn approval(&self, args: &Value) -> ApprovalDecision {
        let raw = args.get("path").and_then(Value::as_str).unwrap_or(".");
        match self.workspace.resolve(raw) {
            // 与 read 工具同一套理由：越界列目录同样是"读取任意系统路径"，抬高一档。
            Ok(resolved) if resolved.outside_root => ApprovalDecision::tier(Tier::Write),
            _ => ApprovalDecision::tier(Tier::Read),
        }
    }

    fn concurrency(&self, _args: &Value) -> Concurrency {
        Concurrency::Shared
    }

    async fn execute(&self, args: Value, _ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        let raw_path = args.get("path").and_then(Value::as_str).unwrap_or(".");
        let all = args.get("all").and_then(Value::as_bool).unwrap_or(false);

        let resolved = self
            .workspace
            .resolve(raw_path)
            .map_err(|err| path_error_to_tool_error(&err))?;
        let display_path = self.workspace.display(&resolved.path);

        let metadata = tokio::fs::metadata(&resolved.path)
            .await
            .map_err(|err| output::error(format!("无法访问 '{display_path}': {err}")))?;
        if !metadata.is_dir() {
            return Err(output::error(format!("'{display_path}' 不是目录。")));
        }

        let root = resolved.path.clone();
        let result = tokio::task::spawn_blocking(move || {
            walk(
                &root,
                WalkOptions {
                    max_depth: LS_MAX_DEPTH,
                    per_dir_limit: LS_MAX_ENTRIES,
                    max_entries: LS_MAX_ENTRIES,
                    respect_gitignore: !all,
                },
            )
        })
        .await
        .map_err(|err| output::error(format!("目录遍历任务失败: {err}")))?;

        let body = render(&display_path, &result);
        Ok(output::finish(body, format!("目录 {display_path}")))
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    fn write_file(path: &Path) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("创建父目录失败");
        }
        std::fs::write(path, b"x").expect("写入测试文件失败");
    }

    #[test]
    fn respects_max_depth() {
        let dir = tempdir().expect("tempdir 创建失败");
        let root = dir.path();
        write_file(&root.join("a/b/c/d/e/f/too_deep.txt"));
        write_file(&root.join("a/shallow.txt"));

        let result = walk(
            root,
            WalkOptions {
                max_depth: 2,
                per_dir_limit: 100,
                max_entries: 1000,
                respect_gitignore: false,
            },
        );

        let deepest = result.entries.iter().map(|e| e.depth).max().unwrap_or(0);
        assert!(deepest <= 2, "深度应被限制在 2 层以内，实际最深 {deepest}");
        assert!(
            result
                .entries
                .iter()
                .any(|e| e.relative == Path::new("a/shallow.txt"))
        );
        assert!(
            !result
                .entries
                .iter()
                .any(|e| e.relative.ends_with("too_deep.txt"))
        );
    }

    #[test]
    fn respects_max_entries_cap() {
        let dir = tempdir().expect("tempdir 创建失败");
        let root = dir.path();
        for i in 0..20 {
            write_file(&root.join(format!("file{i}.txt")));
        }

        let result = walk(
            root,
            WalkOptions {
                max_depth: 5,
                per_dir_limit: 100,
                max_entries: 5,
                respect_gitignore: false,
            },
        );

        assert_eq!(result.entries.len(), 5);
        assert!(result.truncated);
    }

    #[test]
    fn respects_per_dir_limit() {
        let dir = tempdir().expect("tempdir 创建失败");
        let root = dir.path();
        for i in 0..10 {
            write_file(&root.join(format!("file{i}.txt")));
        }

        let result = walk(
            root,
            WalkOptions {
                max_depth: 5,
                per_dir_limit: 3,
                max_entries: 1000,
                respect_gitignore: false,
            },
        );

        assert_eq!(result.entries.len(), 3);
        assert!(result.truncated);
    }

    #[test]
    fn directory_entries_get_trailing_slash() {
        let dir = tempdir().expect("tempdir 创建失败");
        let root = dir.path();
        write_file(&root.join("sub/file.txt"));

        let result = walk(
            root,
            WalkOptions {
                max_depth: 5,
                per_dir_limit: 100,
                max_entries: 1000,
                respect_gitignore: false,
            },
        );
        let rendered = render(".", &result);
        assert!(rendered.contains("sub/\n"));
        assert!(rendered.contains("file.txt\n"));
    }

    #[test]
    fn render_reports_empty_directory() {
        let result = WalkResult {
            entries: Vec::new(),
            truncated: false,
        };
        let rendered = render(".", &result);
        assert!(rendered.contains("空目录"));
    }
}
