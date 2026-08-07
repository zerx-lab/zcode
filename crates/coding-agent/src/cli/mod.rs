//! clap 命令面与分发。
//!
//! # 只有一条执行路径
//!
//! 每个会真正跑 agent 的子命令（默认命令、`run`）都走同一串装配：
//!
//! ```text
//! config::load → Workspace → host::connect::connect → ClientSession::open
//!              → 选定 SessionId → render::run_headless | app::run_tui
//! ```
//!
//! `connect` 内部决定是连真 daemon 还是 `stream_pair()` 自托管，两条分支之后的一切
//! 完全一致（`plans/runtime-boundary/README.md:195` 已裁决）。**绝不**为 headless 另开
//! 一条「直接建 `AgentRuntime`」的近路——同文档 `:179` 把它列为不抄项，理由是
//! 两套执行路径 = 两套 bug。
//!
//! # 会话选择在这一层，不在客户端
//!
//! `--resume` / `--continue` / 新建 是 CLI flag 语义。让 [`crate::render`] 与
//! [`crate::app`] 各自去 `SessionList` → 挑一条 → `SessionCreate`，等于把同一段逻辑写两遍，
//! 第一次修改 `--continue` 的定义就会漂移。本模块选好 `SessionId` 再交下去。
//!
//! # stdout 边界
//!
//! `config` / `models` / `auth` / `session list` 这几条**执行后即退出、不进 TUI、
//! 不与协议共享 stdout**，因此允许直接写 stdout。会进 TUI 或协议的路径一律 `tracing`。
//! 这个例外由语义而非文件名决定（`rule://zcode-architecture` 的「日志与 CLI 输出边界」）。

pub(crate) mod worker;

use std::io::{IsTerminal as _, Write as _};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result, bail};
use clap::{Args, Parser, Subcommand};
use futures_util::FutureExt as _;
use tracing_subscriber::EnvFilter;
use zcode_ai::auth::oauth::BrowserPrompt;
use zcode_ai::{AuthStore, ProviderId};
use zcode_protocol::wire::types::{ClientId, SessionId, SessionSummary};
use zcode_protocol::{Reply, Request};

use crate::config::{self, Config};
use crate::host::HostDeps;
use crate::host::connect::{self, ClientSession, ConnectError};
use crate::render::OutputFormat;
use crate::workspace::Workspace;

/// ZCode —— 终端里的 coding agent。
///
/// # 为什么**没有** `args_conflicts_with_subcommands`
///
/// 那个开关看起来正好用来消解"`zcode run x` 里的 `run` 既像子命令又像提示词"这个歧义，
/// 实际语义是「本命令**任何**参数一旦出现就拒绝子命令」——全局 flag 也算数。
/// 于是 `zcode --cwd X serve` 里的 `serve` 被塞进 [`Cli::prompt`]，命令变成"用提示词
/// `serve` 跑一轮对话"。真机后果是 **daemon fork 炸弹**：`spawn_daemon` 拉起的
/// `zcode --cwd X serve` 不去当 daemon，而是又跑一次客户端流程、发现没有 daemon、
/// 再 spawn 一个……进程表里迅速堆满 `zcode.exe`，客户端永远等不到就绪。
///
/// 不加这个开关时 clap 的默认行为本来就是对的：argv 第一个非 flag 词优先匹配子命令，
/// 匹配不上才落进 `trailing_var_arg` 的提示词。下面 `subcommand_wins_over_trailing_prompt`
/// 与 `global_flag_before_subcommand_still_routes_to_subcommand` 两个测试钉住这两条。
#[derive(Debug, Parser)]
#[command(name = "zcode", version, about, long_about = None)]
struct Cli {
    #[command(flatten)]
    global: GlobalArgs,

    #[command(subcommand)]
    command: Option<Command>,

    /// 初始提示词。给了就在会话建立后立即发出去。
    #[arg(trailing_var_arg = true)]
    prompt: Vec<String>,

    /// 不进 TUI，把回答打到 stdout 后退出。
    #[arg(short = 'p', long, global = true)]
    print: bool,

