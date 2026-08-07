//! Windows named pipe 实现，包装成与 Unix 侧同名的类型。
//!
//! 抄源：jcode `crates/jcode-base/src/transport/windows.rs:11-116`。

use std::io;
use std::path::Path;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use sha2::{Digest as _, Sha256};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::windows::named_pipe::{
    ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions,
};

/// `ERROR_PIPE_BUSY`：所有已发布的 pipe 实例都已被占用，稍后重试即可。
const ERROR_PIPE_BUSY: i32 = 231;
/// `connect` 遇到 `ERROR_PIPE_BUSY` 时的重试间隔。
const CONNECT_RETRY_INTERVAL: Duration = Duration::from_millis(50);
/// `connect` 的重试次数上限。
///
/// 必须有界：jcode `crates/jcode-app-core/src/server/socket.rs:72-83` 记录了无界重试的后果——
/// 一次探活占掉唯一 pipe 实例后，紧接着的 connect 会永远等下去。
const CONNECT_RETRY_LIMIT: u32 = 100;

/// 把文件系统路径映射成稳定且唯一的 pipe 名。
///
/// 取路径的 file stem 便于人眼识别，再拼上**小写归一化后**整条路径的 SHA-256 前 16 个 hex：
/// Windows 路径大小写不敏感，不归一化会让 `C:\X\a.sock` 与 `c:\x\a.sock` 落到两条不同的 pipe。
fn pipe_name(path: &Path) -> String {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("endpoint");
    let digest = Sha256::digest(path.to_string_lossy().to_lowercase().as_bytes());
    let mut hash = String::with_capacity(16);
    for byte in digest.iter().take(8) {
        use std::fmt::Write as _;
        let _ = write!(hash, "{byte:02x}");
    }
    format!(r"\\.\pipe\zcode-{stem}-{hash}")
}

/// 已连接的双向字节流。
///
/// named pipe 的两端是不同类型，所以这里必须是 enum；Unix 侧则是单一的 `UnixStream`。
#[derive(Debug)]
pub enum Stream {
    /// 由 [`Listener::accept`] 产生的服务端一侧。
    Server(NamedPipeServer),
    /// 由 [`Stream::connect`] 产生的客户端一侧。
    Client(NamedPipeClient),
}

/// [`Stream`] 的读半边。
pub type ReadHalf = tokio::io::ReadHalf<Stream>;
/// [`Stream`] 的写半边。
pub type WriteHalf = tokio::io::WriteHalf<Stream>;

impl Stream {
    /// 连接到 `path` 对应的端点。
    ///
    /// 所有实例都忙时每 50ms 重试，最多 100 次（约 5s），之后返回最后一次的 `ERROR_PIPE_BUSY`。
    /// 常量见本文件的 `CONNECT_RETRY_INTERVAL` / `CONNECT_RETRY_LIMIT`。
    pub async fn connect(path: &Path) -> io::Result<Self> {
        let name = pipe_name(path);
        let mut attempts = 0_u32;
        loop {
            match ClientOptions::new().open(&name) {
                Ok(client) => return Ok(Self::Client(client)),
                Err(err)
                    if err.raw_os_error() == Some(ERROR_PIPE_BUSY)
                        && attempts < CONNECT_RETRY_LIMIT =>
                {
                    attempts += 1;
                    tokio::time::sleep(CONNECT_RETRY_INTERVAL).await;
                }
                Err(err) => return Err(err),
            }
        }
    }

    /// 拆成读写两半。
    #[must_use]
    pub fn into_split(self) -> (ReadHalf, WriteHalf) {
        tokio::io::split(self)
    }
}

impl AsyncRead for Stream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Server(inner) => Pin::new(inner).poll_read(cx, buf),
            Self::Client(inner) => Pin::new(inner).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for Stream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            Self::Server(inner) => Pin::new(inner).poll_write(cx, buf),
            Self::Client(inner) => Pin::new(inner).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Server(inner) => Pin::new(inner).poll_flush(cx),
            Self::Client(inner) => Pin::new(inner).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Server(inner) => Pin::new(inner).poll_shutdown(cx),
            Self::Client(inner) => Pin::new(inner).poll_shutdown(cx),
        }
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            Self::Server(inner) => Pin::new(inner).poll_write_vectored(cx, bufs),
            Self::Client(inner) => Pin::new(inner).poll_write_vectored(cx, bufs),
        }
    }

    fn is_write_vectored(&self) -> bool {
        match self {
            Self::Server(inner) => inner.is_write_vectored(),
            Self::Client(inner) => inner.is_write_vectored(),
        }
    }
}

