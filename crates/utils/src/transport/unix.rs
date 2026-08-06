//! Unix domain socket 实现。类型直接别名到 tokio，零包装成本。

use std::io;
use std::path::Path;

/// 已连接的双向字节流。
pub type Stream = tokio::net::UnixStream;
/// [`Stream`] 的读半边。用 owned half 而非 [`tokio::io::split`]：后者走 `BiLock`，多一次原子操作。
pub type ReadHalf = tokio::net::unix::OwnedReadHalf;
/// [`Stream`] 的写半边。
pub type WriteHalf = tokio::net::unix::OwnedWriteHalf;

/// 监听本机端点，接受客户端连接。
#[derive(Debug)]
pub struct Listener {
    inner: tokio::net::UnixListener,
}

impl Listener {
    /// 绑定到 `path`。
    ///
    /// 路径已存在时返回 [`io::ErrorKind::AddrInUse`]，**本函数不会替你删掉它**——原因见模块文档。
    pub fn bind(path: &Path) -> io::Result<Self> {
        tokio::net::UnixListener::bind(path).map(|inner| Self { inner })
    }

    /// 接受下一个连接。
    ///
    /// 取 `&mut self` 是为了与 Windows 侧签名一致（那边每次 accept 必须换一个 pipe 实例）。
    pub async fn accept(&mut self) -> io::Result<Stream> {
        self.inner.accept().await.map(|(stream, _addr)| stream)
    }
}

/// 造一对已连接的流，两端都在本进程内。
///
/// 用途是把"进程内客户端"接到与真跨进程客户端**完全相同**的连接处理函数上，
/// 从而只保留一条执行路径。抄源：jcode `crates/jcode-base/src/gateway.rs:211-220`。
#[expect(
    clippy::unused_async,
    reason = "签名要与 Windows 侧一致：那边必须 await 服务端的 connect 握手"
)]
pub async fn stream_pair() -> io::Result<(Stream, Stream)> {
    Stream::pair()
}
