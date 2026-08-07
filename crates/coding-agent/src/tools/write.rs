//! `write` 工具：整份覆写（或新建）一个文件。
//!
//! # tmp + rename
//!
//! 直接 `fs::write` 到目标路径，进程崩溃或磁盘写满时会留下一个内容截断的文件，原内容彻底
//! 丢失且无法区分"写了一半"与"本来就这么短"。本工具总是先把新内容写进同目录下的一个临时
//! 文件，成功后再 `rename` 到目标名——`rename` 在同一文件系统内是原子操作，中途失败时目标
//! 文件要么是旧内容、要么是新内容，不会是两者的混合，临时文件失败时也会尽力清理掉。
//!
//! # 符号链接：先解到真实路径再写
//!
//! `rename(tmp, target)` 当 `target` 是符号链接时，替换的是链接本身（把它变成一个指向
//! `tmp` 原内容的普通文件），而不是"写穿"到链接指向的真实文件——这正是 tmp+rename 方案
//! 在符号链接场景下的已知陷阱（oh-my-pi `write.ts:636-641` 记录的同一个教训）。
//! [`resolve_symlink_target`] 在写入前把目标解析成真实路径，`tmp` 与 `rename` 都对着真实
//! 路径操作；解不出真实路径（悬空链接）时退回链接路径本身——这时候"写穿"没有意义（目标
//! 根本不存在），只能接受链接被替换成普通文件这个后果，并在输出里如实说明。
//!
//! # 读选择器误触防护
//!
//! 复用 oh-my-pi 的真实故障模式（`write.ts:133-161`）：一个本该调 `read` 的步骤被误路由成
//! `write`，把 `read` 的 `path:selector` 表达式整串塞进了 `write` 的 `path` 参数。冒号在
//! 大多数文件系统上是合法文件名字符，`content` 又恰好是空串（`read` 从不填 `content`），
//! 于是会静默创建一个以选择器结尾命名的零字节文件——模型既意识不到写错了，也没法从一个
//! 空文件恢复。[`guard_read_selector_misfire`] 只在最贴合这个故障特征的组合上触发：目标
//! 以能解析成 `read` 选择器的后缀结尾、`content` 为空、且磁盘上确实没有这个字面文件名。
//! 非空 `content` 是明确的逃生口——真心实意创建一个"形似选择器"文件名的写入永远不会被拦。

use std::ffi::OsString;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use zcode_agent::{ApprovalDecision, Concurrency, Tier, Tool, ToolContext, ToolError, ToolOutput};

use crate::config::ToolsConfig;
use crate::tools::edit::{DIFF_CONTEXT_LINES, compact_diff};
use crate::tools::output;
use crate::tools::read::{parse_selector, split_path_and_selector};
use crate::workspace::Workspace;

/// `write` 的参数：整份写入文件内容。
#[derive(Debug, Deserialize)]
struct WriteArgs {
    /// 目标文件路径。
    path: String,
    /// 要写入的完整内容，整体覆盖已有内容。
    content: String,
}

/// 目标路径相对符号链接的解析结果。
#[derive(Debug)]
pub(crate) struct SymlinkResolution {
    /// 实际应该执行 tmp+rename 的路径：是符号链接就是它解出的真实路径，否则就是原路径。
    pub(crate) path: PathBuf,
    /// 原路径当前是否是一个符号链接。
    pub(crate) was_symlink: bool,
    /// `was_symlink` 为真时，是否成功解出了真实路径（悬空链接解不出）。
    pub(crate) resolved: bool,
}

/// `write` 工具：tmp+rename 原子写入，自动建父目录、写穿符号链接、拦截读选择器误触。
#[derive(Debug)]
pub(crate) struct WriteTool {
    workspace: Arc<Workspace>,
}

impl WriteTool {
    /// 装配期构造。`config` 目前没有 write 专属选项，只是为了保持八个内置工具统一的
    /// 构造签名（`ToolsRegistry` 的既定约定）。
    pub(crate) fn new(workspace: Arc<Workspace>, _config: &ToolsConfig) -> Self {
        Self { workspace }
    }
}

