//! 协议版本、握手与协商。
//!
//! # 握手是三帧，且**双向**认证
//!
//! ```text
//! client → ClientHello { version, agent, nonce_c }
//! server → ServerHello { version, agent, nonce_s, proof = f(secret, "server", nonce_c) }
//! client → ClientAuth  { proof = f(secret, "client", nonce_s) }   // 校验 server proof 通过才发
//! ```
//!
//! **客户端绝不先出示凭据。** 明文 bearer 在本机 IPC 上有真实攻击面：Windows named pipe
//! **没有文件权限模型**，任何本机进程都能抢先用同名 pipe 占坑（oh-my-pi
//! `packages/coding-agent/src/launch/paths.ts:8-11` + `client.ts:90` 记录了"token 是唯一
//! 防线"这一前提）。若客户端首帧就把注册文件里的密钥发出去，占坑者一次连接就把密钥收走，
//! 随后既能冒充 daemon 也能去连真 daemon。让服务端先证明持有密钥，占坑者在第二帧就被识破，
//! 密钥一次都不上线。
//!
//! nonce 每次连接现生成，上一次握手的 proof 无法重放；域分隔串（`"server"` / `"client"`）
//! 防的是把服务端应答原样反射回去冒充客户端应答。
//!
//! 密钥来自 daemon 注册文件（owner-only），HMAC 计算与常数时间比对都在 `zcode-utils` 的
//! `daemon` 模块——**协议层不碰密钥**，只搬运不透明字符串。
//!
//! 版本协商本身抄源：jcode `crates/jcode-harness-api/src/lib.rs:11-15,31-34`。

use std::fmt;

use serde::{Deserialize, Serialize};

/// 本端实现的协议版本。
///
/// 变更规则：
///
/// - **加性变更**（新增 `Event` 变体、新增可选字段）→ bump `minor`。旧对端靠
///   `#[serde(other)]` 兜底变体与 `#[serde(default)]` 吸收，见 crate 文档。
/// - **破坏性变更**（删字段、改语义、改必填性、新增 `Request` 变体的**语义前提**）→ bump `major`。
///   握手直接拒绝。
pub const PROTOCOL_VERSION: Version = Version { major: 1, minor: 0 };

/// 协议版本号。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Version {
    /// 主版本。不相等即不兼容。
    pub major: u16,
    /// 次版本。只表示"多了哪些加性能力"。
    pub minor: u16,
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

/// 主版本不一致，无法通信。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("协议主版本不兼容：本端 {local}，对端 {remote}")]
pub struct VersionMismatch {
    /// 本端版本。
    pub local: Version,
    /// 对端声明的版本。
    pub remote: Version,
}

impl Version {
    /// 与对端协商生效版本。
    ///
    /// 主版本必须相等；生效 `minor` 取两端较小者，双方据此只使用共同拥有的加性能力。
    ///
    /// 失败时调用方**必须显式回一个 [`crate::ProtocolError`] 帧再断开**，不要静默降级——
    /// 那正是 opencode `packages/cli/src/tui.ts:36-45` 的 `gracefulFetch` 垫片踩过的坑：
    /// 把 404 伪造成空对象来兼容版本。
    pub fn negotiate(self, remote: Self) -> Result<Self, VersionMismatch> {
        if self.major != remote.major {
            return Err(VersionMismatch {
                local: self,
                remote,
            });
        }
        Ok(Self {
            major: self.major,
            minor: self.minor.min(remote.minor),
        })
    }
}

/// 连接建立后的第一帧，双向各发一次。
///
/// 握手是协议本身的一部分，与 agent 领域无关，因此归本 crate 所有。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    /// 发送方实现的协议版本。
    pub version: Version,
    /// 发送方的实现标识，仅用于日志与问题定位，**不得**参与任何行为分支。
    ///
    /// 三仓的一致教训：按对端品牌选行为的探测路线已被证伪
    /// （oh-my-pi 删掉了 `eagerEraseScrollbackRisk` / `PI_TUI_ED3_SAFE` 那一整套）。
    pub agent: String,
}

