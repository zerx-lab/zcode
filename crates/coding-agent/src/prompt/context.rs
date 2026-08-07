//! 首条 user 消息里 `<system-reminder>` 包裹块的内容：一次性环境快照
//! （cwd / 操作系统 / 日期 / 当前模型 / git 状态）。
//!
//! # 为什么不进 system prompt
//!
//! 见 [`crate::prompt::PromptSet::session_context`] 的文档注释与 jcode
//! `crates/jcode-base/src/session.rs:897-921`：这段内容里的日期与 git 状态每次
//! 运行都可能不同，放进 system prompt 会天天打穿 prompt cache 的前缀匹配；放进
//! 首条 user 消息则只在**创建新会话**时生成一次并落盘，历史会话重放时不会被
//! 悄悄改写成"今天"的状态。本模块只生成这段文本本身，是否要用
//! `<system-reminder>` 包裹、要不要写进会话是 `HostCore` 的事——它才知道当前
//! 是不是"这个会话的第一条消息"。
//!
//! # 模板机制
//!
//! 本 crate 没有接 handlebars（`Cargo.toml` 由 Main 维护，装配层不允许自己加
//! 依赖）。占位符（`{{DATE}}` 等）用 [`str::replace`] 做纯字符串替换，结构仍然
//! 100% 来自 `.md` 文件，Rust 代码里不出现任何一句面向模型的英文提示词
//! （`rule://zcode-architecture`「Prompt 必须存静态 .md」）。
//!
//! # git 调用集中在这里
//!
//! 私有的 [`mod@git`] 是本 crate**目前唯一**的 git 子进程调用点，只服务于"一次性
//! 环境快照"这一个场景。`rule://zcode-architecture`「中央工具函数优先」要求 git
//! 只能经统一入口调用——一旦出现第二个需要 git 信息的调用方（例如某个工具要展示
//! diff），必须把这个子模块提升为 `src/utils/git.rs` 这样跨模块共享的中央
//! helper，而不是在别处重复拼一遍 `Command::new("git")`。

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// 会话上下文的基础模板：日期 / 操作系统 / 模型 / 工作目录，见 `session_context.md`。
const BASE_TEMPLATE: &str = include_str!("session_context.md");

/// 检测到 git 仓库时追加的小节模板，见 `session_context_git.md`。
const GIT_TEMPLATE: &str = include_str!("session_context_git.md");

/// git 状态里最多列出的变更文件数。取值：jcode `crates/jcode-base/src/prompt.rs:689,695`
/// 同为 5——一次性环境快照只需要让模型知道"有改动、大致在哪"，不是完整
/// diff，5 条足以覆盖典型的单一任务改动范围而不占用过多首条消息篇幅。
const MAX_LISTED_FILES: usize = 5;

/// 构建环境上下文文本（不含 `<system-reminder>` 包裹标签）。
///
/// `cwd` 用于定位 git 仓库（`git -C <cwd>` 语义，经 [`std::process::Command::current_dir`]
/// 而非拼接进参数字符串）；`cwd_display` 是脱敏后的展示路径，调用方应传
/// `workspace.display(workspace.root())`。`model_id` 是当前会话绑定的模型 id。
pub(crate) async fn build(cwd: &Path, cwd_display: &str, model_id: &str) -> String {
    let mut text = BASE_TEMPLATE
        .replace("{{DATE}}", &today_utc())
        .replace("{{OS}}", std::env::consts::OS)
        .replace("{{ARCH}}", std::env::consts::ARCH)
        .replace("{{MODEL}}", model_id)
        .replace("{{CWD}}", cwd_display);

    if let Some(status) = git::snapshot(cwd).await {
        text.push('\n');
        text.push_str(&render_git_section(&status));
    }

    text
}