    /// `--print` 时改用 NDJSON 输出（每个事件一行）。
    #[arg(long, global = true)]
    json: bool,
}

/// 所有子命令共享的全局选项。
#[derive(Debug, Args)]
struct GlobalArgs {
    /// 模型 id 或可唯一匹配的简写。
    #[arg(short = 'm', long, global = true)]
    model: Option<String>,

    /// 切换工作目录后再执行。
    #[arg(short = 'C', long, global = true, value_name = "DIR")]
    cwd: Option<PathBuf>,

    /// 审批模式：`yolo` / `write` / `always-ask`。
    #[arg(long, global = true, value_name = "MODE")]
    approval: Option<String>,

    /// 直接指定一份配置文件，跳过两层发现。
    #[arg(long, global = true, value_name = "FILE")]
    config: Option<PathBuf>,

    /// 不使用 daemon，本进程自托管。
    #[arg(long, global = true)]
    no_daemon: bool,

    /// 续接该工作目录下最近一次会话。
    #[arg(long, global = true)]
    continue_session: bool,

    /// 续接指定 id 的会话。
    #[arg(long, global = true, value_name = "ID")]
    resume: Option<String>,

    /// 把日志打到 stderr 而不是日志文件。
    #[arg(short = 'v', long, global = true)]
    verbose: bool,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// 非交互跑一轮：把回答打到 stdout 后退出。
    Run {
        /// 提示词。留空则从 stdin 读。
        #[arg(trailing_var_arg = true)]
        message: Vec<String>,
    },
    /// 以 daemon 方式常驻，供多个客户端连接。
    Serve,
    /// 凭据管理。
    Auth {
        #[command(subcommand)]
        action: AuthAction,
    },
    /// 列出当前凭据可用的模型。
    Models,
    /// 查看配置。
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// 会话管理。
    Session {
        #[command(subcommand)]
        action: SessionAction,
    },
}

#[derive(Debug, Subcommand)]
enum AuthAction {
    /// 跑一遍交互式登录。
    Login {
        /// 提供商：`anthropic` / `openai-codex` / `xai-oauth`。
        provider: String,
    },
    /// 删除某提供商的凭据。
    Logout {
        /// 提供商。
        provider: String,
    },
    /// 列出各提供商的凭据状态。
    List,
}

#[derive(Debug, Subcommand)]
enum ConfigAction {
    /// 打印配置文件的发现路径。
    Path,
    /// 打印合并后的生效配置。
    Show,
}

#[derive(Debug, Subcommand)]
enum SessionAction {
    /// 列出当前工作目录下的会话。
    List,
}

/// 进程主流程。
pub(crate) async fn run() -> Result<ExitCode> {
    let argv: Vec<String> = std::env::args().collect();

    // 必须早于 `Cli::parse()`：clap 不认识 `__zcode_worker_*`，先让它看到就会以
    // usage error 退出，而那段 usage 文本会打进父子之间的协议管道。
    worker::dispatch(&argv)?;

    let cli = Cli::parse();
    let cwd = resolve_cwd(cli.global.cwd.as_deref())?;
    let config = Arc::new(load_config(&cwd, &cli.global)?);
    init_logging(&config, cli.global.verbose)?;

    let code = match cli.command {
        Some(Command::Serve) => cmd_serve(&cwd, &config, &cli.global).await?,
        Some(Command::Auth { action }) => cmd_auth(action).await?,
        Some(Command::Models) => cmd_models(&config, &cli.global)?,
        Some(Command::Config { action }) => cmd_config(&cwd, &config, &action)?,
        Some(Command::Session { action }) => {
            cmd_session(&cwd, &config, &cli.global, action).await?
        }
        Some(Command::Run { message }) => {
            let prompt = read_prompt(message).await?;
            cmd_agent(
                &cwd,
                &config,
                &cli.global,
                prompt,
                Interface::headless(cli.json),
            )
            .await?
        }
        None => {
            let prompt = read_prompt(cli.prompt).await?;
            let interface = if cli.print || !std::io::stdout().is_terminal() {
                Interface::headless(cli.json)
            } else {
                Interface::Tui
            };
            cmd_agent(&cwd, &config, &cli.global, prompt, interface).await?
        }
    };

    Ok(exit_code(code))
}

