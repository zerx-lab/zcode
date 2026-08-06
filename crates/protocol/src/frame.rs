//! NDJSON 分帧：一行一帧，`\n` 分隔。
//!
//! # 解帧器的四个约束缺一不可
//!
//! 抄源：jcode `crates/jcode-tui/src/tui/backend.rs:230-296`。四条各自防一类 bug：
//!
//! 1. **缓冲区跨调用持久**。读循环通常在 `tokio::select!` 里，future 被取消时不能丢半包。
//! 2. **扫描游标**。引导帧可达数十 MB、跨几千次 socket read；每次从 0 开始找 `\n` 会让分帧
//!    退化成 $O(n^2)$ 并反压写端。
//! 3. **帧长上限**。恶意或失控的对端不得把本端内存撑爆。
//! 4. **容量回缩**。一条大帧不该把缓冲区容量钉死整个连接生命周期。
//!
//! # 单行损坏只跳这一行
//!
//! [`FrameDecoder::decode`] 返回 [`FrameError::Json`] 时，那一行**已被消费**，可以继续调用
//! 取下一帧。抄源：jcode `crates/jcode-base/src/session/persistence.rs:66-125` —— 旧实现在
//! 第一个坏字节处截断整个 transcript，症状是用户报"我最后那条 prompt 不见了"。
//!
//! 例外是 [`FrameError::TooLarge`]：那之后解帧器状态已无意义，调用方**必须断开连接**。

use serde::Serialize;
use serde::de::DeserializeOwned;

/// 单帧字节上限，默认 256 MiB。
///
/// 取值理由（同 jcode `crates/jcode-tui/src/tui/backend.rs:270-272`）：引导帧可能内嵌图像，
/// 所以必须给得很大；但仍要有界，否则失控对端能直接撑爆本端内存。
pub const MAX_FRAME_BYTES: usize = 256 * 1024 * 1024;

/// 触发容量回缩的下限。缓冲区容量超过它才考虑回缩。
const SHRINK_CAPACITY_THRESHOLD: usize = 256 * 1024;
/// 回缩后保留的容量。
///
/// 取值理由：要"明显高于常规流式帧的大小"，否则每帧都在扩/缩之间抖动。
const SHRINK_RETAIN_CAPACITY: usize = 64 * 1024;

/// 分帧失败。
#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    /// 累积字节已超过上限却仍未见 `\n`。**调用方必须断开连接**：解帧器状态此后无意义。
    #[error("帧长度已达 {len} 字节，超过上限 {limit}")]
    TooLarge {
        /// 已累积的字节数。
        len: usize,
        /// 生效的上限。
        limit: usize,
    },
    /// 这一行不是合法 JSON，或结构不符。该行已被消费，可以继续取下一帧。
    #[error("帧不是合法 JSON")]
    Json(#[from] serde_json::Error),
}

/// 把 `value` 编码成一帧，追加到 `out`。
///
/// 紧凑 JSON 内不会出现裸 `\n`（字符串里的换行会被转义成 `\\n`），所以行分隔是安全的。
pub fn encode<T: Serialize + ?Sized>(value: &T, out: &mut Vec<u8>) -> Result<(), FrameError> {
    serde_json::to_writer(&mut *out, value)?;
    out.push(b'\n');
    Ok(())
}

/// 增量解帧器：喂字节，取帧。
#[derive(Debug)]
pub struct FrameDecoder {
    buf: Vec<u8>,
    /// 下一次找 `\n` 的起点。已扫过的前缀不再重扫——这是约束 2。
    scan_from: usize,
    max_frame: usize,
}

impl Default for FrameDecoder {
    fn default() -> Self {
        Self::with_max_frame(MAX_FRAME_BYTES)
    }
}

impl FrameDecoder {
    /// 用默认上限 [`MAX_FRAME_BYTES`] 构造。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 用自定义帧长上限构造。
    #[must_use]
    pub fn with_max_frame(max_frame: usize) -> Self {
        Self {
            buf: Vec::new(),
            scan_from: 0,
            max_frame,
        }
    }

    /// 喂入一段刚读到的字节。
    pub fn push(&mut self, chunk: &[u8]) {
        self.buf.extend_from_slice(chunk);
    }

    /// 当前尚未成帧的字节数。
    #[must_use]
    pub fn buffered(&self) -> usize {
        self.buf.len()
    }

