//! 客户端侧连接建立与帧收发：daemon 在就连它，不在就自托管；`ClientSession` 是
//! `render`/`app` 唯一认识的帧收发类型。

use std::collections::HashMap;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;

#[cfg(unix)]
use std::os::unix::process::CommandExt as _;
#[cfg(windows)]
use std::os::windows::process::CommandExt as _;

use futures_util::future::BoxFuture;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::sync::{Mutex as AsyncMutex, mpsc, oneshot};
use tokio::task::JoinHandle;
use zcode_protocol::version::{ClientAuth, ClientHello, Nonce as ProtoNonce, Proof};
use zcode_protocol::wire::{ClientFrame, Event, Reply, Request, ServerFrame};
use zcode_protocol::{Envelope, FrameDecoder, FrameError, Hello, IdGen, ProtocolError, encode};
use zcode_utils::daemon::{
    DaemonError as PrimitiveError, Domain, Nonce as DaemonNonce, READY_ENDPOINT_ENV, READY_TIMEOUT,
    ReadyChannel, Registration, Secret, proof, verify_proof,
};
use zcode_utils::env::{self, WorkerHostError};
use zcode_utils::transport::{ReadHalf, Stream, WriteHalf, stream_pair};

use crate::config::Config;

use super::daemon::REGISTRATION_FILE_NAME;
use super::{Host, HostDeps};

/// 强制自托管的环境变量名。真正的分支逻辑在 [`connect_inner`]：它把这个开关当一个普通
/// `bool` 参数收，环境变量只在 [`connect`] 里读一次；测试直接调用 `connect_inner` 注入
/// 布尔值，不需要 `std::env::set_var`——那是进程级全局状态，并行测试会互相污染。
const NO_DAEMON_ENV: &str = "ZCODE_NO_DAEMON";

/// 握手时上报的客户端实现标识，仅用于日志，不参与任何行为分支。
const CLIENT_AGENT_ID: &str = concat!("zcode-client/", env!("CARGO_PKG_VERSION"));

/// 连接方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConnectMode {
    /// 连的是真 daemon 进程。
    Daemon,
    /// 同进程自托管：没有真 daemon，也没被要求要有。
    SelfHosted,
}

/// 一条**尚未握手**的连接。
pub(crate) struct Connection {
    /// 底层字节流。
    pub(crate) stream: Stream,
    /// 连接方式。
    pub(crate) mode: ConnectMode,
    /// 握手密钥。daemon 模式来自注册文件；自托管模式现场生成，与 [`HostDeps::secret`]
    /// 是同一份。
    pub(crate) secret: Secret,
    /// 自托管模式下持有 `handle_client` 任务，防止它被提前丢弃；daemon 模式为 `None`。
    _host_task: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connection")
            .field("mode", &self.mode)
            .finish_non_exhaustive()
    }
}