/// 本次运行用哪个客户端。
#[derive(Debug, Clone, Copy)]
enum Interface {
    /// 非交互：打完就退。
    Headless(OutputFormat),
    /// 交互式终端界面。
    Tui,
}

impl Interface {
    const fn headless(json: bool) -> Self {
        Self::Headless(if json {
            OutputFormat::Json
        } else {
            OutputFormat::Text
        })
    }
}

/// `i32` 退出码转成进程退出码。
///
/// 约定的取值只有 0 / 1 / 130，全部落在 `u8` 内；越界说明调用方违反了约定，
/// 此时钳到 1 并告警——`as` 会把 256 静默变成 0，把失败伪装成成功。
fn exit_code(code: i32) -> ExitCode {
    let Ok(byte) = u8::try_from(code) else {
        tracing::warn!(code, "退出码超出 u8 范围，钳到 1");
        return ExitCode::from(1);
    };
    ExitCode::from(byte)
}

fn resolve_cwd(requested: Option<&std::path::Path>) -> Result<PathBuf> {
    let cwd = match requested {
        Some(dir) => dir.to_path_buf(),
        None => std::env::current_dir().context("读取当前工作目录失败")?,
    };
    if !cwd.is_dir() {
        bail!("工作目录不存在：{}", cwd.display());
    }
    Ok(cwd)
}

/// 加载配置并叠上 CLI flag。
///
/// flag 是优先级链的最后一层（`config::load` 只做到环境变量那一层），
/// 因此这里的覆盖必须发生在 `load` 之后。
fn load_config(cwd: &std::path::Path, global: &GlobalArgs) -> Result<Config> {
    let mut config = config::load(cwd).context("加载配置失败")?;
    if let Some(model) = &global.model {
        config.model.id = Some(model.clone());
    }
    if let Some(mode) = &global.approval {
        config.approval.mode = parse_approval_mode(mode)?;
    }
    if global.no_daemon {
        config.daemon.enabled = false;
    }
    Ok(config)
}

fn parse_approval_mode(raw: &str) -> Result<zcode_agent::ApprovalMode> {
    // `ApprovalMode` 的 serde 表示是 kebab-case（`crates/agent/src/approval.rs:66`），
    // 复用它而不是另写一张映射表：两张表迟早会漂移。
    serde_json::from_value(serde_json::Value::String(raw.to_owned()))
        .with_context(|| format!("审批模式 `{raw}` 无效，可选：yolo / write / always-ask"))
}

/// 日志落点。
///
/// 默认写文件：TUI 活动期间 stderr 归渲染，日志混进去会撕碎画面
/// （`rule://zcode-architecture` 的日志边界）。`--verbose` 才切到 stderr，
/// 那是明确的调试意图。
fn init_logging(config: &Config, verbose: bool) -> Result<()> {
    let filter = EnvFilter::try_from_env("ZCODE_LOG")
        .unwrap_or_else(|_| EnvFilter::new(if verbose { "debug" } else { "info" }));

    if verbose {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(std::io::stderr)
            .try_init()
            .map_err(|error| anyhow::anyhow!("初始化日志失败：{error}"))?;
        return Ok(());
    }

    let dir = config.daemon.runtime_dir.join("logs");
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("创建日志目录 {} 失败", dir.display()))?;
    let path = dir.join("zcode.log");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("打开日志文件 {} 失败", path.display()))?;

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        .with_writer(Mutex::new(file))
        .try_init()
        .map_err(|error| anyhow::anyhow!("初始化日志失败：{error}"))
}

