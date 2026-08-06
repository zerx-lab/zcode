//! Unix 专用：有界的 `CSI 6n`（Device Status Report）光标位置探测。
//!
//! # 调用前提：只能在事件流缺席或已暂停时调用
//!
//! crossterm 公开的 `position()` 助手会等待终端回复长达数秒，且与 crossterm 自己维护的
//! event stream 抢同一个输入流。本模块改为发一次性的短促查询，用调用方给定的超时预算
//! 有界等待；但代价是：**读到的字节直接从终端输入流里被吃掉，不会回放给别的读者**。
//! 因此只能在没有事件流在跑、或事件流已经显式暂停期间调用——否则探针和输入 reader
//! 抢 stdin，终端的 CPR 回复字节会被吞掉，或者反过来把终端的其它输出（比如 focus
//! report）泄漏成"用户按键"喂给上层（照抄 codex-rs/tui/src/terminal_probe.rs:10、
//! :269-271 "short, exclusive probe windows" 的结论）。
//!
//! # 抄录来源
//!
//! `codex-rs/tui/src/terminal_probe.rs:62-244`——只搬 `cursor_position` 这一条路径；
//! 不含该文件里的 startup probe、OSC 10/11 默认前景/背景色查询、以及 Windows 分支。

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::time::{Duration, Instant};

use ratatui::layout::Position;

/// 每次探测的有界预算。不支持 DSR 的终端只需要为一次查询付一次等待，不会拖慢启动/恢复
/// （codex `terminal_probe.rs:18`）。
pub const DEFAULT_TIMEOUT: Duration = Duration::from_millis(100);

/// 查询终端光标位置：写 `CSI 6n`，在 `timeout` 预算内等第一个完整的 CPR
/// （`CSI {row};{col} R`）响应。
///
/// 终端不支持 DSR，或者响应没能在预算内到达，都返回 `Ok(None)`——这不是错误，调用方
/// 应当保留自己手上的旧值（codex terminal_probe.rs:284-289）。
///
/// **只能在调用方已经暂停 crossterm 事件流期间调用**：见本模块文档顶部的说明。
pub fn cursor_position(timeout: Duration) -> io::Result<Option<Position>> {
    let tty = Tty::open()?;
    tty.write_all(b"\x1b[6n")?;
    read_until(&tty, timeout, |buffer| scan_cpr(buffer).map(|(pos, _)| pos))
}

/// 探测期间临时借用的终端句柄。
///
/// 优先复制（`dup`）进程自己的 stdin/stdout：终端的响应字节会被投递到 crossterm 读的
/// 同一个输入流上，必须用复制出来的 fd 操作，这样探测结束、句柄销毁时只影响这份复制品，
/// 不会动到进程真正的 stdio（codex terminal_probe.rs:82-83,95-115）。少数嵌入式/重定向
/// 环境里 stdio 不是终端、`dup` 之后也用不了 DSR，这时回退到单独打开 `/dev/tty`。
struct Tty {
    reader: OwnedFd,
    writer: OwnedFd,
    /// `reader` 在被本结构体改成 `O_NONBLOCK` 之前的原始 file status flags。
    original_flags: libc::c_int,
}

impl Tty {
    /// 优先 dup 进程 stdin/stdout；两者之一失败就整体回退到单独打开的 `/dev/tty`。
    fn open() -> io::Result<Self> {
        match (dup_fd(libc::STDIN_FILENO), dup_fd(libc::STDOUT_FILENO)) {
            (Ok(reader), Ok(writer)) => Self::new(reader, writer),
            (reader, writer) => {
                let dup_err = reader.err().or_else(|| writer.err()).map_or_else(
                    || "unknown stdio duplicate error".to_string(),
                    |err| err.to_string(),
                );
                let reader = open_tty_reader().map_err(|err| {
                    io::Error::new(
                        err.kind(),
                        format!(
                            "dup 进程 stdin/stdout 失败（{dup_err}），\
                             回退打开 /dev/tty 读端也失败：{err}"
                        ),
                    )
                })?;
                let writer = open_tty_writer().map_err(|err| {
                    io::Error::new(
                        err.kind(),
                        format!(
                            "dup 进程 stdin/stdout 失败（{dup_err}），\
                             回退打开 /dev/tty 写端也失败：{err}"
                        ),
                    )
                })?;
                Self::new(reader, writer)
            }
        }
    }

