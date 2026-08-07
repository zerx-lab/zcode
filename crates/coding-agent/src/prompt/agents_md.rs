//! `AGENTS.md` 发现与合并。
//!
//! # 发现规则
//!
//! 从工作区根向上走：
//! 1. 全局 `~/AGENTS.md`（如果存在）永远排在最前——它是用户级别的默认约定，
//!    应该被更具体的项目/目录约定覆盖，而不是反过来。
//! 2. 从工作区根开始向上遍历祖先目录，逐级查找 `<dir>/AGENTS.md`，直到找到 git
//!    仓库根（存在 `.git`，目录或文件都算——worktree 用 `.git` 文件）或者到达
//!    `home`（含），以先出现者为准。收集到的文件按“离工作区根越近排越后”合并，
//!    抄 oh-my-pi 的做法：离 cwd 更近的指令更具体，排在后面在同等模型注意力分布
//!    下通常权重更高（`packages/coding-agent/src/discovery/agents-md.ts:25-51` 的
//!    walk-up 顺序 + `system-prompt.ts` 对应渲染把 depth 大的放后）。
//! 3. `home` 目录本身不在第 2 步重复读取——那是第 1 步的职责，避免同一份文件在
//!    合并结果里出现两次。
//!
//! # 边界：不做 `@import` 展开
//!
//! 只读取文件的原始内容，不解析/展开文件内的 `@path/to/file` 之类的行内引用
//! （oh-my-pi 支持这个，`packages/coding-agent/src/discovery/at-imports.ts`）。
//! 本仓暂不实现这一层；后续需要时在这个模块里加，不要在别处旁路。
//!
//! # 字节上限：两个参考仓都没有，这是本仓补的
//!
//! jcode（`crates/jcode-base/src/prompt.rs:816-860`）与 oh-my-pi
//! （`packages/coding-agent/src/capability/fs.ts:11-34` 的 `readFile` 只做类型
//! 与错误防护，不做大小封顶）都没有 `AGENTS.md` 大小上限——调研已确认这是缺失
//! 而非有意的产品决策，不能照抄。
//!
//! 取值反推自 jcode 压缩预算里的系统开销常量
//! （`crates/jcode-compaction-core/src/lib.rs:43` `CHARS_PER_TOKEN = 4`、
//! `:62-63` `SYSTEM_OVERHEAD_TOKENS = 18_000`，注释写明是"~8k 系统 prompt + ~10k
//! 工具定义"的估计）。本仓的 `AGENTS.md` 合并结果会作为 `system[1]` 叠加在这份
//! 开销之上，给它的份额取系统开销的四分之一：`18_000 / 4 = 4_500` token，
//! 乘 `CHARS_PER_TOKEN = 4` 得约 `18_000` 字节，就近取整到 16 KiB 作为**合并后总量**
//! 上限；单文件上限取总量的一半（8 KiB），使全局 + 至少一层项目文件各自都还有
//! 预算，而不是被第一份大文件吃满整个额度。
//!
//! 截断复用 [`zcode_text::enforce_inline_byte_cap`]（中央截断实现，见
//! `rule://rust-quality`「先搜索，再实现」）——它已经在结果里内嵌了省略标记与
//! 被丢弃字节数，调用方不需要也不应该自己再拼一份通知文案。

use std::path::{Path, PathBuf};

/// 单份 `AGENTS.md` 的字节上限，推导见模块文档。
const MAX_FILE_BYTES: usize = 8 * 1024;

/// 合并全部 `AGENTS.md` 后的总字节上限，推导见模块文档。
const MAX_TOTAL_BYTES: usize = 16 * 1024;