/// [`connect`] / [`ClientSession`] 的失败。
#[derive(Debug, thiserror::Error)]
pub(crate) enum ConnectError {
    /// provider / 工具注册表 / prompt 装配失败。自托管路径才会走到。
    #[error(transparent)]
    Deps(#[from] anyhow::Error),
    /// daemon 原语（锁、注册、就绪握手、握手证明）失败。
    #[error(transparent)]
    Primitive(#[from] PrimitiveError),
    /// 传输层 I/O 失败。
    #[error("传输失败：{0}")]
    Io(#[source] std::io::Error),
    /// 当前进程未声明为 worker host，无法重入自身二进制拉起 daemon。
    #[error(transparent)]
    WorkerHost(#[from] WorkerHostError),
    /// 拉起 daemon 子进程失败。
    #[error("拉起 daemon 子进程失败：{0}")]
    Spawn(#[source] std::io::Error),
    /// daemon 子进程已经宣布就绪，但随后仍连不上端点——注册文件与实际监听不一致。
    #[error("daemon 子进程已就绪，但仍无法连接端点")]
    SpawnedButUnreachable,
    /// 帧编码失败。我们的 payload 都是良构类型，正常不会发生，保留分支是因为
    /// `zcode_protocol::encode` 返回 `Result`。
    #[error("帧编码失败：{0}")]
    Encode(#[source] FrameError),
    /// 三帧握手失败：版本不兼容、证明校验不过、收到了意外的帧类型。
    #[error("握手失败：{0}")]
    Handshake(String),
    /// 对端返回结构化协议错误（例如 `UnsupportedRequest`）。
    #[error("协议错误：{0}")]
    Protocol(#[from] ProtocolError),
    /// 连接已关闭：`request` 发起或等待期间对端断开。
    #[error("连接已关闭")]
    Closed,
}

/// 拿到一条**尚未握手**的连接：daemon 在就连它，不在就 `stream_pair()` 自托管。
///
/// `secret` 是握手材料，**必须显式携带**：协议层只搬运不透明字符串，密钥归
/// `zcode_utils::daemon`。来源两条，互斥：
/// - [`ConnectMode::Daemon`]：`Registration::read(<runtime_dir>/daemon.json)?.secret`。
/// - [`ConnectMode::SelfHosted`]：`Secret::generate()` 现场生成，同一份同时塞进
///   [`HostDeps::secret`] 与本结构。
///
/// `deps` 只在真正自托管时才会被调用——`HostDeps` 的构造（provider、工具注册表、prompt
/// 装配）是惰性的，daemon 模式下客户端进程完全不碰它们。
///
/// `workspace_root` 决定连**哪一个** daemon：端点按工作区根分桶
/// （[`super::scoped_runtime_dir`]），一个工作区一个 daemon。全机一个 daemon 会让
/// 后来的客户端拿到先启动者的工作区——真机验证过：在 B 项目里跑，工具读的是 A 项目的文件。
pub(crate) async fn connect(
    config: &Config,
    workspace_root: &Path,
    deps: impl FnOnce(Secret) -> BoxFuture<'static, Result<HostDeps, ConnectError>>,
) -> Result<Connection, ConnectError> {
    let force_self_host = std::env::var_os(NO_DAEMON_ENV).is_some();
    connect_inner(config, workspace_root, force_self_host, deps).await
}

/// [`connect`] 的可测试内核：把环境变量开关变成显式参数。
async fn connect_inner(
    config: &Config,
    workspace_root: &Path,
    force_self_host: bool,
    deps: impl FnOnce(Secret) -> BoxFuture<'static, Result<HostDeps, ConnectError>>,
) -> Result<Connection, ConnectError> {
    if force_self_host || !config.daemon.enabled {
        return self_host(deps).await;
    }

    let runtime_dir = super::scoped_runtime_dir(&config.daemon.runtime_dir, workspace_root);

    if let Some(connection) = try_connect_registered(&runtime_dir).await? {
        return Ok(connection);
    }

    spawn_daemon(&runtime_dir, workspace_root).await?;

    try_connect_registered(&runtime_dir)
        .await?
        .ok_or(ConnectError::SpawnedButUnreachable)
}

/// 同进程自托管：不开真 socket，`deps` 只在这条路径上被调用。
async fn self_host(
    deps: impl FnOnce(Secret) -> BoxFuture<'static, Result<HostDeps, ConnectError>>,
) -> Result<Connection, ConnectError> {
    let secret = Secret::generate()?;
    let host_deps = deps(secret.clone()).await?;
    let host = Host::new(host_deps);
    let (client_stream, server_stream) = stream_pair().await.map_err(ConnectError::Io)?;
    let host_task = tokio::spawn(async move {
        if let Err(err) = host.handle_client(server_stream).await {
            tracing::warn!(error = %err, "自托管连接处理失败");
        }
    });
    Ok(Connection {
        stream: client_stream,
        mode: ConnectMode::SelfHosted,
        secret,
        _host_task: Some(host_task),
    })
}

/// 读注册文件、尝试连接。注册文件不存在或连不上都返回 `Ok(None)`——两种情况对调用方
/// 都意味着"当前没有可用的 daemon"，不是错误。
async fn try_connect_registered(runtime_dir: &Path) -> Result<Option<Connection>, ConnectError> {
    let registration_path = runtime_dir.join(REGISTRATION_FILE_NAME);
    let Some(registration) = Registration::read(&registration_path)? else {
        return Ok(None);
    };
    match Stream::connect(&registration.endpoint).await {
        Ok(stream) => Ok(Some(Connection {
            stream,
            mode: ConnectMode::Daemon,
            secret: registration.secret,
            _host_task: None,
        })),
        Err(_) => Ok(None),
    }
}

/// 重入自身二进制拉起真 daemon，等它宣布就绪。
///
/// **`--cwd` 是必须显式传的**，不能依赖子进程继承 cwd：工作区根决定了端点分桶
/// （[`super::scoped_runtime_dir`]），父子两侧算出不同的桶就会各起各的 daemon，
/// 父进程随后在自己那个桶里永远读不到注册文件。显式传参让"服务哪个工作区"成为
/// 命令行的一部分，`ps` 里也看得见。
async fn spawn_daemon(runtime_dir: &Path, workspace_root: &Path) -> Result<(), ConnectError> {
    let entry = env::worker_host_entry()?;
    let mut ready = ReadyChannel::bind(runtime_dir)?;

    let mut command = Command::new(entry);
    command
        // 子命令**放在最前面**，全局 flag 跟在后面。顺序反过来（`--cwd X serve`）曾经
        // 让 clap 把 `serve` 当成提示词，daemon 子进程于是又跑一遍客户端流程、
        // 再 spawn 一个自己——真机上是 fork 炸弹。测试
        // `cli::tests::spawned_daemon_argv_parses_as_serve` 钉住这条 argv 的形状。
        .arg("serve")
        .arg("--cwd")
        .arg(workspace_root)
        .env(READY_ENDPOINT_ENV, ready.env_value())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    apply_background_spawn_options(&mut command);

    let mut child = command.spawn().map_err(ConnectError::Spawn)?;

    ready
        .wait(&mut child, READY_TIMEOUT)
        .await
        .map_err(ConnectError::Primitive)
}

/// 让 daemon 子进程脱离本进程的控制台/会话，使它在客户端退出后继续存活、且不弹窗口、
/// 不被父终端的 Ctrl+C 连坐。
///
/// 抄的是 oh-my-pi 对等场景（daemon/broker 子进程）的取舍
/// （`packages/coding-agent/src/launch/spawn-options.ts:8-17`）：
/// - **Windows 不用"新建控制台"式的 detach**（那是 Node `detached:true` 在这个平台的语义），
///   而是 `windowsHide`；它在 Win32 层落到 `CREATE_NO_WINDOW`（`CreateProcess`
///   `dwCreationFlags`，MSDN 记 `0x08000000`）：子进程不创建/继承控制台，因此也不在父进程
///   的控制台进程组里，不会被 `CTRL_C_EVENT` 连坐，也不会弹出可见窗口。
/// - **非 Windows** 该场景恒为 `detached:true`（`spawn-options.ts:12`），POSIX 语义是
///   `setsid()` 脱离控制终端所在会话，否则父终端关闭发送 `SIGHUP` 时子进程会被一并杀掉。
///   `std::process::Command` 没有内建接口，走 `pre_exec`。
fn apply_background_spawn_options(command: &mut Command) {
    #[cfg(windows)]
    {
        /// Win32 `CREATE_NO_WINDOW`（`CreateProcess` `dwCreationFlags`，MSDN 记 `0x08000000`）。
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(unix)]
    {
        // SAFETY: 这个闭包在 fork 之后、exec 之前的子进程里运行，此时子进程只有一个线程
        // （fork 直接复制单线程状态），调用 `setsid()` 不分配内存、不碰任何锁，是
        // async-signal-safe 的，满足 `pre_exec` 文档要求的"只调用 async-signal-safe 函数"。
        // 失败（例如已经是 session leader）时吞掉错误而不是让整个 spawn 失败：脱离终端
        // 会话是锦上添花，不是拉起 daemon 的必要条件。
        #[allow(
            unsafe_code,
            reason = "脱离控制终端会话需要 pre_exec + setsid，无安全 std API"
        )]
        unsafe {
            command.pre_exec(|| {
                let _ = libc::setsid();
                Ok(())
            });
        }
    }
}

/// `request()` 待回应的 oneshot 表：按信封 `id` 精确路由 [`Reply`]。
type PendingMap = Arc<AsyncMutex<HashMap<u64, oneshot::Sender<Result<Reply, ConnectError>>>>>;

/// 客户端侧的帧收发：完成三帧握手，之后 reply 与 event 分流。
///
/// `render` 与 `app` 都只认这一个类型，不许自己解帧。内部起一个 reader 任务：
/// 按 `reply_to` 把 [`Reply`] 派到对应的 oneshot，其余帧当 [`Event`] 推进 channel；
/// `Event::Unknown`（对端更新推来的未知事件）静默丢弃，未知帧结构（解析失败）记
/// `tracing::warn!` 后继续读——协议契约：推送可丢，请求不可。
pub(crate) struct ClientSession {
    writer: AsyncMutex<WriteHalf>,
    id_gen: IdGen,
    pending: PendingMap,
    events_rx: std::sync::Mutex<Option<mpsc::UnboundedReceiver<Event>>>,
    reader_task: Option<JoinHandle<()>>,
    host_task: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for ClientSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientSession").finish_non_exhaustive()
    }
}

impl ClientSession {
    /// 完成 `ClientHello` → `ServerHello` → `ClientAuth` 三帧握手。
    ///
    /// 自托管模式下同样走完整握手——只有一条路径，不给它开后门。
    pub(crate) async fn open(conn: Connection) -> Result<Self, ConnectError> {
        let Connection {
            stream,
            secret,
            _host_task: host_task,
            ..
        } = conn;
        let (mut read_half, mut write_half) = stream.into_split();
        let mut decoder = FrameDecoder::new();
        let mut read_buf = [0_u8; 8192];
        let id_gen = IdGen::default();

        let nonce_c = DaemonNonce::generate()?;
        let client_nonce = ProtoNonce(nonce_c.as_str().to_owned());
        write_client_frame(
            &mut write_half,
            &Envelope::new(
                id_gen.next_id(),
                ClientFrame::Hello(ClientHello {
                    hello: Hello::local(CLIENT_AGENT_ID),
                    nonce: client_nonce,
                }),
            ),
        )
        .await?;

        let server_frame = read_server_frame(&mut read_half, &mut decoder, &mut read_buf)
            .await
            .ok_or(ConnectError::Closed)?;
        let ServerFrame::Hello(server_hello) = server_frame.payload else {
            return Err(ConnectError::Handshake(
                "期望握手第 2 帧 Hello，收到了别的帧".to_owned(),
            ));
        };

        if let Err(mismatch) =
            zcode_protocol::PROTOCOL_VERSION.negotiate(server_hello.hello.version)
        {
            let _ = write_client_frame(
                &mut write_half,
                &Envelope::new(
                    id_gen.next_id(),
                    ClientFrame::Error(ProtocolError::from(mismatch)),
                ),
            )
            .await;
            return Err(ConnectError::Handshake(mismatch.to_string()));
        }

        if !verify_proof(&secret, Domain::Server, &nonce_c, &server_hello.proof.0) {
            return Err(ConnectError::Handshake(
                "服务端未能证明持有握手密钥".to_owned(),
            ));
        }

        let server_nonce = DaemonNonce::from(server_hello.nonce.0.clone());
        let client_proof = proof(&secret, Domain::Client, &server_nonce);
        write_client_frame(
            &mut write_half,
            &Envelope::new(
                id_gen.next_id(),
                ClientFrame::Auth(ClientAuth {
                    proof: Proof(client_proof),
                }),
            ),
        )
        .await?;

        let pending: PendingMap = Arc::new(AsyncMutex::new(HashMap::new()));
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let reader_pending = Arc::clone(&pending);
        let reader_task = tokio::spawn(run_reader(read_half, decoder, reader_pending, events_tx));

        Ok(Self {
            writer: AsyncMutex::new(write_half),
            id_gen,
            pending,
            events_rx: std::sync::Mutex::new(Some(events_rx)),
            reader_task: Some(reader_task),
            host_task,
        })
    }

    /// 发一条请求并等它的回应。并发调用安全（`&self`）：每条请求各自的 oneshot 按
    /// 信封 `id` 精确路由，互不串扰。
    pub(crate) async fn request(&self, request: Request) -> Result<Reply, ConnectError> {
        let id = self.id_gen.next_id();
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);

        let envelope = Envelope::new(id, ClientFrame::Request(request));
        let mut buf = Vec::new();
        if let Err(source) = encode(&envelope, &mut buf) {
            self.pending.lock().await.remove(&id);
            return Err(ConnectError::Encode(source));
        }
        {
            let mut writer = self.writer.lock().await;
            if let Err(source) = writer.write_all(&buf).await {
                drop(writer);
                self.pending.lock().await.remove(&id);
                return Err(ConnectError::Io(source));
            }
            if let Err(source) = writer.flush().await {
                drop(writer);
                self.pending.lock().await.remove(&id);
                return Err(ConnectError::Io(source));
            }
        }

        rx.await.unwrap_or(Err(ConnectError::Closed))
    }

    /// 取事件接收端。只能取一次，第二次返回 `None`。
    pub(crate) fn take_events(&self) -> Option<mpsc::UnboundedReceiver<Event>> {
        match self.events_rx.lock() {
            Ok(mut guard) => guard.take(),
            Err(poisoned) => poisoned.into_inner().take(),
        }
    }

    /// 关闭连接并等 reader 任务与（自托管模式下的）server 任务收尾。
    ///
    /// **不能只调 `AsyncWriteExt::shutdown()`。** Windows named pipe 的
    /// `poll_shutdown` 只 flush，不真正断开底层句柄（`tokio::net::windows::named_pipe`
    /// 的实现如此——named pipe 没有 TCP/Unix socket 那种半关闭语义）；只关写端，读端
    /// 仍握着同一份底层流，对端永远看不到断开，`reader_task`/`host_task` 会永久挂起。
    /// 必须让读、写两半**都**被 drop，底层流才会真正释放句柄：写半边在这里直接
    /// `drop`，读半边由 `reader_task` 持有，`abort()` 促使它在下一个轮询点被丢弃。
    pub(crate) async fn shutdown(self) -> Result<(), ConnectError> {
        let Self {
            writer,
            reader_task,
            host_task,
            ..
        } = self;
        drop(writer);
        if let Some(handle) = reader_task {
            handle.abort();
            let _ = handle.await;
        }
        if let Some(host_task) = host_task {
            let _ = host_task.await;
        }
        Ok(())
    }
}

async fn write_client_frame(
    write_half: &mut WriteHalf,
    envelope: &Envelope<ClientFrame>,
) -> Result<(), ConnectError> {
    let mut buf = Vec::new();
    encode(envelope, &mut buf).map_err(ConnectError::Encode)?;
    write_half.write_all(&buf).await.map_err(ConnectError::Io)?;
    write_half.flush().await.map_err(ConnectError::Io)
}

/// 读下一帧 `Envelope<ServerFrame>`。`None` 表示连接已经结束（对端正常关闭、I/O
/// 失败、或单帧超过上限——三者都必须停止读取，调用方据此收尾）。
async fn read_server_frame(
    read_half: &mut ReadHalf,
    decoder: &mut FrameDecoder,
    buf: &mut [u8],
) -> Option<Envelope<ServerFrame>> {
    loop {
        match decoder.decode::<Envelope<ServerFrame>>() {
            Ok(Some(envelope)) => return Some(envelope),
            Ok(None) => {}
            Err(FrameError::Json(source)) => {
                tracing::warn!(error = %source, "收到一帧无法解析的数据，丢弃该行继续读取");
                continue;
            }
            Err(FrameError::TooLarge { len, limit }) => {
                tracing::warn!(len, limit, "单帧超过上限，连接不可再用，停止读取");
                return None;
            }
        }
        match read_half.read(buf).await {
            Ok(0) => return None,
            Ok(n) => decoder.push(buf.get(..n).unwrap_or_default()),
            Err(source) => {
                tracing::warn!(error = %source, "读取连接失败，停止读取");
                return None;
            }
        }
    }
}

/// reader 任务主体：持续读帧，按 `reply_to` 分发给等待中的 `request()` 调用，
/// 其余当事件推进 channel；连接结束后让所有仍在等待的 `request()` 收到
/// [`ConnectError::Closed`]，避免它们永远挂着。
async fn run_reader(
    mut read_half: ReadHalf,
    mut decoder: FrameDecoder,
    pending: PendingMap,
    events_tx: mpsc::UnboundedSender<Event>,
) {
    let mut buf = [0_u8; 8192];
    while let Some(frame) = read_server_frame(&mut read_half, &mut decoder, &mut buf).await {
        match frame.payload {
            ServerFrame::Reply(reply) => {
                dispatch_reply(&pending, frame.reply_to, Ok(reply)).await;
            }
            ServerFrame::Error(err) => {
                dispatch_reply(&pending, frame.reply_to, Err(ConnectError::Protocol(err))).await;
            }
            ServerFrame::Event(Event::Unknown) => {}
            ServerFrame::Event(event) => {
                let _ = events_tx.send(event);
            }
            ServerFrame::Hello(_) => {
                tracing::warn!("握手完成后又收到一帧 Hello，忽略");
            }
        }
    }
    let mut pending = pending.lock().await;
    for (_, sender) in pending.drain() {
        let _ = sender.send(Err(ConnectError::Closed));
    }
}

async fn dispatch_reply(
    pending: &PendingMap,
    reply_to: Option<u64>,
    result: Result<Reply, ConnectError>,
) {
    let Some(id) = reply_to else {
        tracing::warn!("收到一帧没有 reply_to 的回应，丢弃");
        return;
    };
    let sender = pending.lock().await.remove(&id);
    if let Some(sender) = sender {
        let _ = sender.send(result);
    } else {
        tracing::warn!(
            id,
            "收到一帧回应，但没有对应的等待者（可能已超时或从未发出）"
        );
    }
}

#[cfg(test)]
mod tests {
    use futures_util::FutureExt as _;
    use zcode_protocol::wire::types::{SessionId, SessionSummary};

    use super::*;

    /// 测试专用 provider 桩：这些测试只测连接建立与帧路由，从不真正触发 turn，
    /// 因此这个 provider 的 `stream()` 永远不会被调用。
    #[derive(Debug)]
    struct StubProvider;

    #[async_trait::async_trait]
    impl zcode_ai::Provider for StubProvider {
        fn id(&self) -> zcode_ai::ProviderId {
            zcode_ai::ProviderId::Anthropic
        }

        async fn stream(
            &self,
            _request: &zcode_ai::CompletionRequest,
        ) -> Result<zcode_ai::EventStream, zcode_ai::AiError> {
            Err(zcode_ai::AiError::Aborted)
        }
    }

    fn test_config(runtime_dir: std::path::PathBuf, enabled: bool) -> Config {
        Config {
            model: crate::config::ModelConfig {
                id: None,
                thinking: None,
                provider: None,
            },
            approval: crate::config::ApprovalConfig {
                mode: zcode_agent::ApprovalMode::Yolo,
                policies: HashMap::new(),
            },
            tools: crate::config::ToolsConfig {
                disabled: Vec::new(),
                bash_timeout_secs: 30,
                read_max_lines: 2000,
            },
            session: crate::config::SessionConfig {
                dir: runtime_dir.join("sessions"),
            },
            daemon: crate::config::DaemonConfig {
                enabled,
                runtime_dir,
            },
            ui: crate::config::UiConfig {
                show_thinking: false,
            },
        }
    }

    fn stub_deps() -> impl FnOnce(Secret) -> BoxFuture<'static, Result<HostDeps, ConnectError>> {
        |secret: Secret| {
            async move {
                Ok(HostDeps {
                    provider: Arc::new(StubProvider),
                    registry: Arc::new(zcode_agent::tool::registry::ToolRegistry::new()),
                    config: Arc::new(test_config(std::env::temp_dir(), false)),
                    prompts: Arc::new(crate::prompt::PromptSet {
                        system: Vec::new(),
                        session_context: String::new(),
                    }),
                    model: crate::model::ResolvedModel {
                        id: "stub".to_owned(),
                        provider: zcode_ai::ProviderId::Anthropic,
                        context_window: 200_000,
                        thinking: zcode_ai::Thinking::Disabled,
                    },
                    workspace: Arc::new(crate::workspace::Workspace::new(std::env::temp_dir())),
                    sessions_dir: std::env::temp_dir(),
                    secret,
                })
            }
            .boxed()
        }
    }

    /// `config.daemon.enabled == false` 时（"daemon 不在"的默认形态）自动自托管，
    /// 不去碰注册文件、不尝试 spawn。
    #[tokio::test]
    async fn daemon_disabled_falls_back_to_self_host_automatically() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = test_config(dir.path().to_path_buf(), false);

        let connection = connect_inner(&config, dir.path(), false, stub_deps())
            .await
            .expect("daemon 被禁用时应当自动自托管");
        assert_eq!(connection.mode, ConnectMode::SelfHosted);

        // 目录里确实什么都没写：证明这条路径完全没碰 daemon 机制。
        assert!(!dir.path().join(REGISTRATION_FILE_NAME).exists());
    }

    /// `ZCODE_NO_DAEMON` 强制自托管：环境变量在 `connect()` 里只读一次转成 `bool`
    /// 参数，测试直接给 `connect_inner` 注入 `true`，不碰真实进程环境变量。
    #[tokio::test]
    async fn no_daemon_flag_forces_self_host_even_when_daemon_enabled() {
        let dir = tempfile::tempdir().expect("tempdir");
        // enabled=true 且没有真 daemon：若 force 参数不生效，这条路径会尝试
        // spawn_daemon 并因为测试进程未声明 worker host 而失败——用这个反差确认
        // force 参数确实短路了 daemon 逻辑。
        let config = test_config(dir.path().to_path_buf(), true);

        let connection = connect_inner(&config, dir.path(), true, stub_deps())
            .await
            .expect("force_self_host=true 时必须自托管成功，不该尝试 spawn");
        assert_eq!(connection.mode, ConnectMode::SelfHosted);
    }

    /// 没有声明 worker host 时，daemon 缺席且未强制自托管会尝试 spawn 并确定性失败——
    /// 证明"daemon 不在→spawn"这条分支确实被走到了（而不是被默默吞掉/跳过）。
    #[tokio::test]
    async fn daemon_enabled_without_registration_attempts_spawn_and_fails_cleanly() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = test_config(dir.path().to_path_buf(), true);

        let result = connect_inner(&config, dir.path(), false, stub_deps()).await;
        assert!(
            matches!(result, Err(ConnectError::WorkerHost(_))),
            "未声明 worker host 时应当在 spawn_daemon 里确定性失败，实际是 {result:?}"
        );
    }