#[async_trait]
impl Tool for WriteTool {
    fn name(&self) -> &'static str {
        "write"
    }

    fn description(&self) -> &'static str {
        include_str!("./prompts/write.md")
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to write (relative to the workspace root, or absolute)."
                },
                "content": {
                    "type": "string",
                    "description": "The full content to write to the file. Overwrites the entire existing file, if any."
                }
            },
            "required": ["path", "content"],
            "additionalProperties": false
        })
    }

    fn approval(&self, _args: &Value) -> ApprovalDecision {
        ApprovalDecision::tier(Tier::Write)
    }

    fn concurrency(&self, _args: &Value) -> Concurrency {
        Concurrency::Exclusive
    }

    /// 单次 fs 读 + tmp/rename，耗时在毫秒级，来不及也没必要响应软/硬取消。
    async fn execute(&self, args: Value, _ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        let args: WriteArgs = serde_json::from_value(args)
            .map_err(|err| output::error(format!("参数解析失败：{err}")))?;

        let resolved = self
            .workspace
            .resolve(&args.path)
            .map_err(|err| output::error(format!("路径解析失败：{err}")))?;
        let display = self.workspace.display(&resolved.path);

        guard_read_selector_misfire(&args.path, &args.content, &resolved.path).await?;

        let old_bytes = match tokio::fs::read(&resolved.path).await {
            Ok(bytes) => Some(bytes),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
            Err(err) => return Err(output::error(format!("读取既有文件 {display} 失败：{err}"))),
        };

        let resolution = resolve_symlink_target(&resolved.path).await;
        write_atomic(&resolution.path, args.content.as_bytes())
            .await
            .map_err(|err| {
                output::error(format!(
                    "写入 {} 失败：{err}",
                    self.workspace.display(&resolution.path)
                ))
            })?;

        let body = success_body(
            &display,
            &resolution,
            &self.workspace,
            old_bytes.as_deref(),
            &args.content,
        );
        let title = if old_bytes.is_some() {
            format!("覆写 {display}")
        } else {
            format!("新建 {display}")
        };
        Ok(output::finish(body, title))
    }
}

/// oh-my-pi write.ts:150-203 记录的真实故障模式：目标以合法 read 选择器结尾、`content`
/// 为空、且字面文件名在磁盘上确实不存在时拒绝创建，提示改用 `read`。非空 `content` 永远
/// 放行；选择器语法都解析不出来（大概率是字面带冒号的文件名）也放行。
async fn guard_read_selector_misfire(
    raw_path: &str,
    content: &str,
    resolved_path: &Path,
) -> Result<(), ToolError> {
    if !content.is_empty() {
        return Ok(());
    }
    let Some(selector) = split_path_and_selector(raw_path).1 else {
        return Ok(());
    };
    if parse_selector(selector).is_err() {
        return Ok(());
    }
    if !is_definitely_missing(resolved_path).await {
        return Ok(());
    }
    Err(output::error(format!(
        "write 的目标 '{raw_path}' 以一个能解析成 read 选择器的后缀 ':{selector}' 结尾，且磁盘上不存在\
这个字面文件名——拒绝把它当成新文件名静默创建。如果你是想读取这段内容，改用 read（例如 \
read(path=\"{raw_path}\")）。如果确实要创建这个（形似选择器的）文件名，把内容放进 content 参数——\
非空 content 永远不会被这条防护拦截。"
    )))
}

/// 字面路径（不解析符号链接目标，只看这个名字本身是否存在）是否确定不存在。
/// 权限被拒、I/O 抖动等模糊情形一律当"存在"处理，绝不因为探测本身失败就误伤一个真实文件。
async fn is_definitely_missing(path: &Path) -> bool {
    match tokio::fs::symlink_metadata(path).await {
        Ok(_) => false,
        Err(err) => err.kind() == std::io::ErrorKind::NotFound,
    }
}

/// 写入前把符号链接解到真实路径：`rename(tmp, target)` 直接落在链接名上会把链接本身
/// 替换成普通文件，而不是写穿到目标。悬空链接解不出真实路径时退回链接本身——目标不存在，
/// 没有"写穿"可言，落在原地是唯一能继续执行的选择。
pub(crate) async fn resolve_symlink_target(path: &Path) -> SymlinkResolution {
    match tokio::fs::symlink_metadata(path).await {
        Ok(meta) if meta.file_type().is_symlink() => match tokio::fs::canonicalize(path).await {
            Ok(real) => SymlinkResolution {
                path: real,
                was_symlink: true,
                resolved: true,
            },
            Err(_) => SymlinkResolution {
                path: path.to_path_buf(),
                was_symlink: true,
                resolved: false,
            },
        },
        _ => SymlinkResolution {
            path: path.to_path_buf(),
            was_symlink: false,
            resolved: false,
        },
    }
}

