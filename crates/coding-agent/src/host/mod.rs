//! Host：wire 协议服务端。
//!
//! 装配 `zcode-agent` 运行时、`zcode-ai` provider、`zcode-protocol` wire 类型，
//! 对外只暴露一个入口——[`Host::handle_client`]——真 daemon 的 socket accept 循环与
//! 自托管的 `stream_pair()` 都调它，**同一条路径**。
//!
//! | 子模块 | 职责 |
//! | --- | --- |
//! | [`adapter`] | 领域类型 ↔ wire 类型互转，穷尽 match，不绕过 `zcode-protocol` |
//! | [`sessions`] | `SessionId -> SessionSlot` 表；每个会话一个 actor 任务独占
//!   `AgentRuntime` |
//! | [`client`] | 三帧握手 + 读写分离的连接处理循环 |
//! | [`daemon`] | daemon 生命周期编排（`HostDaemon` 所有） |
//! | [`connect`] | 客户端侧连接与帧收发（`HostDaemon` 所有） |

/// 领域类型 ↔ wire 类型互转。
pub(crate) mod adapter;
/// 三帧握手 + 读写分离的连接处理循环。
pub(crate) mod client;
/// 客户端侧连接与帧收发（`HostDaemon` 所有）。
pub(crate) mod connect;
/// daemon 生命周期编排（`HostDaemon` 所有）。
pub(crate) mod daemon;
/// 会话表：`SessionId -> SessionSlot`。
pub(crate) mod sessions;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use sha2::{Digest as _, Sha256};

use zcode_agent::CancelRegistry;
use zcode_agent::tool::registry::ToolRegistry;
use zcode_ai::Provider;
use zcode_utils::daemon::Secret;

use crate::config::Config;
use crate::model::ResolvedModel;
use crate::prompt::PromptSet;
use crate::workspace::Workspace;

/// 把 daemon 的运行时目录按**工作区根**分桶：一个工作区一个 daemon。
///
/// # 为什么必须分桶
///
/// `HostDeps` 里的 `workspace` / `registry` / `prompts` 全部由工作区根派生，且
/// `Request::SessionCreate` **忽略客户端自报的 `cwd`**、一律用 `deps.workspace.root()`
/// （那条是防止客户端把会话建到任意目录的安全约束）。两件事叠加的后果是：
/// 只要 daemon 的端点是全机唯一的，**先启动的那个工作区就会成为所有客户端的工作区**——
/// 在 B 项目里跑 `zcode`，工具读写的却是 A 项目的文件。这是真机跑出来的缺陷，不是理论风险。
///
/// 分桶把"哪个工作区"从运行期状态变成端点身份的一部分，A 与 B 各自连各自的 daemon，
/// 上面那条安全约束也就重新成立了。
///
/// # 名字构成
///
/// `<base>/<末级目录名>-<规范化全路径的 SHA-256 前 16 hex>`。目录名只为人眼可读；
/// 唯一性全靠哈希。
///
/// # Windows 上必须先规范化路径形态
///
/// 父进程从 `current_dir()` 拿到的是 `C:\tmp\projA`，子进程从 `--cwd` 经 clap 拿到的
/// 可能是 `C:/tmp/projA`，某些 API 还会带 `\\?\` verbatim 前缀。**同一个目录的三种写法
/// 哈希不同就会分出三个桶**：客户端在 A 桶等就绪，daemon 在 B 桶注册，
/// `ReadyChannel` 永远等不到，`zcode` 表现为无限挂起。这是真机跑出来的缺陷
/// （`~/.zcode/run/` 下同时出现两个 `proja-<不同哈希>` 空目录）。
///
/// 规范化三步（**仅 Windows**）：剥 `\\?\` / `//?/` 前缀 → `\` 统一成 `/` → 转小写。
/// Unix 一步都不做：那里 `\` 是合法文件名字符、路径大小写敏感，
/// `/a/Proj` 与 `/a/proj` 本就是两个目录，任何折叠都会造成本函数要防的跨工作区串台。
/// 同一套平台分歧见 `crates/text/src/path.rs:43-51` 的 `paths_equal`。
pub(crate) fn scoped_runtime_dir(base: &Path, workspace_root: &Path) -> PathBuf {
    let normalized = normalize_for_bucket(&workspace_root.to_string_lossy());
    let label = normalized
        .rsplit('/')
        .find(|segment| !segment.is_empty())
        .unwrap_or("root");
    let digest = Sha256::digest(normalized.as_bytes());
    let mut hash = String::with_capacity(16);
    for byte in digest.iter().take(8) {
        use std::fmt::Write as _;
        let _ = write!(hash, "{byte:02x}");
    }
    base.join(format!("{label}-{hash}"))
}

