//! Server-Sent Events 解码。
//!
//! 按 [WHATWG SSE 规范][spec] 实现字段解析：`event` / `data` / `id` / `retry`，
//! `:` 开头是注释，空行派发事件，多行 `data` 用 `\n` 拼接。三家提供商的流式响应
//! 都走这一条解码路径。
//!
//! [spec]: https://html.spec.whatwg.org/multipage/server-sent-events.html

use std::collections::VecDeque;
use std::pin::Pin;

use bytes::{Bytes, BytesMut};
use futures_core::Stream;
use futures_util::StreamExt as _;

use crate::error::AiError;
use crate::types::ProviderId;

/// 一条解码完成的 SSE 事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    /// `event:` 字段；缺省时为 `None`，由调用方回落到 `data` 里的 `type`。
    pub event: Option<String>,
    /// `data:` 字段拼接后的负载。
    pub data: String,
}

impl SseEvent {
    /// 是否是 OpenAI 系列的流终止哨兵 `data: [DONE]`。
    #[must_use]
    pub fn is_done_sentinel(&self) -> bool {
        self.data == "[DONE]"
    }
}

/// 增量 SSE 解码器：喂字节，吐事件。
#[derive(Debug, Default)]
pub struct SseDecoder {
    buffer: BytesMut,
    event: Option<String>,
    data: String,
    saw_data: bool,
}

impl SseDecoder {
    /// 新建空解码器。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 追加一段字节，把其中完整的事件推入 `out`。
    pub fn push(&mut self, chunk: &[u8], out: &mut VecDeque<SseEvent>) {
        self.buffer.extend_from_slice(chunk);
        while let Some(newline) = self.buffer.iter().position(|byte| *byte == b'\n') {
            let raw = self.buffer.split_to(newline + 1);
            let line = strip_line_ending(&raw);
            match std::str::from_utf8(line) {
                Ok(line) => self.feed_line(line, out),
                Err(err) => {
                    // 非 UTF-8 只可能来自坏掉的上游；丢掉这一行比中断整条流更可用。
                    tracing::warn!(error = %err, "SSE 行不是合法 UTF-8，已丢弃");
                }
            }
        }
    }

    /// 流结束时调用：把未以空行收尾的残留事件补派发出去。
    pub fn finish(&mut self, out: &mut VecDeque<SseEvent>) {
        if !self.buffer.is_empty() {
            let raw = self.buffer.split_to(self.buffer.len());
            if let Ok(line) = std::str::from_utf8(&raw) {
                self.feed_line(line, out);
            }
        }
        self.dispatch(out);
    }

    fn feed_line(&mut self, line: &str, out: &mut VecDeque<SseEvent>) {
        if line.is_empty() {
            self.dispatch(out);
            return;
        }
        if line.starts_with(':') {
            return;
        }
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "event" => self.event = Some(value.to_owned()),
            "data" => {
                if self.saw_data {
                    self.data.push('\n');
                }
                self.data.push_str(value);
                self.saw_data = true;
            }
            // `id` / `retry` 只对断线重连有意义，这里不做自动重连，忽略。
            _ => {}
        }
    }

    fn dispatch(&mut self, out: &mut VecDeque<SseEvent>) {
        // WHATWG 规范：data 缓冲为空时不派发，只清掉 event 名。网关常发
        // `event: ping\n\n` 这种无 data 的保活，派发出去会让下游按空串解 JSON 而失败。
        if !self.saw_data {
            self.event = None;
            self.data.clear();
            return;
        }
        out.push_back(SseEvent {
            event: self.event.take(),
            data: std::mem::take(&mut self.data),
        });
        self.saw_data = false;
    }
}

fn strip_line_ending(raw: &[u8]) -> &[u8] {
    let without_lf = raw.strip_suffix(b"\n").unwrap_or(raw);
    without_lf.strip_suffix(b"\r").unwrap_or(without_lf)
}