/// 把 [`git::Status`] 渲染成 `session_context_git.md` 模板的最终文本。
fn render_git_section(status: &git::Status) -> String {
    let branch = status.branch.as_deref().unwrap_or("(detached HEAD)");
    let body = if status.total_changed == 0 {
        "clean".to_owned()
    } else {
        let mut lines = vec![format!("{} file(s) changed:", status.total_changed)];
        lines.extend(status.files.iter().map(|file| format!("  {file}")));
        if status.total_changed > status.files.len() {
            lines.push("  ...".to_owned());
        }
        lines.join("\n")
    };

    GIT_TEMPLATE
        .replace("{{BRANCH}}", branch)
        .replace("{{STATUS}}", &body)
}

/// 纯 UTC 日期字符串（`YYYY-MM-DD`），不依赖任何日期 crate——workspace 依赖表里
/// 没有 `chrono`/`time`（`Cargo.toml` 由 Main 维护，本模块不允许自己加依赖），
/// 而这里只需要"今天的 UTC 日期"这一个值，不需要引入完整日历库。
fn today_utc() -> String {
    let days = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        // 系统时钟早于 Unix 纪元在任何真实部署下都不可达；钳到纪元当天而不是
        // panic，环境探测不该让会话创建失败。
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_secs() / 86_400).unwrap_or(0)
        });
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}")
}

/// Howard Hinnant 的 `civil_from_days`：把"Unix 纪元以来的天数"换算成
/// `(年, 月, 日)`（UTC 历法，无闰秒）。公有领域算法，来源与推导过程见
/// <http://howardhinnant.github.io/date_algorithms.html#civil_from_days>。
///
/// 全程用 `i64` 而不是照算法原文用 `unsigned`，以绕开 `as` 转换
/// （`rule://rust-quality`「数值转换」）；返回值转 `u32` 时，月/日的取值范围由
/// 算法本身保证落在 `[1,12]`/`[1,31]`，`expect` 标注的正是这条不变量。
fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let month = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let year = if month <= 2 { y + 1 } else { y };

    #[allow(clippy::expect_used)]
    // 不变量：上面的算法保证 month ∈ [1,12]，任何真实输入都不会触发这个 expect。
    let month = u32::try_from(month).expect("civil_from_days 保证 month 属于 [1,12]");
    #[allow(clippy::expect_used)]
    // 不变量：上面的算法保证 day ∈ [1,31]，任何真实输入都不会触发这个 expect。
    let day = u32::try_from(day).expect("civil_from_days 保证 day 属于 [1,31]");
    // 年份钳位而非 expect：极端到天文数字的年份在实际系统时钟下不可达，钳到
    // i32::MAX 只是为了在类型上排除 panic，不是预期触发路径。
    let year = i32::try_from(year).unwrap_or(i32::MAX);

    (year, month, day)
}

/// 一次性 git 快照：本 crate 唯一的 git 子进程调用点，见模块文档。
mod git {
    use std::path::Path;
    use std::time::Duration;

    use tokio::process::Command;

    use super::MAX_LISTED_FILES;

    /// 单次 git 调用的超时。三条调用（`rev-parse` / `branch --show-current` /
    /// `status --porcelain`）都是纯本地元数据查询，不涉及网络；上游 jcode
    /// （`crates/jcode-base/src/prompt.rs:647-706`）与 oh-my-pi 均未给它们设超时——
    /// 这是本仓补的。取 5 秒：`zcode_text::GrepLimits` 的默认超时是 30 秒，但那是
    /// 给"可能扫描整个仓库"的 grep 用的；这里只是三条恒定复杂度的查询，5 秒足以
    /// 覆盖巨型仓库上 `status --porcelain` 的最坏情况与磁盘抖动，同时避免新会话
    /// 的首条消息被卡住太久。
    const GIT_TIMEOUT: Duration = Duration::from_secs(5);

    /// 一次性 git 状态快照。
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(super) struct Status {
        /// 当前分支名；detached HEAD 或读取失败时为 `None`。
        pub(super) branch: Option<String>,
        /// `git status --porcelain` 报告的变更文件总数。
        pub(super) total_changed: usize,
        /// 变更文件列表，最多 [`MAX_LISTED_FILES`] 条。
        pub(super) files: Vec<String>,
    }