/// 取本轮提示词：命令行参数优先，其次是被管道喂进来的 stdin。
///
/// stdin 只在**非 TTY**时读。TTY 上读 stdin 会让 `zcode` 裸启动时挂住等输入，
/// 而那正是要进 TUI 的场景。
async fn read_prompt(args: Vec<String>) -> Result<Option<String>> {
    if !args.is_empty() {
        return Ok(Some(args.join(" ")));
    }
    if std::io::stdin().is_terminal() {
        return Ok(None);
    }
    let mut buffer = String::new();
    tokio::io::AsyncReadExt::read_to_string(&mut tokio::io::stdin(), &mut buffer)
        .await
        .context("读取 stdin 失败")?;
    let trimmed = buffer.trim();
    Ok(if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    })
}

/// 每进程一个的客户端实例 id。
///
/// 用途是接管仲裁：运行时靠它区分「同一个客户端重连」与「第二个客户端来抢会话」。
/// 因此**进程重启后必须换一个新的**（`crates/protocol/src/wire/types.rs:93-97`），
/// pid 会被 OS 复用，单靠 pid 会让新进程被误认成还活着的老连接——加上启动时刻的
/// 纳秒数即可区分。这里不需要密码学随机性：它不是凭据，只是身份标签。
fn new_client_id() -> ClientId {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    ClientId::from(format!("cli-{}-{nanos}", std::process::id()))
}

/// 跑 agent：装配 → 连接 → 选会话 → 交给客户端。
async fn cmd_agent(
    cwd: &std::path::Path,
    config: &Arc<Config>,
    global: &GlobalArgs,
    prompt: Option<String>,
    interface: Interface,
) -> Result<i32> {
    let workspace = Arc::new(Workspace::new(crate::workspace::detect_root(cwd)));
    let model =
        crate::model::resolve_model(config, global.model.as_deref()).context("解析模型失败")?;

    let connection = {
        // 闭包要 move 走一份 `Arc`，而 `connect` 的前两个参数同时借着外层这份——
        // 同一个绑定既借又移会撞 E0505，所以给闭包单独克隆一份。
        let deps_config = Arc::clone(config);
        let deps_workspace = Arc::clone(&workspace);
        connect::connect(config, workspace.root(), move |secret| {
            build_deps(deps_config, deps_workspace, secret).boxed()
        })
        .await
        .context("连接 agent 运行时失败")?
    };

    let session = ClientSession::open(connection)
        .await
        .context("与运行时握手失败")?;

    let root = workspace.root().to_string_lossy().into_owned();
    let target = select_session(&session, &root, &model.id, global).await?;
    let client = new_client_id();

    match interface {
        Interface::Headless(format) => {
            let prompt = prompt.context(
                "非交互模式需要提示词：作为参数给出，或用管道喂给 stdin（`echo ... | zcode -p`）",
            )?;
            crate::render::run_headless(
                session,
                target,
                client,
                prompt,
                format,
                config.ui.show_thinking,
            )
            .await
            .context("headless 运行失败")
        }
        Interface::Tui => {
            crate::app::run_tui(session, target, client, prompt, config.ui.show_thinking)
                .await
                .context("TUI 运行失败")
        }
    }
}

/// 装配 `HostDeps`。
///
/// **只有自托管路径才会真的调用它**：连上真 daemon 时 provider、工具注册表、
/// system prompt 一个都不建（`plans/runtime-boundary/implementation.md:83-85`
/// 的「客户端进程不初始化 provider」）。这也是它为什么是个惰性闭包而不是提前算好的值。
async fn build_deps(
    config: Arc<Config>,
    workspace: Arc<Workspace>,
    secret: zcode_utils::daemon::Secret,
) -> Result<HostDeps, ConnectError> {
    let (provider, model) = crate::model::build(config.as_ref(), None)
        .await
        .map_err(|error| ConnectError::Deps(error.into()))?;
    let registry = crate::tools::registry(config.as_ref(), &workspace)
        .map_err(|error| ConnectError::Deps(error.into()))?;
    let prompts = crate::prompt::build(workspace.as_ref(), config.as_ref(), &model)
        .await
        .map_err(|error| ConnectError::Deps(error.into()))?;

    Ok(HostDeps {
        provider,
        registry: Arc::new(registry),
        sessions_dir: config.session.dir.clone(),
        config,
        prompts: Arc::new(prompts),
        model,
        workspace,
        secret,
    })
}

