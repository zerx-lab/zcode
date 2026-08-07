//! TUI 应用层的错误类型。

use thiserror::Error;

/// `app` 模块的顶层错误。`Display` 面向用户，中文句子，不带 `{:?}`。
#[derive(Debug, Error)]
pub(crate) enum AppError {
    /// 终端能力不足：非交互输出（重定向/非 TTY）不该走 TUI。
    #[error("当前终端不支持交互式界面（不是 TTY，或终端不支持 ANSI）")]
    NotInteractive,
    /// 终端 IO 失败：进入/退出 raw mode、读写终端字节等。
    #[error("终端 IO 失败：{0}")]
    Terminal(#[source] std::io::Error),
    /// 与运行时的请求/响应往来失败（握手、传输层错误）。
    #[error("与运行时通信失败：{0}")]
    Connection(#[source] crate::host::connect::ConnectError),
    /// 订阅目标会话时，运行时用非期望的 `Reply` 变体作答。
    #[error("订阅会话失败：运行时返回了意料之外的回应")]
    UnexpectedReply,
    /// 会话被另一个客户端占用，用户拒绝接管。
    #[error("会话正被另一个客户端占用（{holder}），已放弃接管")]
    SessionBusy {
        /// 占用者的客户端实例 id。
        holder: String,
    },
    /// `ClientSession::take_events` 返回 `None`：事件接收端已经被取走过一次。
    /// 正常运行不会触发——本函数是每条 `ClientSession` 唯一的事件消费者。
    #[error("内部错误：事件订阅通道已被取走")]
    EventsUnavailable,
    /// 内置主题加载失败。只可能在内置 JSON 与调色板 struct 定义漂移时出现——
    /// `zcode-tui` 侧有单测挡着，真跑出来说明二进制构建有问题，不该静默降级。
    #[error("加载界面主题失败：{0}")]
    Theme(#[source] zcode_tui::theme::ThemeError),
}

impl From<std::io::Error> for AppError {
    fn from(err: std::io::Error) -> Self {
        Self::Terminal(err)
    }
}

impl From<crate::host::connect::ConnectError> for AppError {
    fn from(err: crate::host::connect::ConnectError) -> Self {
        Self::Connection(err)
    }
}
