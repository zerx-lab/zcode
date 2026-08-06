//! 每帧的信封：版本、单调 id、`reply_to`。
//!
//! 抄源：jcode `crates/jcode-harness-api/src/lib.rs:36-58`。

use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

/// 一帧消息的信封。
///
/// `T` 是 payload 类型。它**必须**是本 crate 自己拥有的 wire 类型（`Request` / `Event` /
/// [`crate::Hello`] / [`crate::ProtocolError`]），**绝不**是运行时或 UI crate 的领域类型——
/// 否则协议 crate 就不再是边界，两侧仍要依赖对方。领域类型与 wire 类型之间的互转是
/// host adapter 的职责。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope<T> {
    /// 发送方的协议主版本。握手之后仍然每帧携带：这是排查"混连了别的版本"最省事的手段。
    pub v: u16,
    /// 发送方单调递增的帧序号。同一条连接内唯一。
    pub id: u64,
    /// 本帧回应的请求 `id`。主动推送为 `None`。
    ///
    /// **每一条带 `id` 的请求都必须收到恰好一帧 `reply_to` 指向它的回应**，认不出来也要回
    /// [`crate::ProtocolError`]。理由见 [`crate::error`] 的模块文档。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<u64>,
    /// 载荷。
    pub payload: T,
}

impl<T> Envelope<T> {
    /// 构造一帧主动发送的消息（请求或推送）。
    pub fn new(id: u64, payload: T) -> Self {
        Self {
            v: crate::PROTOCOL_VERSION.major,
            id,
            reply_to: None,
            payload,
        }
    }

    /// 构造一帧对 `request_id` 的回应。
    pub fn reply_to(id: u64, request_id: u64, payload: T) -> Self {
        Self {
            v: crate::PROTOCOL_VERSION.major,
            id,
            reply_to: Some(request_id),
            payload,
        }
    }

    /// 换掉 payload，保留信封字段。用于在 adapter 层做 wire 类型转换。
    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Envelope<U> {
        Envelope {
            v: self.v,
            id: self.id,
            reply_to: self.reply_to,
            payload: f(self.payload),
        }
    }
}

/// 连接内的帧序号发生器。
///
/// 从 1 开始：0 留作"未赋值"的哨兵，便于在日志里认出忘记赋号的帧。
#[derive(Debug, Default)]
pub struct IdGen(AtomicU64);

impl IdGen {
    /// 取下一个序号。
    pub fn next_id(&self) -> u64 {
        self.0.fetch_add(1, Ordering::Relaxed).wrapping_add(1)
    }
}

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};

    use super::{Envelope, IdGen};

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    struct Ping {
        text: String,
    }

    fn ping() -> Ping {
        Ping {
            text: "hi".to_owned(),
        }
    }

    #[test]
    fn push_frame_omits_reply_to() -> Result<(), serde_json::Error> {
        let json = serde_json::to_string(&Envelope::new(7, ping()))?;
        assert!(
            !json.contains("reply_to"),
            "主动推送不该把 null 写上线：{json}"
        );
        assert_eq!(
            serde_json::from_str::<Envelope<Ping>>(&json)?,
            Envelope::new(7, ping())
        );
        Ok(())
    }

    #[test]
    fn reply_frame_carries_request_id() -> Result<(), serde_json::Error> {
        let frame = Envelope::reply_to(8, 7, ping());
        let decoded: Envelope<Ping> = serde_json::from_str(&serde_json::to_string(&frame)?)?;
        assert_eq!(decoded.reply_to, Some(7));
        assert_eq!(decoded.id, 8);
        Ok(())
    }

    #[test]
    fn missing_reply_to_decodes_as_none() -> Result<(), serde_json::Error> {
        let decoded: Envelope<Ping> =
            serde_json::from_slice(br#"{"v":1,"id":3,"payload":{"text":"hi"}}"#)?;
        assert_eq!(decoded.reply_to, None);
        Ok(())
    }

    #[test]
    fn map_preserves_envelope_fields() {
        let mapped = Envelope::reply_to(2, 1, ping()).map(|p| p.text.len());
        assert_eq!(mapped.id, 2);
        assert_eq!(mapped.reply_to, Some(1));
        assert_eq!(mapped.payload, 2);
    }

    #[test]
    fn ids_start_at_one_and_increase() {
        let ids = IdGen::default();
        assert_eq!(ids.next_id(), 1);
        assert_eq!(ids.next_id(), 2);
        assert_eq!(ids.next_id(), 3);
    }
}