    // ── 下面几个测试针对 `ClientSession` 本身的协议正确性（握手、请求路由、事件
    // 静默跳过），刻意绕开 `self_host()`/`Host`，直接用 `stream_pair()` 造一对流、
    // 配一个只实现协议、不含任何领域逻辑的桩服务端。这样测的是本文件自己的帧收发
    // 逻辑，不依赖 `host/mod.rs` 的领域实现细节（例如 `SessionList` 具体怎么答）。

    /// 桩服务端：完成三帧握手后，对 `Ping` 回 `Pong`，对 `SessionList { cwd }` 把
    /// `cwd` 回显进一条伪造摘要的 `title`（用来验证并发请求按 `reply_to` 精确路由），
    /// 其余请求回 `Reply::Ok`；处理完第一条请求后额外推 `Event::Unknown` +
    /// `Event::TurnStart` 两帧，验证客户端"未知事件静默跳过、不断流"。
    // 一条线性的协议脚本：三帧握手 → 请求循环 → 推两帧事件。拆成子函数要把
    // decoder / buf / id_gen / 两个 half 全部当参数来回传，可读性反而更差。
    #[expect(clippy::too_many_lines, reason = "线性协议脚本，拆分反而更难读")]
    async fn run_stub_server(stream: Stream, secret: Secret) {
        let (mut read_half, mut write_half) = stream.into_split();
        let mut decoder = FrameDecoder::new();
        let mut buf = [0_u8; 8192];
        let id_gen = IdGen::default();

        let Some(client_hello_frame) =
            read_client_frame(&mut read_half, &mut decoder, &mut buf).await
        else {
            return;
        };
        let ClientFrame::Hello(client_hello) = client_hello_frame.payload else {
            return;
        };
        let Ok(nonce_s) = DaemonNonce::generate() else {
            return;
        };
        let nonce_c = DaemonNonce::from(client_hello.nonce.0.clone());
        let server_proof = proof(&secret, Domain::Server, &nonce_c);
        let server_hello = zcode_protocol::version::ServerHello {
            hello: Hello::local("test-stub-host"),
            nonce: ProtoNonce(nonce_s.as_str().to_owned()),
            proof: Proof(server_proof),
        };
        if write_server_frame(
            &mut write_half,
            &Envelope::new(id_gen.next_id(), ServerFrame::Hello(server_hello)),
        )
        .await
        .is_err()
        {
            return;
        }

        let Some(auth_frame) = read_client_frame(&mut read_half, &mut decoder, &mut buf).await
        else {
            return;
        };
        let ClientFrame::Auth(ClientAuth {
            proof: client_proof,
        }) = auth_frame.payload
        else {
            return;
        };
        if !verify_proof(&secret, Domain::Client, &nonce_s, &client_proof.0) {
            return;
        }

        let mut pushed_extra_events = false;
        loop {
            let Some(frame) = read_client_frame(&mut read_half, &mut decoder, &mut buf).await
            else {
                return;
            };
            let ClientFrame::Request(request) = frame.payload else {
                continue;
            };
            let reply = match request {
                Request::Ping => Reply::Pong,
                Request::SessionList { cwd } => Reply::Sessions {
                    sessions: vec![SessionSummary {
                        id: SessionId::from(cwd.clone().unwrap_or_default()),
                        title: cwd.clone(),
                        cwd: cwd.unwrap_or_default(),
                        model: "stub".to_owned(),
                        created_ms: 0,
                        updated_ms: 0,
                    }],
                },
                _ => Reply::Ok,
            };
            if write_server_frame(
                &mut write_half,
                &Envelope::reply_to(id_gen.next_id(), frame.id, ServerFrame::Reply(reply)),
            )
            .await
            .is_err()
            {
                return;
            }

            if !pushed_extra_events {
                pushed_extra_events = true;
                if write_server_frame(
                    &mut write_half,
                    &Envelope::new(id_gen.next_id(), ServerFrame::Event(Event::Unknown)),
                )
                .await
                .is_err()
                {
                    return;
                }
                let _ = write_server_frame(
                    &mut write_half,
                    &Envelope::new(
                        id_gen.next_id(),
                        ServerFrame::Event(Event::TurnStart {
                            session: SessionId::from("probe"),
                            user_entry: zcode_protocol::wire::types::EntryId::from("probe-entry"),
                        }),
                    ),
                )
                .await;
            }
        }
    }

