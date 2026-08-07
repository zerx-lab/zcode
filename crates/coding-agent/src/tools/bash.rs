//! `bash` 工具：在 shell 中执行任意命令。
//!
//! # 唯一允许拼 `sh -c` / `cmd /C` 的地方
//!
//! `rule://rust-quality` 明令禁止拼接 shell 字符串启动进程（参数注入风险 + 跨平台行为
//! 不一致），**唯一例外是"用户显式请求执行 shell 命令的工具实现"**——这正是本工具存在
//! 的全部理由：模型请求的是任意 shell 语法（管道、通配符、变量展开、`&&`/`;` 组合），
//! 剥离 shell 语法这个工具就没有存在意义了。
//!
//! # 进程组：前台与后台用同一套收尾，且只有一种 `timeout` 语义
//!
//! `rule://reference-first` 记录的线索表点名了 jcode `crates/jcode-app-core/src/tool/bash.rs`
//! 两处已知技术债，本模块刻意不抄：
//!
//! 1. jcode 的前台执行路径没有 setpgid（只有后台路径 `bash.rs:1113-1122` 有），取消时
//!    只能杀 `bash -c` 本身，孙进程变成孤儿继续跑。本模块**唯一**的执行路径在 spawn 时
//!    就建进程组（Unix：[`tokio::process::Command::process_group`]，安全 API，不需要
//!    `unsafe`；Windows：Job Object），取消/超时时杀的是整个进程组或整个 job，不区分
//!    "前台"/"后台"。
//! 2. jcode 的 `timeout` 字段在前台/后台两条路径里语义相反（`bash.rs:1161-1214`：前台
//!    是"超时就把命令过继给后台任务继续跑，不杀"，后台是"超时就真的杀"）。本模块只有
//!    一条路径，`timeout` 只有一个含义：到点杀掉整个进程组，[`ToolError::Timeout`]。
//!
//! # Windows 用 Job Object 而不是逐个 `TerminateProcess`
//!
//! bash 起的子进程可能再 fork 出孙进程——这正是需要进程组语义的原因。逐个枚举子进程树
//! 再挨个 `TerminateProcess` 天然有竞态：枚举到一半又冒出新进程。Job Object 把"杀干净"
//! 变成内核保证：子进程一开始就被 [`AssignProcessToJobObject`] 装进 job，
//! `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` 使得 job 里**当前及此后加入**的每一个进程，
//! 在句柄关闭或 [`TerminateJobObject`] 时都会被杀，不存在遍历竞态。
//!
//! [`AssignProcessToJobObject`]: windows_sys::Win32::System::JobObjects::AssignProcessToJobObject
//! [`TerminateJobObject`]: windows_sys::Win32::System::JobObjects::TerminateJobObject
//!
//! # 流式输出与内联字节封顶
//!
//! stdout/stderr 按到达顺序合并、经 [`ToolContext::report`] 以 [`ToolProgress::Chunk`]
//! 实时推出去，同时在内存里攒一份**头部**截断到 [`MAX_INLINE_BYTES`] 字节的副本（jcode
//! `crates/jcode-app-core/src/tool/bash.rs:26` 的 `MAX_OUTPUT_LEN = 30000`，前提是这只是
//! 防御性内存上限，不是最终呈现宽度——真正决定模型看到多宽输出的是下游
//! [`crate::tools::output::finish`] 那道按显示宽度/行数的截断）。选头部而不是尾部：
//! 模型只需要看开头就能判断命令是否符合预期，尾部截断反而会把第一行报错藏起来。
//!
//! # 超时不丢已产出的输出
//!
//! [`ToolError::Timeout`] 是 `zcode-agent` 的只读定义（`crates/agent/src/error.rs:52-57`），
//! 只带一个 `seconds: u64`，没有文本字段——这是三个变体里唯一一个无法携带自定义文本的。
//! 因此"模型需要看到超时前跑出了什么"（oh-my-pi `packages/coding-agent/src/tools/bash.ts:1314-1315`
//! 的设计意图）在这里通过 [`ToolProgress::Status`] 实现：终止流程完成后把已缓冲的输出连同
//! 一句终止说明再报一次，再返回 `Err(ToolError::Timeout { seconds })`——`execute` 的返回值
//! 类型不允许再多做什么。

use std::fmt::Write as _;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use regex::Regex;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::time::Instant;
use zcode_agent::{
    ApprovalDecision, Concurrency, Tier, Tool, ToolContext, ToolError, ToolOutput, ToolProgress,
};

use crate::config::ToolsConfig;
use crate::tools::output;
use crate::workspace::{PathError, Workspace};

/// 超时钳制区间下限：小于 1 秒的等待没有意义，也给终止流程留不出最小操作窗口。
const MIN_TIMEOUT_SECS: u64 = 1;

/// 超时钳制区间上限。
///
/// 取值抄自 oh-my-pi `packages/coding-agent/src/tools/tool-timeouts.ts:11`
/// 的 `bash: { default: 300, min: 1, max: 3600 }`——前提是"允许长跑命令，但绝不允许一次
/// 调用无限期占住 [`Concurrency::Exclusive`] 并发槽"；同一常量也用作
/// `config.tools.bash_timeout_secs` 缺省未覆盖时的钳制上限。
const MAX_TIMEOUT_SECS: u64 = 3600;

/// 发出终止信号（Unix `SIGTERM`/Windows `TerminateJobObject`）后，等待进程树自然退出的
/// 宽限期；超过就升级到下一阶段（Unix `SIGKILL`）或直接放弃等待。
///
/// 取值抄自 oh-my-pi `packages/coding-agent/src/tools/bash.ts:1186` 的 `killGraceMs = 1000`
/// ——前提是"kill 与宽限等待必须竞速，杀不掉也不能无限等"，1000ms 是它对这条前提给出的
/// 具体取值，上游注释未给出这个数字本身的实验依据。
const KILL_GRACE: Duration = Duration::from_secs(1);