    /// 采集一次性快照。不在 git 仓库里、系统没装 git、或任意一步超时/非零退出，
    /// 都返回 `None`，让调用方整段省略 Git 小节——环境探测不该让会话创建失败。
    pub(super) async fn snapshot(cwd: &Path) -> Option<Status> {
        let in_repo = run(cwd, &["rev-parse", "--is-inside-work-tree"])
            .await
            .is_some_and(|out| out.trim() == "true");
        if !in_repo {
            return None;
        }

        let branch = run(cwd, &["branch", "--show-current"])
            .await
            .map(|out| out.trim().to_owned())
            .filter(|branch| !branch.is_empty());

        let (total_changed, files) = run(cwd, &["status", "--porcelain"])
            .await
            .map(|out| parse_porcelain(&out, MAX_LISTED_FILES))
            .unwrap_or_default();

        Some(Status {
            branch,
            total_changed,
            files,
        })
    }

    /// 跑一条 git 子命令，返回 stdout；任何失败（超时、spawn 失败、非零退出、
    /// 非 UTF-8 输出）都折叠成 `None`。参数用数组传递，不拼 `sh -c`
    /// （`rule://rust-quality`「标准库 / 生态优先」）。
    async fn run(cwd: &Path, args: &[&str]) -> Option<String> {
        let output = tokio::time::timeout(
            GIT_TIMEOUT,
            Command::new("git").args(args).current_dir(cwd).output(),
        )
        .await
        .ok()? // 超时
        .ok()?; // spawn / IO 错误（含系统没装 git）

        if !output.status.success() {
            return None;
        }
        String::from_utf8(output.stdout).ok()
    }

    /// 纯函数：解析 `git status --porcelain` 的输出，返回
    /// `(变更文件总数, 前 max_listed 个文件条目)`。抽成纯函数是为了能在不依赖
    /// 真实 git 仓库、不 spawn 子进程的前提下单独测试解析逻辑
    /// （`rule://rust-testing`「git 那段的测试不得依赖本仓是 git 仓」）。
    pub(super) fn parse_porcelain(output: &str, max_listed: usize) -> (usize, Vec<String>) {
        let lines: Vec<&str> = output.lines().filter(|line| !line.is_empty()).collect();
        let files = lines
            .iter()
            .take(max_listed)
            .map(|line| line.trim().to_owned())
            .collect();
        (lines.len(), files)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn parse_porcelain_counts_and_truncates_listing() {
            let output =
                " M src/a.rs\n?? src/b.rs\n D src/c.rs\n M src/d.rs\n M src/e.rs\n M src/f.rs\n";

            let (total, files) = parse_porcelain(output, 5);

            assert_eq!(total, 6);
            assert_eq!(
                files,
                vec![
                    "M src/a.rs",
                    "?? src/b.rs",
                    "D src/c.rs",
                    "M src/d.rs",
                    "M src/e.rs"
                ]
            );
        }

        #[test]
        fn parse_porcelain_empty_output_means_clean() {
            let (total, files) = parse_porcelain("", 5);

            assert_eq!(total, 0);
            assert!(files.is_empty());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_from_days_matches_known_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(11_017), (2000, 3, 1));
        assert_eq!(civil_from_days(19_782), (2024, 2, 29));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
    }

    #[tokio::test]
    async fn build_includes_base_fields_and_no_git_section_outside_a_repo() {
        let dir = tempfile::tempdir().expect("创建临时目录");

        let context = build(dir.path(), "~/project", "claude-x").await;

        assert!(context.contains("~/project"));
        assert!(context.contains("claude-x"));
        assert!(context.contains(std::env::consts::OS));
        // 系统临时目录不会位于任何 git 工作树内，Git 小节必须整段省略而不是
        // 打印一个空/占位分支名——静默降级意味着"没有这一段"，不是"有一段空的"。
        assert!(!context.contains("Git branch:"));
    }
}