    /// `reader`/`writer` 必须是两份独立的 file description：只把 `reader` 设为
    /// `O_NONBLOCK`，如果两者共享同一份 description，写入会在终端产生背压时莫名其妙地
    /// 提前失败成 `WouldBlock`。
    fn new(reader: OwnedFd, writer: OwnedFd) -> io::Result<Self> {
        let raw = reader.as_raw_fd();
        // SAFETY: F_GETFL 只读取这份 open file description 当前的 status flags，不修改
        // 任何状态，也不涉及内存安全。
        #[allow(unsafe_code)]
        let original_flags = unsafe { libc::fcntl(raw, libc::F_GETFL) };
        if original_flags == -1 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: F_SETFL 只修改 `raw`（即 `reader`）这一份 open file description 的
        // status flags；这里加的 O_NONBLOCK 不影响其它字段/内存。
        #[allow(unsafe_code)]
        let set = unsafe { libc::fcntl(raw, libc::F_SETFL, original_flags | libc::O_NONBLOCK) };
        if set == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            reader,
            writer,
            original_flags,
        })
    }

    /// 发送探测请求。直接对裸 fd 调用 `libc::write`，天然没有用户态缓冲；如果这里改成走
    /// `std::fs::File`/`BufWriter` 之类的缓冲 writer，就必须再显式 `flush()`——漏掉的话
    /// 请求字节会一直留在用户态缓冲区里，终端永远收不到查询，`poll_readable` 会一路
    /// 等到超时都等不到回复。
    fn write_all(&self, mut bytes: &[u8]) -> io::Result<()> {
        while !bytes.is_empty() {
            // SAFETY: `writer` 是本结构体独占的 fd；`bytes` 的指针与长度来自一个有效的
            // `&[u8]`，`libc::write` 只会读取该范围内的字节，不会越界。
            #[allow(unsafe_code)]
            let written = unsafe {
                libc::write(
                    self.writer.as_raw_fd(),
                    bytes.as_ptr().cast::<libc::c_void>(),
                    bytes.len(),
                )
            };
            if written < 0 {
                let err = io::Error::last_os_error();
                if err.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(err);
            }
            let Ok(written) = usize::try_from(written) else {
                return Err(io::Error::other("libc::write 返回了负的写入长度"));
            };
            if written == 0 {
                return Err(io::Error::from(io::ErrorKind::WriteZero));
            }
            let Some(rest) = bytes.get(written..) else {
                return Err(io::Error::other("libc::write 报告的写入长度超过缓冲区"));
            };
            bytes = rest;
        }
        Ok(())
    }

    /// 排空当前已经到达内核缓冲区的字节，遇到 `EAGAIN`/`EWOULDBLOCK` 就停下——`reader`
    /// 已经被设为非阻塞（见 [`Tty::new`]），这里绝不会阻塞等待更多数据；有界等待交给
    /// [`Tty::poll_readable`]。
    fn read_available(&self, buffer: &mut Vec<u8>) -> io::Result<()> {
        let mut chunk = [0_u8; 256];
        loop {
            // SAFETY: `chunk` 是本函数栈上独占、长度已知的缓冲区；`libc::read` 至多写入
            // `chunk.len()` 字节，不会越界写。
            #[allow(unsafe_code)]
            let count = unsafe {
                libc::read(
                    self.reader.as_raw_fd(),
                    chunk.as_mut_ptr().cast::<libc::c_void>(),
                    chunk.len(),
                )
            };
            if count > 0 {
                let Ok(count) = usize::try_from(count) else {
                    return Err(io::Error::other("libc::read 返回了负的读取长度"));
                };
                let Some(data) = chunk.get(..count) else {
                    return Err(io::Error::other("libc::read 报告的长度超过缓冲区"));
                };
                buffer.extend_from_slice(data);
                continue;
            }
            if count == 0 {
                // 对端关闭了写端（比如终端被拔掉）；没有更多数据可读。
                return Ok(());
            }
            let err = io::Error::last_os_error();
            if matches!(
                err.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ) {
                return Ok(());
            }
            return Err(err);
        }
    }

    /// 有界等待 `reader` 变为可读；绝不阻塞读——这是"有界等待"的唯一正确实现方式
    /// （codex terminal_probe.rs:171-189）。超时预算耗尽就返回 `false`，交给调用方决定
    /// 是放弃还是再读一轮。
    fn poll_readable(&self, timeout: Duration) -> io::Result<bool> {
        let mut fd = libc::pollfd {
            fd: self.reader.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let deadline = Instant::now() + timeout;
        loop {
            let now = Instant::now();
            if now >= deadline {
                return Ok(false);
            }
            let remaining_ms = deadline.saturating_duration_since(now).as_millis();
            let timeout_ms = libc::c_int::try_from(remaining_ms).unwrap_or(libc::c_int::MAX);
            // SAFETY: `fd` 指向栈上单个 `pollfd`，`nfds=1` 与之匹配；`libc::poll` 只会
            // 读写这一个元素，不会越界访问。
            #[allow(unsafe_code)]
            let result = unsafe { libc::poll(&raw mut fd, 1, timeout_ms) };
            if result > 0 {
                return Ok((fd.revents & libc::POLLIN) != 0);
            }
            if result == 0 {
                return Ok(false);
            }
            let err = io::Error::last_os_error();
            if err.kind() != io::ErrorKind::Interrupted {
                return Err(err);
            }
        }
    }
}