/// 目标是符号链接时给输出附一句说明；不是符号链接时返回 `None`。`write` 与 `edit` 共用，
/// 保证两个工具在这件事上的措辞一致。
pub(crate) fn symlink_note(
    display: &str,
    resolution: &SymlinkResolution,
    workspace: &Workspace,
) -> Option<String> {
    if !resolution.was_symlink {
        return None;
    }
    Some(if resolution.resolved {
        format!(
            "{display} 是符号链接，已写穿到真实路径 {}（tmp+rename 若直接落在链接名上会把链接本身换成\
普通文件）。",
            workspace.display(&resolution.path)
        )
    } else {
        format!(
            "{display} 是指向不存在目标的悬空符号链接，已在链接路径本身创建普通文件，原链接已被替换。"
        )
    })
}

/// tmp + rename 原子写入：先建父目录，写到同目录下的临时文件，成功后 rename 到目标名；
/// 任一步失败都会尽力清理掉临时文件再把错误上抛。`write` 与 `edit` 共用这个函数，
/// 两个工具落盘的安全性来自同一处实现。
pub(crate) async fn write_atomic(target: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = target.parent()
        && !parent.as_os_str().is_empty()
    {
        tokio::fs::create_dir_all(parent).await?;
    }
    let tmp = tmp_sibling(target);
    if let Err(err) = tokio::fs::write(&tmp, bytes).await {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(err);
    }
    if let Err(err) = tokio::fs::rename(&tmp, target).await {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(err);
    }
    Ok(())
}

/// 目标同目录下的临时文件名。用进程 pid 而不是随机数区分并发写入：`write`/`edit` 都在
/// `Concurrency::Exclusive` 屏障下运行，同一进程内不会有第二次写同一文件的并发调用，
/// pid 只需要用来跟"同目录残留的旧 tmp 文件"区分即可（oh-my-pi write.ts:648 同样只用 pid，
/// 前提相同：单机单进程独占写这一份文件）。
fn tmp_sibling(target: &Path) -> PathBuf {
    let mut name = target
        .file_name()
        .map_or_else(OsString::new, OsString::from);
    name.push(format!(".zcode.tmp-{}", std::process::id()));
    target.with_file_name(name)
}