    async fn read_client_frame(
        read_half: &mut ReadHalf,
        decoder: &mut FrameDecoder,
        buf: &mut [u8],
    ) -> Option<Envelope<ClientFrame>> {
        loop {
            match decoder.decode::<Envelope<ClientFrame>>() {
                Ok(Some(envelope)) => return Some(envelope),
                Ok(None) => {}
                Err(FrameError::Json(_)) => continue,
                Err(FrameError::TooLarge { .. }) => return None,
            }
            match read_half.read(buf).await {
                Ok(0) | Err(_) => return None,
                Ok(n) => decoder.push(buf.get(..n).unwrap_or_default()),
            }
        }
    }

    async fn write_server_frame(
        write_half: &mut WriteHalf,
        envelope: &Envelope<ServerFrame>,
    ) -> std::io::Result<()> {
        let mut out = Vec::new();
        encode(envelope, &mut out).map_err(std::io::Error::other)?;
        write_half.write_all(&out).await?;
        write_half.flush().await
    }

    /// 造一条绕过 `Host` 的连接：一端喂给桩服务端，另一端包成 [`Connection`] 交回。
    async fn stub_connection() -> Connection {
        let secret = Secret::generate().expect("secret");
        let (client_stream, server_stream) = stream_pair().await.expect("stream_pair");
        let server_secret = secret.clone();
        tokio::spawn(run_stub_server(server_stream, server_secret));
        Connection {
            stream: client_stream,
            mode: ConnectMode::SelfHosted,
            secret,
            _host_task: None,
        }
    }

