//! 跨平台本机 IPC 传输：Unix domain socket / Windows named pipe。
//!
//! 上层（server accept loop、client connect、relay）**一行 `cfg` 都不写**，只认本模块导出的
//! [`Listener`] / [`Stream`] / [`ReadHalf`] / [`WriteHalf`] 与 [`stream_pair`]。
//! 平台差异全部收在 `unix.rs` 与 `windows.rs` 两个实现里。
//!
//! 抄源：jcode `crates/jcode-base/src/transport/mod.rs:1-8`、`unix.rs:1-19`、`windows.rs:11-116`。
//!
//! # 本模块**不做**的事
//!
//! - **不做陈旧 socket 回收。** Unix 上 `bind` 到已存在的路径会返回 `AddrInUse`；判断"上一个
//!   持有者是否真的死了"是 daemon 层策略，且必须是双条件（无活监听 **且** 能拿到独占锁），
//!   见 jcode `crates/jcode-app-core/src/server/socket.rs:88-137`。单条件回收会删掉活着的
//!   接任者的 socket。
//! - **不做探活。** Windows 上有一个专门的坑：先做一次非阻塞探测会临时占掉唯一已发布的 pipe
//!   实例，紧接着的真 connect 会在 `ERROR_PIPE_BUSY` 重试循环里等下去
//!   （jcode `server/socket.rs:72-83`）。所以**不要用"连一次看看"来探活**，改用注册文件 +
//!   独占锁。[`Stream::connect`] 的重试次数因此是有界的。
//!
//! # 单实例语义
//!
//! Windows 侧 [`Listener::bind`] 用 `first_pipe_instance(true)`：同名 pipe 的第二个 `bind`
//! 直接失败，因此 bind 本身就是互斥。Unix 侧没有这个性质，需要额外的 lock 文件。

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::{Listener, ReadHalf, Stream, WriteHalf, stream_pair};

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::{Listener, ReadHalf, Stream, WriteHalf, stream_pair};

#[cfg(test)]
mod tests {
    use std::io;

    use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};

    use super::{Listener, Stream, stream_pair};

    /// 在一对已连接的流上跑一次双向 NDJSON 往返。
    async fn round_trip(a: Stream, b: Stream) -> io::Result<()> {
        let (a_read, mut a_write) = a.into_split();
        let (b_read, mut b_write) = b.into_split();

        a_write.write_all(b"ping\n").await?;
        a_write.flush().await?;
        let mut b_lines = BufReader::new(b_read).lines();
        assert_eq!(b_lines.next_line().await?.as_deref(), Some("ping"));

        b_write.write_all(b"pong\n").await?;
        b_write.flush().await?;
        let mut a_lines = BufReader::new(a_read).lines();
        assert_eq!(a_lines.next_line().await?.as_deref(), Some("pong"));

        Ok(())
    }

    #[tokio::test]
    async fn stream_pair_carries_bytes_both_ways() -> io::Result<()> {
        let (a, b) = stream_pair().await?;
        round_trip(a, b).await
    }

    #[tokio::test]
    async fn bind_accept_connect_round_trip() -> io::Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("zcode-transport-test.sock");

        let mut listener = Listener::bind(&path)?;
        let accept = tokio::spawn(async move { listener.accept().await });
        let client = Stream::connect(&path).await?;
        let server = accept.await.map_err(io::Error::other)??;

        round_trip(server, client).await
    }

    /// `accept()` 被取消后，listener 必须仍然可用。
    ///
    /// 这是一条真机抓到的回归：Windows 侧早期实现在第一次 poll、任何 `await` 之前就
    /// `self.idle.take()`，被 `tokio::time::timeout` 打断后实例永久丢失，下一次 accept
    /// 直接 `BrokenPipe`。而 [`crate::daemon::ReadyChannel::wait`] 正是每 50 ms 轮一次
    /// （好在等待期间 `try_wait` 子进程），于是"拉起 daemon"这条路径**必然**失败。
    /// Unix 侧 `UnixListener::accept` 本就取消安全，本测试对两个平台都成立。
    #[tokio::test]
    async fn accept_stays_usable_after_being_cancelled() -> io::Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("zcode-transport-cancel.sock");
        let mut listener = Listener::bind(&path)?;

        // 没有客户端，这次 accept 必定超时并被丢弃。
        let cancelled =
            tokio::time::timeout(std::time::Duration::from_millis(30), listener.accept()).await;
        assert!(cancelled.is_err(), "无人连接时这次 accept 应当超时");

        // 被取消之后照样能接下一个连接。
        let accept = tokio::spawn(async move { listener.accept().await });
        let client = Stream::connect(&path).await?;
        let server = accept.await.map_err(io::Error::other)??;

        round_trip(server, client).await
    }

    #[tokio::test]
    async fn second_bind_to_live_endpoint_fails() -> io::Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("zcode-transport-busy.sock");

        let _listener = Listener::bind(&path)?;
        assert!(
            Listener::bind(&path).is_err(),
            "同一端点被占用时 bind 必须失败——单实例互斥依赖这个性质"
        );
        Ok(())
    }
}