/// 流式读取阶段的内联字节封顶：防止一个话痨命令在内存里堆出几百 MB 的 `Vec<u8>`。
///
/// 取值抄自 jcode `crates/jcode-app-core/src/tool/bash.rs:26` 的 `MAX_OUTPUT_LEN = 30000`
/// ——前提见模块文档「流式输出与内联字节封顶」一节。
const MAX_INLINE_BYTES: usize = 30_000;

/// 命中即视为高危操作、强制要求确认的 bash 命令特征。
///
/// 逐条搬运自 oh-my-pi `packages/coding-agent/src/tools/bash.ts:167-206` 的
/// `CRITICAL_BASH_PATTERNS`，含它们的打磨痕迹：
/// - `shutdown`/`poweroff`/`reboot`/`halt`/`init 0` 锚在命令位置（`(?:^|[\s;&|(])`），
///   否则 `npm run reboot-tests` 或 `echo 'shutdown the queue'` 会误触；
/// - `.`/`source` 同样锚在命令边界，否则 `find . -name` 会误触；
/// - `chmod -R` 拆成数字模式（`[0-7]+`）与符号模式（`[ugoa+-=rwxXst,]+`）两条独立规则。
///
/// 前提：这份列表刻意收紧——假阴性的代价是数据丢失或主机沦陷，假阳性只是多问一次
/// （命中后仍然可以在审批弹窗里确认放行），上游注释原文如是说。
const CRITICAL_BASH_PATTERNS: &[&str] = &[
    // 递归破坏。
    r"(?i)\brm\s+-[a-z]*[rRfF][a-z]*\s+/",
    r"(?i)\bsudo\s+rm\b",
    r"(?i)\bchmod\s+-R\s+[0-7]+\s+/",
    r"\bchmod\s+-R\s+[ugoa+\-=rwxXst,]+\s+/",
    r"(?i)\bchown\s+-R\s+\S+\s+/",
    // fork bomb（几种常见空格写法）。
    r"(?i):\(\)\s*\{\s*:\s*\|\s*:",
    // 磁盘 / 文件系统破坏。
    r"(?i)>\s*/dev/sd[a-z]",
    r"(?i)\bmkfs(\.|\b)",
    r"(?i)\bdd\s+if=.+of=/dev/",
    r"(?i)\bshred\s+/dev/",
    r"(?i)\bcryptsetup\b",
    // 系统配置破坏。
    r"(?i)>\s*/etc/(?:passwd|shadow|sudoers)\b",
    r"(?i)\btee\s+(?:-a\s+)?/etc/(?:passwd|shadow|sudoers)\b",
    // 远程拉取即执行。
    r"(?i)\b(?:curl|wget|fetch)\b[^|]*\|\s*(?:bash|sh|zsh|fish)\b",
    r"(?i)(?:^|[\s;&|(])(?:bash|sh|zsh|source|\.)\s+<\(\s*(?:curl|wget|fetch)\b",
    r#"(?i)\beval\s+["'`]?\$\(\s*(?:curl|wget|fetch)\b|\beval\s+`\s*(?:curl|wget|fetch)\b"#,
    // 进程 / 主机控制。
    r"\bkill\s+-9\s+1\b",
    r"(?i)(?:^|[\s;&|(])(?:shutdown|poweroff|reboot|halt)(?:\s|$|[;|&])",
    r"(?i)(?:^|[\s;&|(])init\s+0\b",
    // 网络 shell 外泄。
    r"(?i)\bnc\b[^|;]*\s-[a-zA-Z]*[ec][a-zA-Z]*\s",
];

/// [`CRITICAL_BASH_PATTERNS`] 的预编译形态。
///
/// [`Tool::approval`] 在渲染路径上每次审批检查都会调用，必须纯且廉价——反面教材是
/// oh-my-pi 在 `write.approval` 里现场 `JSON.parse`（`packages/coding-agent/src/tools/write.ts:530-534`）。
/// 这里用 `OnceLock` 保证正则只编译一次，后续调用只是遍历切片做 `is_match`。
#[allow(clippy::expect_used)] // 见 `.map()` 内联注释：硬编码常量正则，失败即开发期缺陷
fn critical_patterns() -> &'static [Regex] {
    static PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        CRITICAL_BASH_PATTERNS
            .iter()
            // 硬编码常量正则编译失败＝开发期缺陷（写错了正则语法），不是运行期可恢复
            // 错误：这属于 rule://rust-quality 明确允许的"违反内部不变量"例外。
            .map(|pattern| Regex::new(pattern).expect("CRITICAL_BASH_PATTERNS 常量必须是合法正则"))
            .collect()
    })
}

/// `command` 是否命中 [`CRITICAL_BASH_PATTERNS`] 中的任意一条。
fn matches_critical_pattern(command: &str) -> bool {
    !command.is_empty()
        && critical_patterns()
            .iter()
            .any(|pattern| pattern.is_match(command))
}

/// `bash` 工具的参数。
#[derive(Debug, Deserialize)]
struct BashArgs {
    /// 要执行的命令，交给 shell 解释，不由本工具自己拆分 argv。
    command: String,
    /// 超时秒数；省略时取工具构造时传入的默认值，取值最终会被钳制到
    /// `[MIN_TIMEOUT_SECS, MAX_TIMEOUT_SECS]`。
    #[serde(default)]
    timeout: Option<u64>,
    /// 命令的工作目录；省略时使用 [`ToolContext::cwd`]。
    #[serde(default)]
    cwd: Option<String>,
}