impl Drop for Tty {
    fn drop(&mut self) {
        // SAFETY: F_SETFL 只修改 `reader` 这份 open file description 的 status flags，
        // 恢复成 `Tty::new` 记录的原始值。
        //
        // `reader` 由 `dup(2)` 复制而来时，与被复制的原始 fd（进程真正的 stdin）共享同一份
        // 内核 open file description，因此 `Tty::new` 里加的 O_NONBLOCK 会立刻反映到进程
        // 真正的 stdin 上。这里不恢复的话，探测结束后进程 stdin 会一直停在非阻塞态，
        // 后续任何期望阻塞读的代码都会异常收到 `EAGAIN`
        // （plans/tui/platform.md:205，codex terminal_probe.rs:66-67,128）。
        #[allow(unsafe_code)]
        let _ = unsafe { libc::fcntl(self.reader.as_raw_fd(), libc::F_SETFL, self.original_flags) };
        // `reader`/`writer` 两个 OwnedFd 字段随本结构体在函数返回后按声明顺序自动析构，
        // 各自 close(2) 恰好一次；这里不手写 `libc::close`，避免和 OwnedFd 的 Drop
        // 产生 double-close。
    }
}

/// 复制一个进程 stdio 描述符，返回独立管理生命周期的 [`OwnedFd`]。
fn dup_fd(fd: RawFd) -> io::Result<OwnedFd> {
    // SAFETY: `libc::dup` 返回一个全新的、调用者独占所有权的 fd（与 `fd` 共享同一份
    // open file description，但描述符本身是新分配的）；只有返回值合法（非负）时，
    // 下面才会把它交给 `OwnedFd` 接管。
    #[allow(unsafe_code)]
    let duplicated = unsafe { libc::dup(fd) };
    if duplicated < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `duplicated` 是上一行 `libc::dup` 刚返回、尚未被任何其它代码持有或关闭过的
    // fd，满足 `OwnedFd::from_raw_fd` 要求的"调用者独占所有权"前提。
    #[allow(unsafe_code)]
    let owned = unsafe { OwnedFd::from_raw_fd(duplicated) };
    Ok(owned)
}

/// `dup` 失败时的回退路径：部分嵌入式/重定向环境没有可 dup 的终端 stdio，但仍然暴露
/// controlling terminal，可以单独打开读端。
fn open_tty_reader() -> io::Result<OwnedFd> {
    Ok(std::fs::OpenOptions::new()
        .read(true)
        .open("/dev/tty")?
        .into())
}

/// 同 [`open_tty_reader`]，但打开写端。
fn open_tty_writer() -> io::Result<OwnedFd> {
    Ok(std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/tty")?
        .into())
}

/// 反复"读可用字节 + 有界 poll"，直到 `parse` 从累积缓冲区里识别出一个响应，或者
/// `timeout` 预算耗尽。
///
/// 累积的 `buffer` 可能混着与本次探测无关的字节（比如恢复时终端先发的 focus report）；
/// 这个函数不负责把它们回放回正常的输入流，所以只能在短促、独占的探测窗口里调用——
/// 契约见 [`cursor_position`] 顶部的模块文档。
fn read_until<T>(
    tty: &Tty,
    timeout: Duration,
    mut parse: impl FnMut(&[u8]) -> Option<T>,
) -> io::Result<Option<T>> {
    let deadline = Instant::now() + timeout;
    let mut buffer = Vec::new();
    loop {
        tty.read_available(&mut buffer)?;
        if let Some(value) = parse(&buffer) {
            return Ok(Some(value));
        }
        let now = Instant::now();
        if now >= deadline {
            return Ok(None);
        }
        if !tty.poll_readable(deadline.saturating_duration_since(now))? {
            return Ok(None);
        }
    }
}