    /// 完整跑一次三帧握手 + 一次 Request/Reply 往返。
    #[tokio::test]
    async fn handshake_and_request_reply_round_trip() {
        let session = ClientSession::open(stub_connection().await)
            .await
            .expect("握手应当成功");

        let reply = session
            .request(Request::Ping)
            .await
            .expect("Ping 应当拿到回应");
        assert!(matches!(reply, Reply::Pong));

        session.shutdown().await.expect("shutdown 应当成功");
    }

    /// `ClientSession::request` 并发发两条请求各自拿到自己的 reply，不串——桩服务端
    /// 把 `cwd` 原样回显进摘要标题，客户端按 `reply_to` 而不是到达顺序匹配才能对上。
    #[tokio::test]
    async fn concurrent_requests_are_routed_independently() {
        let session = Arc::new(
            ClientSession::open(stub_connection().await)
                .await
                .expect("握手应当成功"),
        );

        let session_a = Arc::clone(&session);
        let call_a = tokio::spawn(async move {
            session_a
                .request(Request::SessionList {
                    cwd: Some("A".to_owned()),
                })
                .await
        });
        let session_b = Arc::clone(&session);
        let call_b = tokio::spawn(async move {
            session_b
                .request(Request::SessionList {
                    cwd: Some("B".to_owned()),
                })
                .await
        });

        let reply_a = call_a
            .await
            .expect("任务 A 不应 panic")
            .expect("A 应当拿到回应");
        let reply_b = call_b
            .await
            .expect("任务 B 不应 panic")
            .expect("B 应当拿到回应");

        let title_of = |reply: Reply| match reply {
            Reply::Sessions { sessions } => sessions
                .into_iter()
                .next()
                .and_then(|summary| summary.title)
                .expect("桩服务端总是回一条摘要"),
            other => panic!("期望 Reply::Sessions，拿到了 {other:?}"),
        };
        assert_eq!(title_of(reply_a), "A");
        assert_eq!(title_of(reply_b), "B");
    }

    /// 未知事件（`Event::Unknown`）静默跳过，事件流不断：紧随其后的真实事件仍然能收到。
    #[tokio::test]
    async fn unknown_event_is_dropped_without_breaking_the_stream() {
        let session = ClientSession::open(stub_connection().await)
            .await
            .expect("握手应当成功");
        let mut events = session.take_events().expect("事件接收端只能取一次");
        assert!(session.take_events().is_none(), "第二次取事件必须是 None");

        // 触发桩服务端在第一条请求后推 Unknown + TurnStart 两帧。
        session
            .request(Request::Ping)
            .await
            .expect("Ping 应当拿到回应");

        let event = tokio::time::timeout(std::time::Duration::from_secs(5), events.recv())
            .await
            .expect("等事件不应超时")
            .expect("事件流不应提前结束");
        assert!(
            matches!(event, Event::TurnStart { .. }),
            "Unknown 事件应当被静默跳过，收到的应当是紧随其后的 TurnStart，实际是 {event:?}"
        );
    }
}