/// 按 flag 选定要用的会话：`--resume` > `--continue` > 新建。
async fn select_session(
    session: &ClientSession,
    cwd: &str,
    model: &str,
    global: &GlobalArgs,
) -> Result<SessionId> {
    if let Some(id) = &global.resume {
        // 不做前缀匹配：`--resume` 是显式指定，模糊匹配到别的会话比报错糟得多。
        return Ok(SessionId::from(id.clone()));
    }

    if global.continue_session {
        let sessions = list_sessions(session, cwd).await?;
        match sessions.first() {
            Some(latest) => return Ok(latest.id.clone()),
            None => bail!("{cwd} 下没有可续接的会话；去掉 --continue 即可新建"),
        }
    }

    match session
        .request(Request::SessionCreate {
            cwd: cwd.to_owned(),
            model: model.to_owned(),
        })
        .await
        .context("新建会话失败")?
    {
        Reply::SessionCreated { summary, .. } => Ok(summary.id),
        other => bail!("新建会话时收到意外回应：{other:?}"),
    }
}

/// 拉当前工作目录下的会话列表，按最后更新时间倒序（运行时保证该顺序）。
async fn list_sessions(session: &ClientSession, cwd: &str) -> Result<Vec<SessionSummary>> {
    match session
        .request(Request::SessionList {
            cwd: Some(cwd.to_owned()),
        })
        .await
        .context("列出会话失败")?
    {
        Reply::Sessions { sessions } => Ok(sessions),
        other => bail!("列出会话时收到意外回应：{other:?}"),
    }
}

async fn cmd_serve(
    cwd: &std::path::Path,
    config: &Arc<Config>,
    _global: &GlobalArgs,
) -> Result<i32> {
    let workspace = Arc::new(Workspace::new(crate::workspace::detect_root(cwd)));
    // daemon 自己就是运行时，必须实打实把 provider 与工具注册表建起来。
    // secret 由 `serve` 内部用 `Registration::create` 重建并覆盖，这里给的只是占位。
    let secret = zcode_utils::daemon::Secret::generate().context("生成握手密钥失败")?;
    let deps = build_deps(Arc::clone(config), workspace, secret)
        .await
        .context("装配运行时失败")?;

    crate::host::daemon::serve(config.as_ref(), deps)
        .await
        .context("daemon 退出")?;
    Ok(0)
}

async fn cmd_auth(action: AuthAction) -> Result<i32> {
    let store = AuthStore::discover().context("打开凭据存储失败")?;
    match action {
        AuthAction::Login { provider } => {
            let id = parse_provider(&provider)?;
            // 登录流程要把授权 URL 递给用户；这条命令不进 TUI，写 stderr 是安全的，
            // 而且能让 stdout 保持干净（脚本可能只关心成功与否）。
            let prompt = BrowserPrompt::stderr();
            store
                .login(id, &prompt)
                .await
                .with_context(|| format!("{id} 登录失败"))?;
            let mut out = std::io::stdout();
            writeln!(out, "{id} 登录成功").context("写 stdout 失败")?;
        }
        AuthAction::Logout { provider } => {
            let id = parse_provider(&provider)?;
            store
                .logout(id)
                .await
                .with_context(|| format!("{id} 登出失败"))?;
            let mut out = std::io::stdout();
            writeln!(out, "{id} 凭据已删除").context("写 stdout 失败")?;
        }
        AuthAction::List => {
            let mut out = std::io::stdout();
            for id in PROVIDERS {
                let status = match store.access(*id).await {
                    Ok(_) => "已就绪",
                    Err(_) => "未登录",
                };
                writeln!(out, "{id:<14} {status}").context("写 stdout 失败")?;
            }
        }
    }
    Ok(0)
}