/// 组装成功文案：新建与覆写用不同的措辞，覆写额外附一份紧凑 diff。
fn success_body(
    display: &str,
    resolution: &SymlinkResolution,
    workspace: &Workspace,
    old: Option<&[u8]>,
    new_content: &str,
) -> String {
    let mut body = String::new();
    if let Some(note) = symlink_note(display, resolution, workspace) {
        body.push_str(&note);
        body.push_str("\n\n");
    }
    match old {
        None => {
            let _ = write!(
                body,
                "已创建 {display}（{} 字节，{} 行）。",
                new_content.len(),
                new_content.lines().count()
            );
        }
        Some(old_bytes) => {
            let _ = write!(
                body,
                "已覆写 {display}（{} 字节 → {} 字节）。",
                old_bytes.len(),
                new_content.len()
            );
            let old_text = String::from_utf8_lossy(old_bytes);
            let diff = compact_diff(&old_text, new_content, DIFF_CONTEXT_LINES);
            if !diff.is_empty() {
                body.push_str("\n\n");
                body.push_str(&diff);
            }
        }
    }
    body
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;
    use tokio::sync::mpsc;
    use zcode_agent::{EntryId, InterruptSignal, SessionId, StoredToolResultContent};

    use super::*;
    use crate::config::ToolsConfig;

    fn ctx() -> ToolContext {
        let (progress, _rx) = mpsc::unbounded_channel();
        ToolContext {
            session_id: SessionId::generate(),
            entry_id: EntryId::generate(),
            call_id: "call_1".to_owned(),
            cwd: std::env::temp_dir(),
            cancel: InterruptSignal::new(),
            steering: InterruptSignal::new(),
            progress,
        }
    }

    fn tools_config() -> ToolsConfig {
        ToolsConfig {
            disabled: Vec::new(),
            bash_timeout_secs: 120,
            read_max_lines: 2000,
        }
    }

    fn tool_in(dir: &Path) -> WriteTool {
        let workspace = Arc::new(Workspace::new(dir.to_path_buf()));
        WriteTool::new(workspace, &tools_config())
    }

    fn result_text(output: &ToolOutput) -> String {
        output
            .content
            .iter()
            .map(|block| match block {
                StoredToolResultContent::Text { text } => text.clone(),
                StoredToolResultContent::Image { .. } => String::new(),
            })
            .collect()
    }

    #[tokio::test]
    async fn creates_a_new_file() {
        let dir = tempdir().expect("tempdir");
        let tool = tool_in(dir.path());

        let args = json!({ "path": "hello.txt", "content": "hi\n" });
        let result = tool
            .execute(args, ctx())
            .await
            .expect("write should succeed");
        assert!(
            result_text(&result).contains("已创建"),
            "新建文案应说明是创建"
        );

        let on_disk = tokio::fs::read_to_string(dir.path().join("hello.txt"))
            .await
            .expect("read back");
        assert_eq!(on_disk, "hi\n");
    }

    #[tokio::test]
    async fn overwrites_existing_file_with_a_diff() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("hello.txt");
        tokio::fs::write(&path, "old\n")
            .await
            .expect("seed fixture");
        let tool = tool_in(dir.path());

        let args = json!({ "path": "hello.txt", "content": "new\n" });
        let result = tool
            .execute(args, ctx())
            .await
            .expect("write should succeed");
        let text = result_text(&result);
        assert!(text.contains("已覆写"), "覆写文案应说明是覆写：{text}");
        assert!(text.contains("- old"), "覆写应附带 diff 的删除行：{text}");
        assert!(text.contains("+ new"), "覆写应附带 diff 的新增行：{text}");

        let on_disk = tokio::fs::read_to_string(&path).await.expect("read back");
        assert_eq!(on_disk, "new\n");
    }

    #[tokio::test]
    async fn creates_missing_parent_directories() {
        let dir = tempdir().expect("tempdir");
        let tool = tool_in(dir.path());

        let args = json!({ "path": "a/b/c/hello.txt", "content": "nested\n" });
        tool.execute(args, ctx())
            .await
            .expect("write should create parents");

        let on_disk = tokio::fs::read_to_string(dir.path().join("a/b/c/hello.txt"))
            .await
            .expect("read back nested file");
        assert_eq!(on_disk, "nested\n");
    }

    #[tokio::test]
    async fn blocks_read_selector_shaped_target_with_empty_content() {
        let dir = tempdir().expect("tempdir");
        let tool = tool_in(dir.path());

        // "report.md:50-100" 解析成合法的 read 选择器，字面文件不存在，content 为空——
        // 三个条件同时成立，必须被拦。
        let args = json!({ "path": "report.md:50-100", "content": "" });
        let err = tool
            .execute(args, ctx())
            .await
            .expect_err("读选择器误触必须被拦截");
        let message = err.to_string();
        assert!(
            message.contains("read("),
            "错误应引导模型改用 read：{message}"
        );

        assert!(
            tokio::fs::metadata(dir.path().join("report.md:50-100"))
                .await
                .is_err(),
            "被拦截时不应该在磁盘上留下这个文件"
        );
    }

    #[tokio::test]
    async fn non_empty_content_is_never_blocked_by_the_selector_guard() {
        let dir = tempdir().expect("tempdir");
        // 直接测防护函数本身，不走 execute() 的真实落盘：在 NTFS 上，字面带冒号的文件名
        // 会被解释成候补数据流（Alternate Data Stream）语法而不是一个普通文件名，
        // 那是操作系统的文件名限制，不是这条防护要负责的事——这里只需要证明"非空
        // content 时防护函数放行"，不需要真的把这个名字写到磁盘上。
        let missing = dir.path().join("report.md:50-100");
        let result =
            guard_read_selector_misfire("report.md:50-100", "real content\n", &missing).await;
        assert!(result.is_ok(), "非空 content 不应被这条防护拦截");
    }
}