/// 发现并合并当前工作区可见的全部 `AGENTS.md`，返回 `None` 表示一份都没找到。
///
/// `display` 把绝对路径转成脱敏后的展示路径：生产环境调用方应传
/// `|p| workspace.display(p)`；单元测试传一个不脱敏的桩，好让发现/合并/截断
/// 逻辑可以脱离 `Workspace` 单独测试，不必在每个用例里构造完整的工作区。`home`
/// 同理由调用方注入（生产环境传 `zcode_text::home_dir()`），测试传 `tempdir`
/// 路径，避免碰真实 `$HOME`（`rule://rust-testing`「并行安全」）。
pub(crate) async fn discover(
    workspace_root: &Path,
    home: Option<&Path>,
    display: impl Fn(&Path) -> String,
) -> Option<String> {
    let mut entries = Vec::new();
    if let Some(global) = read_global(home).await {
        entries.push(global);
    }
    entries.extend(collect_project_files(workspace_root, home).await);

    if entries.is_empty() {
        return None;
    }

    let header_template = include_str!("agents_md_header.md");
    let mut merged = include_str!("agents_md_intro.md").to_owned();
    for (path, content) in entries {
        let capped = zcode_text::enforce_inline_byte_cap(content.trim(), MAX_FILE_BYTES);
        merged.push_str("\n\n");
        merged.push_str(&header_template.replace("{{PATH}}", &display(&path)));
        merged.push_str("\n\n");
        merged.push_str(&capped);
    }

    Some(zcode_text::enforce_inline_byte_cap(
        &merged,
        MAX_TOTAL_BYTES,
    ))
}

/// 读取全局 `~/AGENTS.md`；`home` 未知、文件不存在、或读取失败一律返回 `None`。
async fn read_global(home: Option<&Path>) -> Option<(PathBuf, String)> {
    let home = home?;
    let path = home.join("AGENTS.md");
    let content = tokio::fs::read_to_string(&path).await.ok()?;
    Some((path, content))
}