    /// 当前缓冲区容量。仅用于测试与诊断。
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.buf.capacity()
    }

    /// 取出下一帧并反序列化。
    ///
    /// 返回 `Ok(None)` 表示当前字节还凑不满一帧，等下一次 [`push`](Self::push)。
    /// 空行与纯空白行被跳过——SSE 风格的心跳会发它们。
    pub fn decode<T: DeserializeOwned>(&mut self) -> Result<Option<T>, FrameError> {
        loop {
            let Some(line_end) = self.find_newline()? else {
                return Ok(None);
            };
            let line = self.buf.get(..line_end).unwrap_or_default();
            if line.iter().all(u8::is_ascii_whitespace) {
                self.consume(line_end + 1);
                continue;
            }
            let parsed = serde_json::from_slice::<T>(line);
            // 无论解析成功与否都先消费这一行：单行损坏只跳这一行，不毒化后续帧。
            self.consume(line_end + 1);
            return parsed.map(Some).map_err(FrameError::Json);
        }
    }

    /// 找下一个 `\n` 的绝对位置。
    ///
    /// 上限检查必须**同时**覆盖两条路径：已经见到分隔符的完整超长帧，以及尚未见到分隔符的
    /// 累积字节。只查后者会让"一次 read 就带来整条超长帧"绕过上限。
    fn find_newline(&mut self) -> Result<Option<usize>, FrameError> {
        let tail = self.buf.get(self.scan_from..).unwrap_or_default();
        if let Some(offset) = memchr::memchr(b'\n', tail) {
            let line_end = self.scan_from + offset;
            if line_end > self.max_frame {
                return Err(FrameError::TooLarge {
                    len: line_end,
                    limit: self.max_frame,
                });
            }
            return Ok(Some(line_end));
        }
        // 整段尾部都没有分隔符：推进游标，下次只扫新到的字节。
        self.scan_from = self.buf.len();
        if self.buf.len() > self.max_frame {
            return Err(FrameError::TooLarge {
                len: self.buf.len(),
                limit: self.max_frame,
            });
        }
        Ok(None)
    }

    /// 丢弃前 `upto` 个字节，重置游标，并在必要时回缩容量。
    fn consume(&mut self, upto: usize) {
        self.buf.drain(..upto.min(self.buf.len()));
        self.scan_from = 0;
        if self.buf.capacity() > SHRINK_CAPACITY_THRESHOLD
            && self.buf.len() < SHRINK_RETAIN_CAPACITY
        {
            self.buf.shrink_to(SHRINK_RETAIN_CAPACITY);
        }
    }
}

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};

    use super::{
        FrameDecoder, FrameError, SHRINK_CAPACITY_THRESHOLD, SHRINK_RETAIN_CAPACITY, encode,
    };

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    struct Msg {
        text: String,
    }

    fn msg(text: &str) -> Msg {
        Msg {
            text: text.to_owned(),
        }
    }

    #[test]
    fn encode_decode_round_trip() -> Result<(), FrameError> {
        let mut wire = Vec::new();
        encode(&msg("one"), &mut wire)?;
        encode(&msg("two"), &mut wire)?;

        let mut decoder = FrameDecoder::new();
        decoder.push(&wire);
        assert_eq!(decoder.decode::<Msg>()?, Some(msg("one")));
        assert_eq!(decoder.decode::<Msg>()?, Some(msg("two")));
        assert_eq!(decoder.decode::<Msg>()?, None);
        Ok(())
    }

    #[test]
    fn frame_split_across_chunks_is_reassembled() -> Result<(), FrameError> {
        let mut wire = Vec::new();
        encode(&msg("split me"), &mut wire)?;

        let mut decoder = FrameDecoder::new();
        // 逐字节喂入：模拟 select! 里被反复取消的读循环。
        for byte in &wire {
            assert_eq!(
                decoder.decode::<Msg>()?,
                None,
                "帧未完整时不得产出，缓冲区必须跨调用存活"
            );
            decoder.push(std::slice::from_ref(byte));
        }
        assert_eq!(decoder.decode::<Msg>()?, Some(msg("split me")));
        Ok(())
    }

    #[test]
    fn blank_and_whitespace_lines_are_skipped() -> Result<(), FrameError> {
        let mut decoder = FrameDecoder::new();
        decoder.push(b"\n   \n\t\n{\"text\":\"after heartbeats\"}\n");
        assert_eq!(decoder.decode::<Msg>()?, Some(msg("after heartbeats")));
        assert_eq!(decoder.decode::<Msg>()?, None);
        Ok(())
    }

    #[test]
    fn malformed_line_skips_only_that_line() {
        let mut decoder = FrameDecoder::new();
        decoder.push(b"{not json}\n{\"text\":\"survivor\"}\n");

        let err = decoder
            .decode::<Msg>()
            .expect_err("坏行必须报错，不能被静默吞掉");
        assert!(matches!(err, FrameError::Json(_)));

        // 关键契约：坏行之后的帧仍然可解。
        assert_eq!(
            decoder.decode::<Msg>().expect("坏行不得毒化后续帧"),
            Some(msg("survivor"))
        );
    }

    #[test]
    fn wrong_shape_is_a_json_error_not_a_hang() {
        let mut decoder = FrameDecoder::new();
        decoder.push(b"{\"unexpected\":1}\n{\"text\":\"ok\"}\n");
        assert!(matches!(decoder.decode::<Msg>(), Err(FrameError::Json(_))));
        assert_eq!(
            decoder.decode::<Msg>().expect("结构不符也只跳这一行"),
            Some(msg("ok"))
        );
    }

    #[test]
    fn oversized_frame_is_rejected() {
        let mut decoder = FrameDecoder::with_max_frame(16);
        decoder.push(b"{\"text\":\"far too long to fit in sixteen bytes\"}");
        let err = decoder.decode::<Msg>().expect_err("超限必须报错");
        match err {
            FrameError::TooLarge { len, limit } => {
                assert_eq!(limit, 16);
                assert!(len > 16);
            }
            FrameError::Json(e) => panic!("应报 TooLarge，实得 JSON 错误：{e}"),
        }
    }

    #[test]
    fn limit_counts_accumulated_bytes_not_single_chunk() {
        let mut decoder = FrameDecoder::with_max_frame(8);
        decoder.push(b"1234");
        assert!(decoder.decode::<Msg>().is_ok(), "还没超限，不该报错");
        decoder.push(b"56789");
        assert!(
            matches!(decoder.decode::<Msg>(), Err(FrameError::TooLarge { .. })),
            "上限针对累积字节，不是单次 chunk"
        );
    }

    #[test]
    fn oversized_frame_arriving_complete_is_still_rejected() {
        // 回归：分隔符已在缓冲区里时，也必须查上限——否则一次 read 带来整条超长帧就绕过了限制。
        let mut decoder = FrameDecoder::with_max_frame(8);
        decoder.push(b"123456789\n");
        match decoder.decode::<Msg>().expect_err("完整的超长帧必须报错") {
            FrameError::TooLarge { len, limit } => {
                assert_eq!((len, limit), (9, 8));
            }
            FrameError::Json(e) => panic!("应报 TooLarge，实得 JSON 错误：{e}"),
        }
    }

    #[test]
    fn frame_exactly_at_the_limit_is_accepted() -> Result<(), FrameError> {
        let payload = br#"{"text":"ok"}"#;
        let mut decoder = FrameDecoder::with_max_frame(payload.len());
        decoder.push(payload);
        decoder.push(b"\n");
        assert_eq!(decoder.decode::<Msg>()?, Some(msg("ok")));
        Ok(())
    }

    #[test]
    fn large_frame_does_not_pin_capacity_for_the_connection() -> Result<(), FrameError> {
        let big = msg(&"x".repeat(SHRINK_CAPACITY_THRESHOLD * 2));
        let mut wire = Vec::new();
        encode(&big, &mut wire)?;
        encode(&msg("small"), &mut wire)?;

        let mut decoder = FrameDecoder::new();
        decoder.push(&wire);
        assert_eq!(decoder.decode::<Msg>()?, Some(big));
        assert_eq!(decoder.decode::<Msg>()?, Some(msg("small")));

        assert!(
            decoder.capacity() <= SHRINK_RETAIN_CAPACITY,
            "大帧过后容量必须回缩，实测 {}",
            decoder.capacity()
        );
        Ok(())
    }

    #[test]
    fn buffered_reports_pending_bytes() {
        let mut decoder = FrameDecoder::new();
        assert_eq!(decoder.buffered(), 0);
        decoder.push(b"{\"text\":\"partial\"}");
        assert_eq!(decoder.buffered(), 18);
        decoder.push(b"\n");
        assert_eq!(
            decoder.decode::<Msg>().expect("完整帧应可解"),
            Some(msg("partial"))
        );
        assert_eq!(decoder.buffered(), 0, "成帧后缓冲区必须清空");
    }
}
