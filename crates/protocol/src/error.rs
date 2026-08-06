//! 结构化协议错误帧。
//!
//! # 为什么未知 `Request` 不能静默跳过
//!
//! 未知 `Event`（运行时推给客户端）可以丢——它是通知，没人在等回音。
//! 未知 `Request` **绝不可以**：请求方在等 `reply_to` 指向自己 `id` 的那一帧，跳过它
//! 等于让调用方永久挂着。
//!
//! 这个失败形状在 opencode 上有实证：权限询问的 pending 只在内存，客户端重连后不重拉，
//! 于是 SSE 在 `permission.asked` 之后、`reply` 之前断开时，**服务端工具永久挂着而 UI 无显示**
//! （`packages/opencode/src/permission/index.ts:98-107`，重连路径
//! `packages/tui/src/context/sync.tsx:451-532` 里没有任何 `permission.list` 调用）。
//!
//! 所以规则是：**每一条带 `id` 的请求都必须收到恰好一帧回应**，认不出来也要回
//! [`ErrorCode::UnsupportedRequest`]。

use serde::{Deserialize, Serialize};

/// 协议层错误码。
///
/// 只覆盖**协议本身**的失败；agent 领域的失败（工具执行失败、模型报错）走各自的
/// `Event` 变体，不占这里。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// 主版本协商失败，连接即将关闭。
    VersionMismatch,
    /// 本端不认识这条请求——通常是对端比本端新。请求方应据此降级，而不是重试。
    UnsupportedRequest,
    /// 帧不是合法 JSON，或不符合本协议的结构。
    MalformedFrame,
    /// 帧长度超过上限，连接即将关闭。
    FrameTooLarge,
    /// 握手尚未完成就发了业务请求。
    HandshakeRequired,
    /// 对端比本端新，发来了本端不认识的错误码。**必须**静默接受，不得再回一个错误帧
    /// ——否则两端会互相回错误直到断连。
    #[serde(other)]
    Unknown,
}

/// 一帧结构化协议错误。
///
/// 作为对某条请求的回应时，外层信封的 `reply_to` **必须**指向那条请求的 `id`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("{code:?}: {message}")]
pub struct ProtocolError {
    /// 机器可判的错误码。分支只许基于它，不许基于 `message`。
    pub code: ErrorCode,
    /// 人可读的说明，仅用于日志与提示。
    pub message: String,
}

impl ProtocolError {
    /// 构造一帧协议错误。
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// 认不出请求时的标准回应。
    #[must_use]
    pub fn unsupported_request(kind: &str) -> Self {
        Self::new(
            ErrorCode::UnsupportedRequest,
            format!("本端不支持请求 `{kind}`"),
        )
    }
}

impl From<crate::VersionMismatch> for ProtocolError {
    fn from(err: crate::VersionMismatch) -> Self {
        Self::new(ErrorCode::VersionMismatch, err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::{ErrorCode, ProtocolError};
    use crate::{PROTOCOL_VERSION, Version, VersionMismatch};

    #[test]
    fn code_round_trips_as_snake_case() -> Result<(), serde_json::Error> {
        assert_eq!(
            serde_json::to_string(&ErrorCode::UnsupportedRequest)?,
            r#""unsupported_request""#
        );
        assert_eq!(
            serde_json::from_str::<ErrorCode>(r#""frame_too_large""#)?,
            ErrorCode::FrameTooLarge
        );
        Ok(())
    }

    #[test]
    fn unknown_code_from_newer_peer_is_absorbed() -> Result<(), serde_json::Error> {
        // 关键契约：认不出错误码时不得再回一个错误帧，否则两端互相回错误直到断连。
        let err: ProtocolError =
            serde_json::from_slice(br#"{"code":"quota_exhausted","message":"nope"}"#)?;
        assert_eq!(err.code, ErrorCode::Unknown);
        assert_eq!(err.message, "nope");
        Ok(())
    }

    #[test]
    fn version_mismatch_converts_to_error_frame() {
        let mismatch = VersionMismatch {
            local: PROTOCOL_VERSION,
            remote: Version { major: 9, minor: 1 },
        };
        let err = ProtocolError::from(mismatch);
        assert_eq!(err.code, ErrorCode::VersionMismatch);
        assert!(err.message.contains("9.1"), "说明里要带上对端版本便于定位");
    }

    #[test]
    fn unsupported_request_names_the_request() {
        let err = ProtocolError::unsupported_request("session.fork");
        assert_eq!(err.code, ErrorCode::UnsupportedRequest);
        assert!(err.message.contains("session.fork"));
    }
}