struct SseState<S> {
    body: Pin<Box<S>>,
    decoder: SseDecoder,
    queue: VecDeque<SseEvent>,
    finished: bool,
}

/// 把 HTTP 响应体转成 SSE 事件流。
///
/// 传输错误会作为一个 [`AiError::Transport`] 项产出，随后流立即结束。
pub fn decode_stream<S>(
    provider: ProviderId,
    body: S,
) -> impl Stream<Item = Result<SseEvent, AiError>> + Send
where
    S: Stream<Item = Result<Bytes, reqwest::Error>> + Send + 'static,
{
    let state = SseState {
        body: Box::pin(body),
        decoder: SseDecoder::new(),
        queue: VecDeque::new(),
        finished: false,
    };
    futures_util::stream::unfold(state, move |mut state| async move {
        loop {
            if let Some(event) = state.queue.pop_front() {
                return Some((Ok(event), state));
            }
            if state.finished {
                return None;
            }
            match state.body.next().await {
                Some(Ok(chunk)) => state.decoder.push(&chunk, &mut state.queue),
                Some(Err(source)) => {
                    state.finished = true;
                    return Some((Err(AiError::Transport { provider, source }), state));
                }
                None => {
                    state.finished = true;
                    state.decoder.finish(&mut state.queue);
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(chunks: &[&str]) -> Vec<SseEvent> {
        let mut decoder = SseDecoder::new();
        let mut out = VecDeque::new();
        for chunk in chunks {
            decoder.push(chunk.as_bytes(), &mut out);
        }
        decoder.finish(&mut out);
        out.into_iter().collect()
    }

    #[test]
    fn parses_named_event_with_payload() {
        let events = decode(&["event: message_start\ndata: {\"a\":1}\n\n"]);
        assert_eq!(
            events,
            vec![SseEvent {
                event: Some("message_start".to_owned()),
                data: "{\"a\":1}".to_owned(),
            }]
        );
    }

    #[test]
    fn joins_multiline_data_with_newline() {
        let events = decode(&["data: one\ndata: two\n\n"]);
        assert_eq!(events.first().map(|e| e.data.as_str()), Some("one\ntwo"));
    }

    #[test]
    fn reassembles_events_split_across_chunk_boundaries() {
        let events = decode(&["event: resp", "onse.created\nda", "ta: {\"x\":", "2}\n\n"]);
        assert_eq!(
            events,
            vec![SseEvent {
                event: Some("response.created".to_owned()),
                data: "{\"x\":2}".to_owned(),
            }]
        );
    }

    #[test]
    fn handles_crlf_and_skips_comments() {
        let events = decode(&[": keep-alive\r\ndata: hi\r\n\r\n"]);
        assert_eq!(events.len(), 1);
        assert_eq!(events.first().map(|e| e.data.as_str()), Some("hi"));
    }

    #[test]
    fn value_keeps_leading_space_beyond_the_first() {
        let events = decode(&["data:  padded\n\n"]);
        assert_eq!(events.first().map(|e| e.data.as_str()), Some(" padded"));
    }

    #[test]
    fn flushes_trailing_event_without_blank_line() {
        let events = decode(&["data: [DONE]\n"]);
        assert_eq!(events.len(), 1);
        assert!(events.first().is_some_and(SseEvent::is_done_sentinel));
    }

    #[test]
    fn data_less_keepalive_events_are_dropped() {
        // `event: ping\n\n` 没有 data 行；派发出去下游会拿空串去解 JSON。
        assert!(decode(&["event: ping\n\n"]).is_empty());
        // 后续真实事件不受影响，event 名也没被串到下一条上。
        let events = decode(&["event: ping\n\n", "event: message\ndata: {}\n\n"]);
        assert_eq!(
            events,
            vec![SseEvent {
                event: Some("message".to_owned()),
                data: "{}".to_owned()
            }]
        );
    }

    #[test]
    fn emits_nothing_for_pure_comment_stream() {
        assert!(decode(&[": ping\n\n: ping\n\n"]).is_empty());
    }
}