/// `auth list` 的遍历顺序。列表要稳定，不能依赖 `HashMap` 遍历序。
const PROVIDERS: &[ProviderId] = &[
    ProviderId::Anthropic,
    ProviderId::OpenAi,
    ProviderId::OpenAiCodex,
    ProviderId::Xai,
    ProviderId::XaiOAuth,
];

fn parse_provider(raw: &str) -> Result<ProviderId> {
    ProviderId::parse(raw).with_context(|| {
        let names: Vec<&str> = PROVIDERS.iter().map(|p| p.as_str()).collect();
        format!("未知提供商 `{raw}`，可选：{}", names.join(" / "))
    })
}

fn cmd_models(config: &Config, global: &GlobalArgs) -> Result<i32> {
    let resolved =
        crate::model::resolve_model(config, global.model.as_deref()).context("解析模型失败")?;
    let mut out = std::io::stdout();
    writeln!(
        out,
        "{}\t{}\t上下文 {} tokens",
        resolved.id, resolved.provider, resolved.context_window
    )
    .context("写 stdout 失败")?;
    Ok(0)
}

fn cmd_config(cwd: &std::path::Path, config: &Config, action: &ConfigAction) -> Result<i32> {
    let mut out = std::io::stdout();
    match action {
        ConfigAction::Path => {
            let paths = config::discover(cwd);
            let state = config::state_dir().context("定位状态目录失败")?;
            writeln!(out, "状态目录\t{}", state.display()).context("写 stdout 失败")?;
            writeln!(out, "全局配置\t{}", paths.global.display()).context("写 stdout 失败")?;
            match &paths.project_root {
                Some(root) => writeln!(out, "项目根\t{}", root.display()),
                None => writeln!(out, "项目根\t（未找到 .git 或 .zcode/）"),
            }
            .context("写 stdout 失败")?;
            match &paths.project {
                Some(path) => writeln!(out, "项目配置\t{}", path.display()),
                None => writeln!(out, "项目配置\t（无）"),
            }
            .context("写 stdout 失败")?;
        }
        ConfigAction::Show => {
            writeln!(out, "{config:#?}").context("写 stdout 失败")?;
        }
    }
    Ok(0)
}