/// 把一条工作区根路径归一成"同一个目录必得同一个字符串"的形态。见
/// [`scoped_runtime_dir`] 的文档。
fn normalize_for_bucket(raw: &str) -> String {
    if !cfg!(windows) {
        return raw.to_owned();
    }
    let stripped = raw
        .strip_prefix(r"\\?\")
        .or_else(|| raw.strip_prefix("//?/"))
        .unwrap_or(raw);
    stripped
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_lowercase()
}

/// Host 运行所需的全部依赖，由 CLI 层在启动时装配一次。
pub(crate) struct HostDeps {
    /// 推理提供商。整个进程共用一个：`SessionCreate.model` 只是记在会话树里的字符串，
    /// 不会切换 provider——本仓面向单人 power-user，进程级模型锁定是显式选择
    /// （见 `local://contract.md` 的模型解析一节）。
    pub(crate) provider: Arc<dyn Provider>,
    /// 内置工具注册表。所有会话共享同一份；工具本身不持有会话状态。
    pub(crate) registry: Arc<ToolRegistry>,
    /// 已解析配置。
    pub(crate) config: Arc<Config>,
    /// 已装配的提示集。
    pub(crate) prompts: Arc<PromptSet>,
    /// 已解析模型。
    pub(crate) model: ResolvedModel,
    /// 工作区。
    pub(crate) workspace: Arc<Workspace>,
    /// 会话 JSONL 文件所在目录。
    pub(crate) sessions_dir: PathBuf,
    /// 握手密钥。**服务端要先证明持有它**（`crates/protocol/src/version.rs:139-149`：
    /// `ServerHello.proof` 是对 `ClientHello.nonce` 的应答）。
    ///
    /// 真 daemon 用 `Registration::create()` 生成并写进注册文件；自托管由 `connect`
    /// 现场 `Secret::generate()`，同一份同时交给 `HostDeps` 与 `Connection`。
    pub(crate) secret: Secret,
}

#[cfg(test)]
mod tests {
    use super::scoped_runtime_dir;
    use std::path::Path;

    /// 两个工作区必须落到两个 daemon 桶。
    ///
    /// 回归防线：不分桶时全机只有一个 daemon，先启动者的工作区会成为所有客户端的
    /// 工作区——真机复现过「在 B 项目里跑 `zcode`，`ls` 列出的是 A 项目的文件」。
    #[test]
    fn different_workspaces_get_different_buckets() {
        let base = Path::new("/state/run");
        let a = scoped_runtime_dir(base, Path::new("/tmp/projA"));
        let b = scoped_runtime_dir(base, Path::new("/tmp/projB"));
        assert_ne!(a, b);
        assert!(a.starts_with(base) && b.starts_with(base));
    }

    /// 同一个工作区每次都要算出同一个桶——客户端与它 spawn 出来的 daemon 各算一次，
    /// 算不到一起就会无限重复 spawn。
    #[test]
    fn same_workspace_is_stable() {
        let base = Path::new("/state/run");
        assert_eq!(
            scoped_runtime_dir(base, Path::new("/tmp/projA")),
            scoped_runtime_dir(base, Path::new("/tmp/projA"))
        );
    }

    /// Windows 路径大小写不敏感：不折叠会让同一个目录落到两个桶，同一个项目起两个
    /// 互相看不见的 daemon。
    #[cfg(windows)]
    #[test]
    fn case_differences_map_to_the_same_bucket_on_windows() {
        let base = Path::new("C:/state/run");
        assert_eq!(
            scoped_runtime_dir(base, Path::new("C:/Work/Proj")),
            scoped_runtime_dir(base, Path::new("c:/work/proj"))
        );
    }

    /// 同一个目录的三种写法必须落到同一个桶。
    ///
    /// 真机回归：父进程 `current_dir()` 给 `C:\tmp\projA`，子进程 `--cwd` 经 clap 可能
    /// 变成 `C:/tmp/projA`，某些 API 还带 `\\?\` 前缀。三者哈希不同 → 客户端在 A 桶等
    /// 就绪、daemon 在 B 桶注册 → `ReadyChannel` 永远等不到 → `zcode` 无限挂起。
    /// 现象是 `~/.zcode/run/` 下并排躺着两个 `proja-<不同哈希>` 空目录。
    #[cfg(windows)]
    #[test]
    fn separator_and_verbatim_forms_map_to_the_same_bucket() {
        let base = Path::new("C:/state/run");
        let backslash = scoped_runtime_dir(base, Path::new(r"C:\tmp\projA"));
        let forward = scoped_runtime_dir(base, Path::new("C:/tmp/projA"));
        let verbatim = scoped_runtime_dir(base, Path::new(r"\\?\C:\tmp\projA"));
        let trailing = scoped_runtime_dir(base, Path::new(r"C:\tmp\projA\"));
        assert_eq!(backslash, forward, "分隔符不同不得分桶");
        assert_eq!(backslash, verbatim, "verbatim 前缀不得分桶");
        assert_eq!(backslash, trailing, "末尾分隔符不得分桶");
    }

    /// Unix 路径大小写敏感：`/a/Proj` 与 `/a/proj` 是两个不同目录，**必须**分到两个桶，
    /// 否则就是本函数要防的那种跨工作区串台。
    #[cfg(unix)]
    #[test]
    fn case_differences_stay_separate_on_unix() {
        let base = Path::new("/state/run");
        assert_ne!(
            scoped_runtime_dir(base, Path::new("/work/Proj")),
            scoped_runtime_dir(base, Path::new("/work/proj"))
        );
    }
}

/// Host 的错误。
#[derive(Debug, thiserror::Error)]
pub(crate) enum HostError {
    /// 连接 I/O 失败。
    #[error("连接 I/O 失败")]
    Io(#[from] std::io::Error),
    /// 帧编解码失败。
    #[error("帧编解码失败")]
    Frame(#[from] zcode_protocol::FrameError),
    /// 会话存储失败。
    #[error("会话存储失败")]
    Store(#[from] zcode_agent::StoreError),
    /// Agent 运行时失败（提供商请求失败、上下文超限等）。
    #[error(transparent)]
    Agent(#[from] zcode_agent::AgentError),
    /// 握手原语失败（生成 nonce 用的随机源不可用等）。
    #[error("握手失败")]
    Handshake(#[from] zcode_utils::daemon::DaemonError),
    /// 引用了一个不存在的会话。
    #[error("会话 {0} 不存在")]
    UnknownSession(String),
    /// 会话的后台 actor 任务已经退出（通常意味着它此前 panic 了）。
    #[error("会话后台任务已停止响应")]
    ActorGone,
}

/// 一条连接的服务端。持有全部会话与跨会话共享的取消表。
///
/// 一个进程只建一个 `Host`：真 daemon 用它服务多条连接，自托管用它服务
/// `stream_pair()` 的那一端。
///
/// **不派生 `Debug`**：`HostDeps` 里的 `Arc<dyn Provider>` / `Arc<Config>` /
/// `Arc<PromptSet>` 等跨 crate 类型是否都实现了 `Debug` 不由本模块决定，装配层
/// 尚未全部落盘，此刻无法验证；`missing_debug_implementations` 只是 `warn`，
/// 不值得为了消一个警告去耦合别的模块的实现细节。
pub(crate) struct Host {
    sessions: sessions::SessionTable,
    /// 会话 → 在飞 turn / 后台作业的取消表。**跨会话共享**：取消请求只带 session id，
    /// 必须能从一张表里找到目标会话此刻所有在飞信号，理由见 `zcode_agent::cancel` 的
    /// 模块文档。
    cancels: Arc<CancelRegistry>,
    deps: HostDeps,
}

impl Host {
    /// 装配一个新 Host。
    #[must_use]
    pub(crate) fn new(deps: HostDeps) -> Arc<Self> {
        Arc::new(Self {
            sessions: sessions::SessionTable::new(),
            cancels: Arc::new(CancelRegistry::new()),
            deps,
        })
    }

    /// 服务一条客户端连接直到对端关闭。
    ///
    /// 真 socket 与 `stream_pair()` 自托管走**同一个**函数——这正是自托管模式能验证
    /// 握手/取消/慢消费者三条不变量的原因：测试直接喂 `stream_pair()` 就等价于测了
    /// 真实的跨进程路径。
    pub(crate) async fn handle_client(
        self: Arc<Self>,
        stream: zcode_utils::transport::Stream,
    ) -> Result<(), HostError> {
        client::handle_client(self, stream).await
    }
}