fn clamp_timeout(requested: u64) -> u64 {
    requested.clamp(MIN_TIMEOUT_SECS, MAX_TIMEOUT_SECS)
}

/// 在 shell 中执行任意命令。
#[derive(Debug)]
pub(crate) struct BashTool {
    workspace: Arc<Workspace>,
    default_timeout_secs: u64,
}

impl BashTool {
    /// 用工作区与工具配置构造。`config.bash_timeout_secs` 是省略 `timeout` 参数时的默认值，
    /// 同样会被钳制到 `[MIN_TIMEOUT_SECS, MAX_TIMEOUT_SECS]`。
    pub(crate) fn new(workspace: Arc<Workspace>, config: &ToolsConfig) -> Self {
        Self {
            workspace,
            default_timeout_secs: clamp_timeout(config.bash_timeout_secs),
        }
    }

    /// 解析可选的 `cwd` 参数。越界工作区根目录不是错误——`bash` 本来就是 `Tier::Exec`，
    /// 越界只值得提示，不值得单独再挡一次（挡不住：命令内部还是能 `cd` 到任何地方）。
    fn resolve_cwd(&self, ctx: &ToolContext, raw: Option<&str>) -> Result<PathBuf, ToolError> {
        let Some(raw) = raw else {
            return Ok(ctx.cwd.clone());
        };
        match self.workspace.resolve(raw) {
            Ok(resolved) => {
                if resolved.outside_root {
                    tracing::warn!(
                        cwd = %resolved.path.display(),
                        "bash 的 cwd 落在工作区根目录之外，仍按请求使用"
                    );
                }
                Ok(resolved.path)
            }
            Err(PathError::Empty) => Err(output::error("cwd 不能是空字符串。")),
            Err(PathError::NotUtf8) => Err(output::error("cwd 归一化后不是合法 UTF-8 路径。")),
        }
    }
}

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &'static str {
        "bash"
    }

    fn description(&self) -> &'static str {
        include_str!("./prompts/bash.md")
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The command to run in a shell.",
                },
                "timeout": {
                    "type": "integer",
                    "minimum": 1,
                    "description": format!(
                        "Timeout in seconds; defaults to {default} and is clamped to [{min}, {max}].",
                        default = self.default_timeout_secs,
                        min = MIN_TIMEOUT_SECS,
                        max = MAX_TIMEOUT_SECS,
                    ),
                },
                "cwd": {
                    "type": "string",
                    "description": "Working directory for this call; relative paths resolve against the workspace root.",
                },
            },
            "required": ["command"],
            "additionalProperties": false,
        })
    }

    fn approval(&self, args: &Value) -> ApprovalDecision {
        let command = args
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if matches_critical_pattern(command) {
            return ApprovalDecision::require_confirmation(
                Tier::Exec,
                "命中高危 bash 命令特征（递归删除根路径 / 远程拉取即执行 / 主机控制等），需要人工确认",
            );
        }
        ApprovalDecision::tier(Tier::Exec)
    }

    fn concurrency(&self, _args: &Value) -> Concurrency {
        // oh-my-pi 对非 PTY 的 bash 调用返回 shared（会话内多条命令能并跑，PTY 才独占
        // 终端，`packages/coding-agent/src/tools/bash.ts:605-606`）。本工具没有 PTY 分支，
        // 且 shell 命令普遍有副作用（写文件、改进程状态、起后台进程），不像 `read`/`grep`
        // 那样能安全断言"这次调用绝对只读"。倒向保守侧固定 `Exclusive`，与
        // `zcode_agent::tool::Tool::concurrency` 默认方向一致的准则相同：判定不了并发性
        // 的调用串行跑只是慢，并行跑可能是数据竞争（`crates/agent/src/tool/mod.rs:176-178`）。
        Concurrency::Exclusive
    }

    fn interruptible(&self, _args: &Value) -> bool {
        // bash 命令普遍有副作用（已经写了一半的文件、已经起来的后台进程），软中断
        // （用户插话）绝不能打断它们——只有硬取消（`ToolContext::cancel`）才杀。
        false
    }

    async fn execute(&self, args: Value, ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        let args: BashArgs = serde_json::from_value(args)
            .map_err(|err| output::error(format!("bash 参数不合法：{err}")))?;
        if args.command.trim().is_empty() {
            return Err(output::error("command 不能为空。"));
        }

        let cwd = self.resolve_cwd(&ctx, args.cwd.as_deref())?;
        let timeout_secs = clamp_timeout(args.timeout.unwrap_or(self.default_timeout_secs));

        let (shell_label, mut command) = build_shell_command(&args.command).await?;
        command
            .current_dir(&cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let result = Box::pin(run_and_stream(command, timeout_secs, &ctx)).await?;
        let output_text = String::from_utf8_lossy(&result.buffer).into_owned();

        let mut body = format!("$ {shell_label}\n{output_text}");
        if result.dropped_bytes > 0 {
            let _ = write!(
                body,
                "\n\n[已丢弃 {dropped} 字节原始输出——内联封顶是 {cap} 字节；\
                 最终展示给模型的内容还会再经过一轮独立的显示宽度/行数截断]",
                dropped = result.dropped_bytes,
                cap = MAX_INLINE_BYTES,
            );
        }

        match result.cause {
            Cause::Cancelled => Err(ToolError::Cancelled),
            Cause::TimedOut => {
                let _ = write!(
                    body,
                    "\n\n[命令超过 {timeout_secs} 秒超时上限，已终止整个进程树（不只是 shell 本身）]"
                );
                // `ToolError::Timeout` 只带 `seconds`，见模块文档「超时不丢已产出的输出」——
                // 已缓冲的输出改经 `ToolProgress::Status` 再报一次，避免这次调用在超时时
                // 唯一持有的内容随 `Err` 被吞掉、什么都没留下。
                ctx.report(ToolProgress::Status { text: body });
                Err(ToolError::Timeout {
                    seconds: timeout_secs,
                })
            }
            Cause::Completed(status) => {
                let exit_code = status.code();
                if exit_code == Some(0) {
                    Ok(output::finish(body, command_title(&args.command)))
                } else {
                    let code_text = exit_code.map_or_else(
                        || "未知（进程被信号终止）".to_owned(),
                        |code| code.to_string(),
                    );
                    let _ = write!(body, "\n\n[退出码: {code_text}]");
                    Err(ToolError::Failed(body))
                }
            }
        }
    }
}

