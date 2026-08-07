//! 项目根发现：从当前目录向上找 `.git` 或 `.zcode/` 标记。
//!
//! # 与 `workspace::detect_root` 的区别（IRC 已对齐，别合并成一个函数）
//!
//! `workspace::detect_root` 是路径解析用的工作区根：只认 `.git`，找不到就回退到 `cwd`
//! （工作区根必须永远存在，不能是 `Option`）。本模块的 [`find_project_root`] 是**配置发现**
//! 用的项目根：`.git` 与 `.zcode/` 目录任一命中即可（后者覆盖"项目还没 `git init` 但已经有
//! `.zcode/config.toml`"这种场景，例如 CI 沙箱或临时演示目录），找不到就是 `None`——
//! "没有项目级配置"本身是合法状态，调用方只需回退到全局配置与内置默认值。

use std::path::{Path, PathBuf};

/// 向上遍历查找项目根时的层数硬上限。
///
/// 磁盘根与主目录两个停止条件已经界定了绝大多数场景；这个上限只是防御性兜底——例如
/// `cwd` 落在一个不含用户主目录前缀的深层挂载点，两个自然条件都碰不到。上游三仓都没有
/// 给出这个具体数字的依据；量级参考同仓另一处有界向上遍历
/// （jcode `crates/jcode-core/src/stdin_detect.rs:102` 用 32 层防进程树遍历失控）：
/// 真实项目目录嵌套深度极少超过一位数，64 留了两倍以上余量，同时保证遍历在微秒级完成。
const MAX_ASCEND: usize = 64;

/// 从 `cwd` 向上查找项目根：命中 `.zcode/` 目录或 `.git`（目录或文件——后者是
/// git worktree 的 gitdir 指针）即为项目根并停止，返回该目录。
///
/// 遍历三重有界：到达文件系统根、到达 `home`（若提供）、或超过 [`MAX_ASCEND`] 层，
/// 命中任一都会停止并返回 `None`。`home` 由调用方传入而不是内部调用
/// `zcode_text::home_dir()`——这样测试能注入合成的主目录边界，不依赖真实环境。
#[must_use]
pub(crate) fn find_project_root(cwd: &Path, home: Option<&Path>) -> Option<PathBuf> {
    let mut dir = cwd;
    for _ in 0..MAX_ASCEND {
        // **home 边界必须先于标记判定**。`~/.zcode` 是状态目录（凭据、会话、日志、
        // 全局配置都在里面），不是项目标记；顺序反过来的话，首次运行创建出 `~/.zcode`
        // 之后，从主目录启动就会把主目录判成项目根，于是同一份 `~/.zcode/config.toml`
        // 被当作"全局"和"项目"两层各加载一次。
        if home.is_some_and(|boundary| boundary == dir) {
            return None;
        }
        if is_marker_dir(dir) {
            return Some(dir.to_path_buf());
        }
        dir = dir.parent()?;
    }
    None
}

/// 判定一个目录是否携带项目根标记：`.zcode/` 目录，或 `.git`（目录或文件）。
fn is_marker_dir(dir: &Path) -> bool {
    dir.join(".zcode").is_dir() || dir.join(".git").exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_git_directory_marker() {
        let root = tempfile::tempdir().expect("创建临时目录");
        std::fs::create_dir(root.path().join(".git")).expect("创建 .git 目录");
        let nested = root.path().join("a").join("b");
        std::fs::create_dir_all(&nested).expect("创建嵌套子目录");

        let found = find_project_root(&nested, None);
        assert_eq!(found, Some(root.path().to_path_buf()));
    }

    #[test]
    fn finds_git_file_marker_for_worktree() {
        let root = tempfile::tempdir().expect("创建临时目录");
        std::fs::write(root.path().join(".git"), "gitdir: ../main/.git/worktrees/x")
            .expect("写 worktree .git 指针文件");
        let nested = root.path().join("src");
        std::fs::create_dir(&nested).expect("创建子目录");

        let found = find_project_root(&nested, None);
        assert_eq!(found, Some(root.path().to_path_buf()));
    }

    #[test]
    fn finds_dot_zcode_marker_without_git() {
        let root = tempfile::tempdir().expect("创建临时目录");
        std::fs::create_dir(root.path().join(".zcode")).expect("创建 .zcode 目录");
        let nested = root.path().join("nested");
        std::fs::create_dir(&nested).expect("创建子目录");

        let found = find_project_root(&nested, None);
        assert_eq!(found, Some(root.path().to_path_buf()));
    }

    #[test]
    fn stops_at_home_and_returns_none() {
        let home = tempfile::tempdir().expect("创建临时主目录");
        let project = home.path().join("workspace").join("app");
        std::fs::create_dir_all(&project).expect("创建嵌套子目录");

        // home 本身与其所有祖先都没有标记，遍历必须在 home 处停止，不再向上到真实磁盘根。
        let found = find_project_root(&project, Some(home.path()));
        assert_eq!(found, None);
    }

    /// 主目录自身带 `.zcode/`（首次运行后必然如此）也不算项目根。
    ///
    /// 回归防线：边界判定若排在标记判定之后，从主目录启动会把 `~/.zcode/config.toml`
    /// 当成项目配置再加载一遍。
    #[test]
    fn home_with_state_dir_is_not_a_project_root() {
        let home = tempfile::tempdir().expect("创建临时目录");
        std::fs::create_dir_all(home.path().join(".zcode")).expect("造出状态目录");

        let found = find_project_root(home.path(), Some(home.path()));
        assert_eq!(found, None);
    }

    /// 向上遍历必须有界。
    ///
    /// 用"深过 `MAX_ASCEND`"而不是"一路走到磁盘根"来测：后者会真的走出临时目录、
    /// 撞上开发者主目录里的 `.git`/`.zcode`，结果取决于跑测机器的家目录长什么样
    /// （`rule://rust-testing` 禁止依赖环境状态）。
    #[test]
    fn traversal_stops_after_max_ascend_levels() {
        let root = tempfile::tempdir().expect("创建临时目录");
        let mut nested = root.path().to_path_buf();
        for _ in 0..=MAX_ASCEND {
            nested.push("d");
        }
        std::fs::create_dir_all(&nested).expect("创建超深子目录");
        // 标记放在链条最顶端：只有"无界遍历"才够得着它。
        std::fs::create_dir_all(root.path().join(".zcode")).expect("造出标记");

        assert_eq!(find_project_root(&nested, None), None);
    }
}