/// 在 `haystack` 里找 `needle` 第一次出现的起始下标。本文件只用它找 2 字节的
/// `ESC [`（CSI）前缀，`needle` 非空且不长于 `haystack` 时才有意义。
fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// 在已读缓冲区里扫描第一个完整的 CPR（`CSI {row};{col} R`）响应。
///
/// resume 之后终端可能先发一个 focus report（`CSI I` / `CSI O`）再发光标响应
/// （codex terminal_probe.rs:236-239 "Resume can emit a focus report immediately before
/// the cursor-position response"）；因此这里逐个扫描 `ESC [` 起点，跳过终止字节不是
/// `R` 的 CSI 序列，而不是假定缓冲区里第一段字节就是答案——漏了这一步，focus report
/// 会被误当成光标响应解析失败，或者反过来把它的字节泄漏进上层当按键处理。
///
/// 返回 `(位置, 已消费字节数)`。`None` 分两种情况：完全没找到 CSI 起点，或者看到的最后
/// 一段 CSI 序列还没读完（比如只到了 `\x1b[12;`）——两种情况调用方都必须继续 poll 等
/// 更多字节，不能当成"不支持 DSR"处理。
fn scan_cpr(buf: &[u8]) -> Option<(Position, usize)> {
    let mut cursor = 0usize;
    loop {
        let tail = buf.get(cursor..)?;
        let rel_start = find_subslice(tail, b"\x1b[")?;
        let body_start = cursor + rel_start + 2;
        let body = buf.get(body_start..)?;
        // ECMA-48：CSI 序列的终止字节落在 0x40..=0x7E（参数/中间字节都更小）。找不到
        // 就说明这段序列还没读完，必须整体返回 None 等更多字节，不能跳过它继续找下一个
        // `ESC [`——那样会把半截序列的尾巴误认成下一条序列的开头。
        let term_rel = body.iter().position(|byte| (0x40..=0x7e).contains(byte))?;
        let terminator = *body.get(term_rel)?;
        let payload = body.get(..term_rel)?;
        let consumed = body_start + term_rel + 1;
        if terminator == b'R'
            && let Some(pos) = parse_cpr_payload(payload)
        {
            return Some((pos, consumed));
        }
        cursor = consumed;
    }
}

/// 解析 CPR payload（`ESC [` 与终止字节 `R` 之间的部分，形如 `"{row};{col}"`）。
///
/// 终端回的是 1-based 行列号；本模块约定返回 0-based [`Position`]（与 ratatui 的坐标系
/// 一致，也是全 crate 对光标坐标的统一约定）。少了减 1 这一步会让 viewport 锚点整体
/// 下移一行。数字溢出、非数字字符、缺少分号都返回 `None`，绝不 panic。
fn parse_cpr_payload(payload: &[u8]) -> Option<Position> {
    let text = std::str::from_utf8(payload).ok()?;
    let (row, col) = text.split_once(';')?;
    let row: u16 = row.parse().ok()?;
    let col: u16 = col.parse().ok()?;
    let row = row.checked_sub(1)?;
    let col = col.checked_sub(1)?;
    Some(Position { x: col, y: row })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_clean_response_as_zero_based() {
        let (pos, consumed) = scan_cpr(b"\x1b[12;34R").expect("应识别出完整的 CPR 响应");
        assert_eq!(pos, Position { x: 33, y: 11 });
        assert_eq!(consumed, b"\x1b[12;34R".len());
    }

    #[test]
    fn skips_leading_focus_report() {
        let (pos, _consumed) =
            scan_cpr(b"\x1b[I\x1b[12;34R").expect("应跳过 focus report 找到 CPR");
        assert_eq!(pos, Position { x: 33, y: 11 });
    }

    #[test]
    fn returns_none_for_incomplete_response() {
        assert_eq!(scan_cpr(b"\x1b[12;"), None);
    }

    #[test]
    fn takes_first_response_when_multiple_are_present() {
        let (pos, consumed) = scan_cpr(b"\x1b[1;1R\x1b[2;2R").expect("应取第一个完整响应");
        assert_eq!(pos, Position { x: 0, y: 0 });
        assert_eq!(consumed, b"\x1b[1;1R".len());
    }

    #[test]
    fn returns_none_without_panicking_on_bad_params() {
        // 行号解析成 u16 时溢出。
        assert_eq!(scan_cpr(b"\x1b[99999999999;1R"), None);
        // 非数字参数。
        assert_eq!(scan_cpr(b"\x1b[abc;1R"), None);
        // 缺少分号，split_once 找不到分隔符。
        assert_eq!(scan_cpr(b"\x1b[12R"), None);
    }
}