/// 从命令文本裁出一句简短标题，供 [`ToolOutput::with_title`] 使用。
fn command_title(command: &str) -> String {
    const MAX_TITLE_CHARS: usize = 60;
    let normalized = command.split_whitespace().collect::<Vec<_>>().join(" ");
    let char_count = normalized.chars().count();
    let mut title: String = normalized.chars().take(MAX_TITLE_CHARS).collect();
    if char_count > MAX_TITLE_CHARS {
        title.push('…');
    }
    title
}

/// 构建执行命令的 [`Command`]，返回 `(展示用的 shell 标签, 已配置好的命令)`。
///
/// 允许拼接命令字符串的理由见模块文档开头一节。
#[cfg(unix)]
async fn build_shell_command(command_text: &str) -> Result<(String, Command), ToolError> {
    let mut command = Command::new("sh");
    command.arg("-c").arg(command_text);
    Ok(("sh -c".to_owned(), command))
}

/// Windows 版 shell 选择：优先 `pwsh`（PowerShell 7+），其次 Windows 自带的
/// `powershell`，两者都探测不到时兜底 `cmd /C`（几乎总是存在）。
#[cfg(windows)]
#[derive(Debug, Clone, Copy)]
enum WindowsShell {
    PowerShell(&'static str),
    Cmd,
}

#[cfg(windows)]
async fn windows_shell() -> WindowsShell {
    // 探测本身要 spawn 一个真实子进程，只做一次、按进程生命周期缓存——bash 每次调用都
    // 重新探测既浪费又会让"实际选中的 shell"在同一进程内变来变去。
    static DETECTED: tokio::sync::OnceCell<WindowsShell> = tokio::sync::OnceCell::const_new();
    *DETECTED
        .get_or_init(|| async {
            for candidate in ["pwsh", "powershell"] {
                let probe = Command::new(candidate)
                    .args([
                        "-NoLogo",
                        "-NoProfile",
                        "-NonInteractive",
                        "-Command",
                        "$null",
                    ])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .await;
                if probe.is_ok() {
                    return WindowsShell::PowerShell(candidate);
                }
            }
            WindowsShell::Cmd
        })
        .await
}

#[cfg(windows)]
async fn build_shell_command(command_text: &str) -> Result<(String, Command), ToolError> {
    match windows_shell().await {
        WindowsShell::PowerShell(bin) => {
            let mut command = Command::new(bin);
            command.args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                command_text,
            ]);
            Ok((format!("{bin} -Command"), command))
        }
        WindowsShell::Cmd => {
            let mut command = Command::new("cmd");
            command.args(["/C", command_text]);
            Ok(("cmd /C".to_owned(), command))
        }
    }
}

/// 增量 UTF-8 解码：把跨 chunk 读取截断的多字节字符正确地拼回来，避免流式输出里每个
/// 多字节字符边界都可能崩出一个替换字符。只用于喂给 [`ToolProgress::Chunk`] 的实时文本；
/// 攒进 [`StreamResult::buffer`] 的原始字节不需要这个（[`String::from_utf8_lossy`] 在最终
/// 解码时天然安全，最多在硬字节封顶的切点出现一个替换字符）。
#[derive(Debug, Default)]
struct Utf8ChunkDecoder {
    pending: Vec<u8>,
}

impl Utf8ChunkDecoder {
    fn push(&mut self, bytes: &[u8]) -> String {
        self.pending.extend_from_slice(bytes);
        match std::str::from_utf8(&self.pending) {
            Ok(text) => {
                let text = text.to_owned();
                self.pending.clear();
                text
            }
            Err(err) => {
                let valid_len = err.valid_up_to();
                let Some(valid) = self.pending.get(..valid_len) else {
                    return String::new();
                };
                let text = String::from_utf8_lossy(valid).into_owned();
                self.pending.drain(..valid_len);
                text
            }
        }
    }

    /// 流结束时把剩余字节（大概率是被截断的多字节字符，罕见情况下是命令输出本身就不是
    /// 合法 UTF-8）尽力转成文本，不留在缓冲区里悄悄丢掉。
    fn finish(&mut self) -> String {
        let text = String::from_utf8_lossy(&self.pending).into_owned();
        self.pending.clear();
        text
    }
}

/// 执行为什么结束的。
#[derive(Debug)]
enum Cause {
    Completed(std::process::ExitStatus),
    Cancelled,
    TimedOut,
}

/// [`run_and_stream`] 的产出。
struct StreamResult {
    /// 合并后的原始字节，已按 [`MAX_INLINE_BYTES`] 做头部截断。
    buffer: Vec<u8>,
    /// 因头部截断而丢弃的字节数；0 表示没有截断。
    dropped_bytes: usize,
    cause: Cause,
}