/// 监听本机端点，接受客户端连接。
#[derive(Debug)]
pub struct Listener {
    name: String,
    /// 当前待接入的空闲实例。`accept` 取走它并立刻补一个新的。
    idle: Option<NamedPipeServer>,
}

impl Listener {
    /// 绑定到 `path`。
    ///
    /// 用 `first_pipe_instance(true)`：同名 pipe 已被别的进程持有时直接失败，
    /// 所以 **bind 本身就是单实例互斥**——这一点 Unix 侧没有，那边需要额外的 lock 文件。
    pub fn bind(path: &Path) -> io::Result<Self> {
        let name = pipe_name(path);
        let idle = ServerOptions::new()
            .first_pipe_instance(true)
            .create(&name)?;
        Ok(Self {
            name,
            idle: Some(idle),
        })
    }

    /// 接受下一个连接。
    ///
    /// 顺序是"等到连接 → 立刻补下一个空闲实例"。补实例必然晚于 connect 返回，中间存在一个
    /// 无空闲实例的窗口；落在窗口里的客户端会收到 `ERROR_PIPE_BUSY`，由
    /// [`Stream::connect`] 的有界重试吸收。
    ///
    /// # 取消安全
    ///
    /// **本方法是取消安全的**：`self.idle` 只在 `connect()` 成功返回**之后**才被取走。
    /// 这一点是必须的——调用方普遍会把它塞进 `tokio::time::timeout` 或 `select!`
    /// （例如 `crate::daemon::ReadyChannel::wait` 每 50 ms 轮一次，好在等待期间
    /// `try_wait` 子进程）。早期写法在第一次 poll、任何 `await` 之前就 `take()`，
    /// future 一被丢弃实例就永久丢失，**下一次 accept 必然报 `BrokenPipe`**——
    /// 症状是 `zcode run` 拉起 daemon 时立刻失败。`NamedPipeServer::connect` 本身
    /// 由 tokio 保证取消安全，所以只要不提前 `take()`，整个方法就是取消安全的。
    pub async fn accept(&mut self) -> io::Result<Stream> {
        let pending = self.idle.as_mut().ok_or_else(missing_instance)?;
        pending.connect().await?;
        // 走到这里说明连接已建立；此时才取走它，被取消的路径不会碰到这一行。
        let server = self.idle.take().ok_or_else(missing_instance)?;
        self.idle = Some(ServerOptions::new().create(&self.name)?);
        Ok(Stream::Server(server))
    }
}

/// `idle` 为 `None` 时的错误。理论上只在"补实例失败"之后出现——见 [`Listener::accept`]
/// 的取消安全说明。
fn missing_instance() -> io::Error {
    io::Error::new(
        io::ErrorKind::BrokenPipe,
        "listener 没有空闲 pipe 实例：上一次 accept 补实例失败",
    )
}

/// 造一对已连接的流，两端都在本进程内。
///
/// 用途是把"进程内客户端"接到与真跨进程客户端**完全相同**的连接处理函数上，
/// 从而只保留一条执行路径。抄源：jcode `crates/jcode-base/src/gateway.rs:211-220`。
///
/// Windows 没有 `socketpair`，所以走一条一次性的匿名 pipe：名字用 pid + 进程内单调计数器，
/// 保证同机同进程并发调用不会撞名。
pub async fn stream_pair() -> io::Result<(Stream, Stream)> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let seq = NEXT.fetch_add(1, Ordering::Relaxed);
    let name = format!(r"\\.\pipe\zcode-pair-{}-{seq}", std::process::id());

    let server = ServerOptions::new()
        .first_pipe_instance(true)
        .create(&name)?;
    let client = ClientOptions::new().open(&name)?;
    // 客户端已连上，所以这次握手立即返回；省掉它则服务端一侧尚未进入已连接状态。
    server.connect().await?;
    Ok((Stream::Server(server), Stream::Client(client)))
}