impl Hello {
    /// 构造本端的握手帧。
    pub fn local(agent: impl Into<String>) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            agent: agent.into(),
        }
    }
}

/// 一次性随机数，base64url 无填充编码。
///
/// 每条连接、每个方向各生成一个，**绝不复用**：复用会让上一次握手的证明可以被重放。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Nonce(pub String);

/// 对一个 [`Nonce`] 的持有证明，base64url 无填充编码。
///
/// 计算方式是 `HMAC(注册密钥, 域分隔串 ‖ 对端 nonce)`，实现在 `zcode_utils::daemon`
/// （本 crate 不依赖它，因此这里不做 intra-doc 链接）——**协议层不碰密钥**，
/// 只搬运不透明字符串。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Proof(pub String);

/// 客户端首帧。
///
/// **不携带任何凭据。** 客户端在这一步只出示 nonce，等服务端先证明自己持有注册密钥。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientHello {
    /// 版本与实现标识。
    #[serde(flatten)]
    pub hello: Hello,
    /// 客户端出的挑战。
    pub nonce: Nonce,
}

/// 服务端首帧：同时是对客户端挑战的应答。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerHello {
    /// 版本与实现标识。
    #[serde(flatten)]
    pub hello: Hello,
    /// 服务端出的挑战。
    pub nonce: Nonce,
    /// 对 [`ClientHello::nonce`] 的应答。客户端校验不过必须立刻断开。
    pub proof: Proof,
}

/// 客户端第二帧：对服务端挑战的应答。握手到此结束。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientAuth {
    /// 对 [`ServerHello::nonce`] 的应答。
    pub proof: Proof,
}

#[cfg(test)]
mod tests {
    use super::{Hello, PROTOCOL_VERSION, Version};

    const fn v(major: u16, minor: u16) -> Version {
        Version { major, minor }
    }

    #[test]
    fn same_major_negotiates_to_lower_minor() {
        assert_eq!(v(1, 7).negotiate(v(1, 3)), Ok(v(1, 3)));
        assert_eq!(v(1, 3).negotiate(v(1, 7)), Ok(v(1, 3)));
        assert_eq!(v(2, 0).negotiate(v(2, 0)), Ok(v(2, 0)));
    }

    #[test]
    fn different_major_is_rejected() {
        let err = v(1, 9).negotiate(v(2, 0)).expect_err("主版本不同必须拒绝");
        assert_eq!(err.local, v(1, 9));
        assert_eq!(err.remote, v(2, 0));
    }

    #[test]
    fn negotiation_is_symmetric() {
        for (a, b) in [
            (v(1, 0), v(1, 5)),
            (v(1, 5), v(1, 0)),
            (v(1, 2), v(2, 2)),
            (v(3, 0), v(1, 0)),
        ] {
            assert_eq!(
                a.negotiate(b).map_err(|e| (e.remote, e.local)),
                b.negotiate(a).map_err(|e| (e.local, e.remote)),
                "协商结果必须与谁先发起无关"
            );
        }
    }

    #[test]
    fn version_renders_as_major_dot_minor() {
        assert_eq!(PROTOCOL_VERSION.to_string(), "1.0");
        assert_eq!(v(12, 345).to_string(), "12.345");
    }

    #[test]
    fn hello_round_trips_and_tolerates_unknown_fields() -> Result<(), serde_json::Error> {
        let json = serde_json::to_string(&Hello::local("zcode-tui"))?;
        assert_eq!(
            serde_json::from_str::<Hello>(&json)?,
            Hello::local("zcode-tui")
        );

        // 更新的对端会带上本端还不认识的字段，握手不能因此失败。
        let from_newer =
            br#"{"version":{"major":1,"minor":4},"agent":"zcode-mobile","locale":"zh"}"#;
        let hello: Hello = serde_json::from_slice(from_newer)?;
        assert_eq!(hello.version, v(1, 4));
        Ok(())
    }
}