async fn cmd_session(
    cwd: &std::path::Path,
    config: &Arc<Config>,
    _global: &GlobalArgs,
    action: SessionAction,
) -> Result<i32> {
    let workspace = Arc::new(Workspace::new(crate::workspace::detect_root(cwd)));
    let root = workspace.root().to_string_lossy().into_owned();

    let connection = {
        // 同 `cmd_agent`：闭包 move 一份，`connect` 的前两个参数借外层那份。
        let deps_config = Arc::clone(config);
        let deps_workspace = Arc::clone(&workspace);
        connect::connect(config, workspace.root(), move |secret| {
            build_deps(deps_config, deps_workspace, secret).boxed()
        })
        .await
        .context("连接 agent 运行时失败")?
    };
    let session = ClientSession::open(connection)
        .await
        .context("与运行时握手失败")?;

    match action {
        SessionAction::List => {
            let sessions = list_sessions(&session, &root).await?;
            let mut out = std::io::stdout();
            for summary in sessions {
                writeln!(
                    out,
                    "{}\t{}\t{}",
                    summary.id,
                    summary.model,
                    summary.title.as_deref().unwrap_or("(无标题)")
                )
                .context("写 stdout 失败")?;
            }
        }
    }

    session.shutdown().await.context("关闭连接失败")?;
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::{
        Cli, Command, Interface, exit_code, new_client_id, parse_approval_mode, parse_provider,
    };
    use clap::Parser as _;
    use std::process::ExitCode;

    #[test]
    fn subcommand_wins_over_trailing_prompt() {
        let cli = Cli::try_parse_from(["zcode", "run", "hello"]).expect("应当解析成功");
        assert!(cli.command.is_some(), "`run` 必须被当作子命令而不是提示词");
        assert!(cli.prompt.is_empty());
    }

    #[test]
    fn bare_prompt_is_collected() {
        let cli = Cli::try_parse_from(["zcode", "修一下", "这个 bug"]).expect("应当解析成功");
        assert!(cli.command.is_none());
        assert_eq!(cli.prompt, vec!["修一下".to_owned(), "这个 bug".to_owned()]);
    }

    /// **全局 flag 出现在子命令之前，子命令仍然必须被当作子命令。**
    ///
    /// 真机回归：曾经给 `Cli` 加了 `args_conflicts_with_subcommands = true`，
    /// 语义是"本命令任何参数一出现就拒绝子命令"，全局 flag 也算数。于是
    /// `zcode --cwd X serve` 里的 `serve` 落进了 `prompt`，`spawn_daemon` 拉起的子进程
    /// 不去当 daemon 而是又跑一遍客户端流程 → 发现没 daemon → 再 spawn → **fork 炸弹**。
    ///
    /// 旧版本的这条测试只断言 `--model` 被收到、没断言 `command` 是子命令，
    /// 所以炸弹存在时它照样是绿的——断言必须落在真正会坏的那件事上。
    #[test]
    fn global_flag_before_subcommand_still_routes_to_subcommand() {
        let cli = Cli::try_parse_from(["zcode", "-m", "sonnet", "run", "hi"])
            .expect("全局 flag 必须能出现在子命令之前");
        assert_eq!(cli.global.model.as_deref(), Some("sonnet"));
        assert!(
            matches!(cli.command, Some(Command::Run { .. })),
            "`run` 必须是子命令，实得 {:?} / prompt={:?}",
            cli.command,
            cli.prompt
        );
        assert!(cli.prompt.is_empty(), "子命令的词不得落进提示词");
    }

    /// `spawn_daemon` 实际发出的那条 argv 必须解析成 `serve`。
    ///
    /// 这是 fork 炸弹的精确形状，单独钉一条：参数顺序变了也要立刻红。
    #[test]
    fn spawned_daemon_argv_parses_as_serve() {
        let cli = Cli::try_parse_from(["zcode", "serve", "--cwd", "C:/tmp/projA"])
            .expect("daemon 子进程的 argv 必须可解析");
        assert!(
            matches!(cli.command, Some(Command::Serve)),
            "实得 {:?} / prompt={:?}",
            cli.command,
            cli.prompt
        );
        assert_eq!(
            cli.global.cwd.as_deref(),
            Some(std::path::Path::new("C:/tmp/projA"))
        );
    }

    #[test]
    fn approval_mode_accepts_kebab_case() {
        assert!(parse_approval_mode("always-ask").is_ok());
        assert!(parse_approval_mode("yolo").is_ok());
        let error = parse_approval_mode("nope").expect_err("无效模式必须报错");
        assert!(
            error.to_string().contains("always-ask"),
            "错误要列出可选值：{error}"
        );
    }

    #[test]
    fn unknown_provider_lists_the_valid_ones() {
        let error = parse_provider("gemini").expect_err("未知提供商必须报错");
        let text = format!("{error}");
        assert!(text.contains("anthropic"), "错误要列出可选值：{text}");
    }

    /// 退出码超出 `u8` 时必须钳到失败，绝不能回绕成 0（`as` 就会）。
    #[test]
    fn out_of_range_exit_code_clamps_to_failure() {
        assert_eq!(
            format!("{:?}", exit_code(256)),
            format!("{:?}", ExitCode::from(1))
        );
        assert_eq!(
            format!("{:?}", exit_code(130)),
            format!("{:?}", ExitCode::from(130))
        );
    }

    /// 客户端实例 id 是接管仲裁的身份标签，两次生成必须不同。
    #[test]
    fn client_ids_are_distinct() {
        assert_ne!(new_client_id(), new_client_id());
    }

    #[test]
    fn print_flag_selects_json_only_when_asked() {
        assert!(matches!(
            Interface::headless(true),
            Interface::Headless(crate::render::OutputFormat::Json)
        ));
        assert!(matches!(
            Interface::headless(false),
            Interface::Headless(crate::render::OutputFormat::Text)
        ));
    }
}