/// 累计一段新读到的字节：推送增量文本、按内联封顶决定是否继续写入 `buffer`。
fn record_chunk(
    raw: &[u8],
    decoder: &mut Utf8ChunkDecoder,
    buffer: &mut Vec<u8>,
    dropped_bytes: &mut usize,
    ctx: &ToolContext,
) {
    let text = decoder.push(raw);
    if !text.is_empty() {
        ctx.report(ToolProgress::Chunk { text });
    }

    let remaining = MAX_INLINE_BYTES.saturating_sub(buffer.len());
    if remaining == 0 {
        *dropped_bytes += raw.len();
    } else if raw.len() <= remaining {
        buffer.extend_from_slice(raw);
    } else if let Some(head) = raw.get(..remaining) {
        buffer.extend_from_slice(head);
        *dropped_bytes += raw.len() - remaining;
    } else {
        *dropped_bytes += raw.len();
    }
}

#[cfg(unix)]
mod unix_kill {
    /// 给整个进程组发信号。
    ///
    /// `-pgid` 是 POSIX `kill(2)` 的标准写法：pid 为负数时，信号发给 `|pid|` 指代的
    /// 整个进程组，而不是单个进程（`man 2 kill`："If pid is less than -1, then sig is
    /// sent to every process in the process group whose ID is -pid"）。
    ///
    /// # SAFETY
    /// `libc::kill` 是纯粹的系统调用封装，不涉及内存操作，也不解引用任何指针；失败
    /// （进程组已经不存在）用返回值判断而不是 panic——kill 必须对"进程已经死了"这种
    /// 情况保持幂等，重复调用无害。
    #[allow(unsafe_code)]
    pub(super) fn signal_process_group(pgid: i32, signal: i32) {
        unsafe {
            let _ = libc::kill(-pgid, signal);
        }
    }
}

#[cfg(windows)]
mod win_job {
    use std::io;

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject,
    };

    /// 一个 Windows Job Object：`setpgid`/`kill(-pgid, ...)` 在 Windows 上的等价物。
    /// 见模块文档「Windows 用 Job Object 而不是逐个 `TerminateProcess`」一节。
    #[derive(Debug)]
    pub(super) struct JobObject {
        handle: HANDLE,
    }

    // SAFETY: `HANDLE` 是内核对象句柄的不透明 ID，不是指向本进程内存的裸指针；跨线程
    // 转移它的所有权是 Win32 API 的标准用法。本类型只在下面几个方法里使用这个句柄，
    // 不会把它暴露给外部做未经同步的并发访问。
    #[allow(unsafe_code)]
    unsafe impl Send for JobObject {}

    impl JobObject {
        /// 创建一个匿名 job，并把 [`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`] 设成它的极限。
        #[allow(unsafe_code)] // FFI：Win32 Job Object API，见类型级 SAFETY 说明
        pub(super) fn create() -> io::Result<Self> {
            // SAFETY: `lpjobattributes = null`（默认、不可继承的安全描述符）与
            // `lpname = null`（匿名 job）都是 `CreateJobObjectW` 文档允许的合法输入；
            // 失败时返回值为 null，用它判定失败而不是依赖未初始化内存。
            let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if handle.is_null() {
                return Err(io::Error::last_os_error());
            }

            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let len = u32::try_from(size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>())
                .unwrap_or(u32::MAX);
            // SAFETY: `info` 是本函数栈上刚初始化的合法结构体，`len` 精确等于它的大小，
            // 与 `JobObjectExtendedLimitInformation` 这个 information class 要求的结构体
            // 类型一致（Win32 文档：`JOBOBJECT_EXTENDED_LIMIT_INFORMATION`）。
            let ok = unsafe {
                SetInformationJobObject(
                    handle,
                    JobObjectExtendedLimitInformation,
                    std::ptr::addr_of!(info).cast(),
                    len,
                )
            };
            if ok == 0 {
                let err = io::Error::last_os_error();
                // SAFETY: `handle` 是本函数刚创建、还没交给任何人的句柄，关闭它是唯一
                // 持有者的正常清理路径。
                unsafe {
                    CloseHandle(handle);
                }
                return Err(err);
            }

            Ok(Self { handle })
        }

        /// 把一个仍存活的进程纳入这个 job。
        #[allow(unsafe_code)]
        pub(super) fn assign(&self, process: HANDLE) -> io::Result<()> {
            // SAFETY: `self.handle` 由 `create()` 创建并全程有效；`process` 由调用方
            // 保证是子进程尚未回收时取得的句柄（`Child::raw_handle()`）。
            let ok = unsafe { AssignProcessToJobObject(self.handle, process) };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }

        /// 终止 job 里的整棵进程树。幂等：进程已经不在 job 里（已退出）时 Win32 返回
        /// 失败，按"已经死了"处理，不 panic、不重试。
        #[allow(unsafe_code)]
        pub(super) fn kill_tree(&self) {
            // SAFETY: `self.handle` 全程由本类型持有并保证有效；退出码只用于记账，不
            // 影响任何调用方逻辑，失败（比如已经没有存活进程）静默忽略即可。
            unsafe {
                let _ = TerminateJobObject(self.handle, 1);
            }
        }
    }

    impl Drop for JobObject {
        #[allow(unsafe_code)]
        fn drop(&mut self) {
            // SAFETY: `self.handle` 由 `create()` 创建、只在本类型内使用，`Drop` 保证
            // 只关闭一次。`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` 保证关闭时整棵树被杀，
            // 即便调用方从没显式调用过 `kill_tree()`（正常完成路径也一样安全收尾）。
            unsafe {
                CloseHandle(self.handle);
            }
        }
    }
}

/// 触发终止流程的原因，一旦确定就不会再变（取消与超时谁先到算谁）。
enum PendingCause {
    Cancelled,
    TimedOut,
}

impl PendingCause {
    fn into_cause(self) -> Cause {
        match self {
            Self::TimedOut => Cause::TimedOut,
            Self::Cancelled => Cause::Cancelled,
        }
    }
}