/// 从 `start` 向上走到 git 仓库根（存在 `.git`）或 `home`（含）为止，收集沿途
/// 的 `AGENTS.md`。返回顺序是"离 `start` 越近排越后"，见模块文档的合并顺序说明。
///
/// `home` 目录本身的 `AGENTS.md` 不在这里读取——那是 [`read_global`] 的职责，
/// 这里再读一遍会在合并结果里造成重复段落。
async fn collect_project_files(start: &Path, home: Option<&Path>) -> Vec<(PathBuf, String)> {
    let mut found = Vec::new();
    let mut current = start.to_path_buf();

    loop {
        let is_home = home.is_some_and(|home| home == current);
        if !is_home && let Ok(content) = tokio::fs::read_to_string(current.join("AGENTS.md")).await
        {
            found.push((current.clone(), content));
        }

        let is_repo_root = tokio::fs::try_exists(current.join(".git"))
            .await
            .unwrap_or(false);
        if is_repo_root || is_home {
            break;
        }

        match current.parent() {
            Some(parent) => current = parent.to_path_buf(),
            // 到达文件系统根：既没找到 `.git` 也没找到 `home`，就此打住而不是报错——
            // 环境探测不该让会话创建失败。
            None => break,
        }
    }

    found.reverse();
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity_display(path: &Path) -> String {
        path.display().to_string()
    }

    /// 用 `.git` 标记把祖先游走钳在临时目录内，避免测试环境真实的祖先目录
    /// （例如系统临时目录之上的路径）里意外存在的 `AGENTS.md` 污染断言。
    fn mark_repo_root(dir: &Path) {
        std::fs::write(dir.join(".git"), "").expect("写入 .git 标记文件");
    }

    #[tokio::test]
    async fn no_agents_md_returns_none() {
        let dir = tempfile::tempdir().expect("创建临时目录");
        mark_repo_root(dir.path());

        let merged = discover(dir.path(), None, identity_display).await;

        assert!(merged.is_none());
    }

    #[tokio::test]
    async fn single_project_file_is_included() {
        let dir = tempfile::tempdir().expect("创建临时目录");
        mark_repo_root(dir.path());
        std::fs::write(dir.path().join("AGENTS.md"), "Use tabs, not spaces.")
            .expect("写入 AGENTS.md");

        let merged = discover(dir.path(), None, identity_display)
            .await
            .expect("应当找到这一份文件");

        assert!(merged.contains("Use tabs, not spaces."));
    }

    #[tokio::test]
    async fn nested_directories_merge_with_cwd_last() {
        let root = tempfile::tempdir().expect("创建临时目录");
        mark_repo_root(root.path());
        let nested = root.path().join("pkg").join("sub");
        std::fs::create_dir_all(&nested).expect("创建嵌套目录");
        std::fs::write(root.path().join("AGENTS.md"), "ROOT RULE").expect("写入根目录文件");
        std::fs::write(nested.join("AGENTS.md"), "NESTED RULE").expect("写入嵌套目录文件");

        let merged = discover(&nested, None, identity_display)
            .await
            .expect("应当找到两份文件");

        let root_pos = merged.find("ROOT RULE").expect("根目录内容应当存在");
        let nested_pos = merged.find("NESTED RULE").expect("嵌套目录内容应当存在");
        assert!(
            root_pos < nested_pos,
            "离 cwd 更近的文件必须排在更后面（更显著）"
        );
    }

    #[tokio::test]
    async fn global_agents_md_leads_the_merge() {
        let home = tempfile::tempdir().expect("创建临时 home 目录");
        let project = tempfile::tempdir().expect("创建临时项目目录");
        mark_repo_root(project.path());
        std::fs::write(home.path().join("AGENTS.md"), "GLOBAL RULE").expect("写入全局文件");
        std::fs::write(project.path().join("AGENTS.md"), "PROJECT RULE").expect("写入项目文件");

        let merged = discover(project.path(), Some(home.path()), identity_display)
            .await
            .expect("应当找到两份文件");

        let global_pos = merged.find("GLOBAL RULE").expect("全局内容应当存在");
        let project_pos = merged.find("PROJECT RULE").expect("项目内容应当存在");
        assert!(global_pos < project_pos, "全局文件必须排在最前");
    }

    #[tokio::test]
    async fn home_agents_md_is_not_duplicated_by_project_walk() {
        let home = tempfile::tempdir().expect("创建临时 home 目录");
        // 项目目录本身就是 home（没有独立仓库根），验证第 2 步不会把 home 的
        // AGENTS.md 再读一遍导致重复段落。
        std::fs::write(home.path().join("AGENTS.md"), "GLOBAL RULE").expect("写入全局文件");

        let merged = discover(home.path(), Some(home.path()), identity_display)
            .await
            .expect("应当找到这一份文件");

        assert_eq!(merged.matches("GLOBAL RULE").count(), 1);
    }

    #[tokio::test]
    async fn oversized_single_file_is_truncated_with_notice() {
        let dir = tempfile::tempdir().expect("创建临时目录");
        mark_repo_root(dir.path());
        let huge = "x".repeat(MAX_FILE_BYTES * 4);
        std::fs::write(dir.path().join("AGENTS.md"), &huge).expect("写入超大文件");

        let merged = discover(dir.path(), None, identity_display)
            .await
            .expect("应当找到这份文件");

        assert!(merged.len() < huge.len(), "合并结果必须小于原始输入");
        assert!(
            merged.contains('…'),
            "超限截断必须带明示标记，而不是静默裁掉内容"
        );
    }

    #[tokio::test]
    async fn oversized_total_is_truncated_with_notice() {
        let root = tempfile::tempdir().expect("创建临时目录");
        mark_repo_root(root.path());
        // 每层都在单文件上限之内，但层数够多时总量仍然会超过 MAX_TOTAL_BYTES。
        let per_file = "y".repeat(MAX_FILE_BYTES - 256);
        let mut current = root.path().to_path_buf();
        for i in 0..6 {
            current = current.join(format!("d{i}"));
            std::fs::create_dir_all(&current).expect("创建嵌套目录");
            std::fs::write(current.join("AGENTS.md"), &per_file).expect("写入分层文件");
        }

        let merged = discover(&current, None, identity_display)
            .await
            .expect("应当找到这些文件");

        assert!(
            merged.len() <= MAX_TOTAL_BYTES,
            "合并结果不得超过总量上限，实际 {} 字节",
            merged.len()
        );
        assert!(merged.contains('…'), "总量超限也必须带明示标记");
    }
}