/// spawn 命令、按到达顺序流式转发输出，并在取消/超时时终止整个进程组/job。
///
/// 取消与超时共用同一套终止流程：先发"温和"信号（Unix `SIGTERM`/Windows
/// `TerminateJobObject`——Windows 没有分级信号，一步到位），等 [`KILL_GRACE`]；仍未退出
/// 就升级（Unix 补 `SIGKILL`；Windows 没有下一级，放弃等待）。全程继续读 stdout/stderr，
/// 不会因为进入终止流程就停止转发——进程在收到信号后仍可能再输出几行。
///
/// 单个状态机横跨"spawn / 流式读取 / 取消・超时升级 / 收尾"四个阶段，拆成多个函数只会
/// 把状态（`buffer`/`*_open`/`exit_status`/`kill_stage`…）拆散到几处调用之间来回传递，
/// 可读性反而更差；保留一个较长的函数体。
#[allow(clippy::too_many_lines)]
async fn run_and_stream(
    mut command: Command,
    timeout_secs: u64,
    ctx: &ToolContext,
) -> Result<StreamResult, ToolError> {
    #[cfg(unix)]
    command.process_group(0);

    #[cfg(windows)]
    let job = win_job::JobObject::create()
        .map_err(|err| output::error(format!("创建 Windows Job Object 失败：{err}")))?;

    let mut child = command
        .spawn()
        .map_err(|err| output::error(format!("启动 shell 失败：{err}")))?;

    #[cfg(windows)]
    {
        let handle = child
            .raw_handle()
            .ok_or_else(|| output::error("子进程刚启动就已退出，拿不到进程句柄"))?;
        if let Err(err) = job.assign(handle) {
            let _ = child.start_kill();
            return Err(output::error(format!(
                "把子进程装进 Job Object 失败，已终止已起进程：{err}"
            )));
        }
    }

    #[cfg(unix)]
    let pgid: Option<i32> = child.id().and_then(|pid| i32::try_from(pid).ok());

    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| output::error("子进程缺少 stdout 管道"))?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| output::error("子进程缺少 stderr 管道"))?;

    let mut buffer: Vec<u8> = Vec::new();
    let mut dropped_bytes: usize = 0;
    let mut stdout_open = true;
    let mut stderr_open = true;
    let mut stdout_decoder = Utf8ChunkDecoder::default();
    let mut stderr_decoder = Utf8ChunkDecoder::default();
    let mut exit_status: Option<std::process::ExitStatus> = None;

    let mut pending_cause: Option<PendingCause> = None;
    let mut terminating = false;
    let mut kill_stage: u8 = 0;

    let deadline_sleep = tokio::time::sleep(Duration::from_secs(timeout_secs));
    tokio::pin!(deadline_sleep);
    // 终止阶段用的宽限计时器；`terminating` 变 true 之前从不会被 `select!` 轮询到，
    // 初始时长无所谓。
    let term_sleep = tokio::time::sleep(Duration::ZERO);
    tokio::pin!(term_sleep);

    let mut stdout_buf = [0_u8; 8192];
    let mut stderr_buf = [0_u8; 8192];

    let cause = loop {
        if pending_cause.is_none()
            && let Some(status) = exit_status
            && !stdout_open
            && !stderr_open
        {
            break Cause::Completed(status);
        }

        tokio::select! {
            biased;

            () = &mut deadline_sleep, if pending_cause.is_none() => {
                pending_cause = Some(PendingCause::TimedOut);
            }
            () = ctx.cancel.notified(), if pending_cause.is_none() => {
                pending_cause = Some(PendingCause::Cancelled);
            }
            () = &mut term_sleep, if terminating => {
                kill_stage += 1;
                #[cfg(unix)]
                let advanced = match (pgid, kill_stage) {
                    (Some(pgid), 1) => { unix_kill::signal_process_group(pgid, libc::SIGTERM); true }
                    (Some(pgid), 2) => { unix_kill::signal_process_group(pgid, libc::SIGKILL); true }
                    _ => false,
                };
                #[cfg(windows)]
                let advanced = match kill_stage {
                    1 => { job.kill_tree(); true }
                    _ => false,
                };
                if advanced {
                    term_sleep.as_mut().reset(Instant::now() + KILL_GRACE);
                } else {
                    // 升级手段已经用尽，仍然没能确认进程退出——不能无限等，放弃等待，
                    // 把已经缓冲到的输出如实返回。
                    break pending_cause.map_or(Cause::Cancelled, PendingCause::into_cause);
                }
            }
            result = stdout.read(&mut stdout_buf), if stdout_open => {
                match result {
                    Ok(0) | Err(_) => {
                        stdout_open = false;
                        let tail = stdout_decoder.finish();
                        if !tail.is_empty() {
                            ctx.report(ToolProgress::Chunk { text: tail });
                        }
                    }
                    Ok(n) => {
                        if let Some(chunk) = stdout_buf.get(..n) {
                            record_chunk(chunk, &mut stdout_decoder, &mut buffer, &mut dropped_bytes, ctx);
                        }
                    }
                }
            }
            result = stderr.read(&mut stderr_buf), if stderr_open => {
                match result {
                    Ok(0) | Err(_) => {
                        stderr_open = false;
                        let tail = stderr_decoder.finish();
                        if !tail.is_empty() {
                            ctx.report(ToolProgress::Chunk { text: tail });
                        }
                    }
                    Ok(n) => {
                        if let Some(chunk) = stderr_buf.get(..n) {
                            record_chunk(chunk, &mut stderr_decoder, &mut buffer, &mut dropped_bytes, ctx);
                        }
                    }
                }
            }
            status = child.wait(), if exit_status.is_none() => {
                exit_status = status.ok();
                // `wait()` 本身失败是极罕见的边缘情况（例如已经被别处 reap 过）；`exit_status`
                // 留空即可——循环靠 stdout/stderr 都关闭后自然收尾，`status.code()` 读不到时
                // 上层按"未知退出码"呈现，不需要在这里构造一个人工的、内容虚构的退出状态。
            }
        }

        if pending_cause.is_some() {
            if !terminating {
                terminating = true;
                kill_stage = 0;
                term_sleep.as_mut().reset(Instant::now());
            }
            if exit_status.is_some() && !stdout_open && !stderr_open {
                break pending_cause.map_or(Cause::Cancelled, PendingCause::into_cause);
            }
        }
    };

    Ok(StreamResult {
        buffer,
        dropped_bytes,
        cause,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tempfile::tempdir;
    use tokio::sync::mpsc;
    use zcode_agent::{EntryId, InterruptSignal, SessionId};

    use super::*;

    fn tools_config(timeout_secs: u64) -> ToolsConfig {
        ToolsConfig {
            disabled: Vec::new(),
            bash_timeout_secs: timeout_secs,
            read_max_lines: 300,
        }
    }

    fn test_ctx(cwd: PathBuf) -> (ToolContext, mpsc::UnboundedReceiver<ToolProgress>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let ctx = ToolContext {
            session_id: SessionId::generate(),
            entry_id: EntryId::generate(),
            call_id: "call-1".to_owned(),
            cwd,
            cancel: InterruptSignal::new(),
            steering: InterruptSignal::new(),
            progress: tx,
        };
        (ctx, rx)
    }

    fn drain_progress_text(rx: &mut mpsc::UnboundedReceiver<ToolProgress>) -> String {
        let mut text = String::new();
        while let Ok(progress) = rx.try_recv() {
            match progress {
                ToolProgress::Chunk { text: chunk } | ToolProgress::Status { text: chunk } => {
                    text.push_str(&chunk);
                }
            }
        }
        text
    }

    #[cfg(unix)]
    fn echo_command() -> &'static str {
        "echo hello-from-bash"
    }
    #[cfg(windows)]
    fn echo_command() -> &'static str {
        "Write-Output hello-from-bash"
    }

    #[cfg(unix)]
    fn nonzero_exit_command() -> &'static str {
        "exit 7"
    }
    #[cfg(windows)]
    fn nonzero_exit_command() -> &'static str {
        "exit 7"
    }

    #[tokio::test]
    async fn success_command_reports_stdout() {
        let dir = tempdir().expect("创建临时目录");
        let workspace = Arc::new(Workspace::new(dir.path().to_path_buf()));
        let tool = BashTool::new(workspace, &tools_config(30));
        let (ctx, mut rx) = test_ctx(dir.path().to_path_buf());

        let output = tool
            .execute(json!({ "command": echo_command() }), ctx)
            .await
            .expect("成功命令不应该失败");

        let text = match output.content.first() {
            Some(zcode_agent::StoredToolResultContent::Text { text }) => text.clone(),
            other => panic!("预期文本内容，实际是 {other:?}"),
        };
        assert!(
            text.contains("hello-from-bash"),
            "输出应包含 stdout 内容，实际是 {text}"
        );

        let streamed = drain_progress_text(&mut rx);
        assert!(
            streamed.contains("hello-from-bash"),
            "应通过 ToolProgress::Chunk 流式转发同样的内容，实际收到 {streamed}"
        );
    }

    #[tokio::test]
    async fn nonzero_exit_becomes_failed_with_exit_code_and_output() {
        let dir = tempdir().expect("创建临时目录");
        let workspace = Arc::new(Workspace::new(dir.path().to_path_buf()));
        let tool = BashTool::new(workspace, &tools_config(30));
        let (ctx, _rx) = test_ctx(dir.path().to_path_buf());

        let err = tool
            .execute(json!({ "command": nonzero_exit_command() }), ctx)
            .await
            .expect_err("非零退出码应该变成 Err");

        match err {
            ToolError::Failed(message) => {
                assert!(
                    message.contains('7'),
                    "错误文本应带退出码，实际是 {message}"
                );
            }
            other => panic!("预期 ToolError::Failed，实际是 {other:?}"),
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn timeout_returns_timeout_error_with_buffered_output() {
        let dir = tempdir().expect("创建临时目录");
        let workspace = Arc::new(Workspace::new(dir.path().to_path_buf()));
        let tool = BashTool::new(workspace, &tools_config(30));
        let (ctx, mut rx) = test_ctx(dir.path().to_path_buf());

        let err = tool
            .execute(
                json!({ "command": "echo before-timeout; sleep 5", "timeout": 1 }),
                ctx,
            )
            .await
            .expect_err("超过 timeout 应该失败");

        match err {
            ToolError::Timeout { seconds } => assert_eq!(seconds, 1),
            other => panic!("预期 ToolError::Timeout，实际是 {other:?}"),
        }

        let streamed = drain_progress_text(&mut rx);
        assert!(
            streamed.contains("before-timeout"),
            "超时前的输出必须能通过 progress 通道被看到，实际收到 {streamed}"
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn timeout_returns_timeout_error_with_buffered_output() {
        let dir = tempdir().expect("创建临时目录");
        let workspace = Arc::new(Workspace::new(dir.path().to_path_buf()));
        let tool = BashTool::new(workspace, &tools_config(30));
        let (ctx, mut rx) = test_ctx(dir.path().to_path_buf());

        let err = tool
            .execute(
                json!({
                    "command": "Write-Output before-timeout; Start-Sleep -Seconds 5",
                    "timeout": 1,
                }),
                ctx,
            )
            .await
            .expect_err("超过 timeout 应该失败");

        match err {
            ToolError::Timeout { seconds } => assert_eq!(seconds, 1),
            other => panic!("预期 ToolError::Timeout，实际是 {other:?}"),
        }

        let streamed = drain_progress_text(&mut rx);
        assert!(
            streamed.contains("before-timeout"),
            "超时前的输出必须能通过 progress 通道被看到，实际收到 {streamed}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancel_kills_grandchild_process() {
        // 外层命令自己再起一个后台子进程（`sleep`），并把它的 PID 写进文件——这个
        // `sleep` 相对 zcode 进程是"孙进程"（zcode -> sh -> sleep）。取消之后断言这个
        // PID 已经不存在，证明进程组 kill 真的传导到了孙进程，而不是只杀了 `sh` 本身。
        let dir = tempdir().expect("创建临时目录");
        let pid_file = dir.path().join("grandchild.pid");
        let workspace = Arc::new(Workspace::new(dir.path().to_path_buf()));
        let tool = BashTool::new(workspace, &tools_config(30));
        let (ctx, _rx) = test_ctx(dir.path().to_path_buf());
        let cancel = ctx.cancel.clone();

        let command = format!("sleep 30 & echo $! > {}; wait", pid_file.display());
        let exec =
            tokio::spawn(async move { tool.execute(json!({ "command": command }), ctx).await });

        // 等 PID 文件出现，确认孙进程真的起来了，再触发取消。
        let grandchild_pid: i32 = wait_for_pid_file(&pid_file).await;
        cancel.fire();

        let err = exec
            .await
            .expect("execute 任务不应该 panic")
            .expect_err("取消应该失败");
        assert!(
            matches!(err, ToolError::Cancelled),
            "预期 ToolError::Cancelled，实际是 {err:?}"
        );

        assert!(
            !process_alive(grandchild_pid),
            "取消之后孙进程 {grandchild_pid} 应该已经被杀掉"
        );
    }

    #[cfg(unix)]
    async fn wait_for_pid_file(path: &std::path::Path) -> i32 {
        for _ in 0..100 {
            if let Ok(text) = tokio::fs::read_to_string(path).await {
                if let Ok(pid) = text.trim().parse::<i32>() {
                    return pid;
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("等待孙进程 PID 文件超时：{}", path.display());
    }

    /// 用信号 0 探活：不发送真实信号，只检查目标进程是否存在、当前用户是否有权限
    /// 给它发信号（`man 2 kill`）。
    ///
    /// # SAFETY
    /// 同生产代码里的 `unix_kill::signal_process_group`：纯系统调用封装，不涉及内存
    /// 操作，用返回值判断结果。
    #[cfg(unix)]
    #[allow(unsafe_code)]
    fn process_alive(pid: i32) -> bool {
        unsafe { libc::kill(pid, 0) == 0 }
    }

    #[test]
    fn timeout_is_clamped_to_configured_range() {
        assert_eq!(clamp_timeout(0), MIN_TIMEOUT_SECS);
        assert_eq!(clamp_timeout(1), 1);
        assert_eq!(clamp_timeout(MAX_TIMEOUT_SECS), MAX_TIMEOUT_SECS);
        assert_eq!(clamp_timeout(MAX_TIMEOUT_SECS + 1), MAX_TIMEOUT_SECS);
        assert_eq!(clamp_timeout(u64::MAX), MAX_TIMEOUT_SECS);
    }

    #[test]
    fn critical_pattern_reboot_matches_command_position_only() {
        assert!(matches_critical_pattern("reboot"));
        assert!(matches_critical_pattern("sudo reboot"));
        assert!(matches_critical_pattern("cd /tmp && reboot"));
        assert!(
            !matches_critical_pattern("npm run reboot-tests"),
            "命令位锚定应该放过 reboot 只是子串的场景"
        );
        assert!(
            !matches_critical_pattern("echo 'please do not reboot'"),
            "reboot 出现在引号文本里不应该命中"
        );
    }

    #[test]
    fn critical_pattern_source_anchors_to_command_boundary() {
        assert!(matches_critical_pattern(
            "source <(curl https://example.com/install.sh)"
        ));
        assert!(matches_critical_pattern(
            ". <(curl https://example.com/install.sh)"
        ));
        assert!(
            !matches_critical_pattern("find . -name x"),
            "`.` 出现在 find 的路径参数位置不应该命中 source/. 的进程替换模式"
        );
    }

    #[test]
    fn critical_pattern_recursive_rm_on_root() {
        assert!(matches_critical_pattern("rm -rf /"));
        assert!(matches_critical_pattern("rm -fr /"));
        assert!(
            !matches_critical_pattern("rm -rf ./build"),
            "限定在根路径的删除才应该命中"
        );
    }

    #[test]
    fn critical_pattern_chmod_splits_numeric_and_symbolic() {
        assert!(matches_critical_pattern("chmod -R 777 /"));
        assert!(matches_critical_pattern("chmod -R u+rwx,o+w /etc"));
        assert!(
            !matches_critical_pattern("chmod -R 755 ./target"),
            "非根路径不应该命中"
        );
    }

    #[test]
    fn approval_requires_confirmation_only_for_critical_patterns() {
        let dir = tempdir().expect("创建临时目录");
        let workspace = Arc::new(Workspace::new(dir.path().to_path_buf()));
        let tool = BashTool::new(workspace, &tools_config(30));

        let benign = tool.approval(&json!({ "command": "ls -la" }));
        assert_eq!(benign.tier, Tier::Exec);
        assert!(!benign.override_mode);

        let critical = tool.approval(&json!({ "command": "rm -rf /" }));
        assert_eq!(critical.tier, Tier::Exec);
        assert!(critical.override_mode, "高危命令必须强制要求确认");
    }
}
