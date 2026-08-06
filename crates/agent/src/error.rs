//! `zcode-agent` 的错误类型。

use std::path::PathBuf;

use crate::id::EntryId;

/// 会话存储的错误。
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// 底层文件 I/O 失败。
    #[error("会话文件 {path} 的 I/O 失败")]
    Io {
        /// 出错的文件路径。
        path: PathBuf,
        /// 底层错误。
        #[source]
        source: std::io::Error,
    },
    /// 序列化一条条目失败。
    #[error("条目序列化失败")]
    Encode(#[source] serde_json::Error),
    /// 会话文件没有根条目（`parent_id == None`）。
    #[error("会话文件 {path} 没有根条目")]
    MissingRoot {
        /// 出错的文件路径。
        path: PathBuf,
    },
    /// 条目引用了一个不存在的父条目。
    #[error("条目 {child} 引用了不存在的父条目 {parent}")]
    DanglingParent {
        /// 子条目 id。
        child: EntryId,
        /// 缺失的父条目 id。
        parent: EntryId,
    },
}

/// 工具执行的错误。
///
/// 注意这里**只覆盖真正的执行故障**。参数不合 schema、工具名不存在、审批被拒这三类
/// 都不是错误路径：它们被翻译成 `is_error` 的工具结果喂回模型，turn 继续跑。
/// 抄源 opencode `packages/opencode/src/tool/tool.ts:25-33`——把校验失败当异常抛会
/// 让一次可自愈的参数笔误变成整个 turn 中断。
#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    /// 工具自身报告的失败，文本会原样喂回模型。
    #[error("{0}")]
    Failed(String),
    /// 执行被取消。
    #[error("工具执行被取消")]
    Cancelled,
    /// 执行超时。
    #[error("工具执行超过 {seconds} 秒上限")]
    Timeout {
        /// 触发的超时秒数。
        seconds: u64,
    },
}

/// Agent 运行时的错误。
#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    /// 提供商请求失败。
    #[error(transparent)]
    Ai(#[from] zcode_ai::AiError),
    /// 会话存储失败。
    #[error(transparent)]
    Store(#[from] StoreError),
    /// 上下文已超限且压缩也救不回来。
    #[error("上下文超限且已重试 {attempts} 次压缩")]
    ContextExhausted {
        /// 已尝试的压缩次数。
        attempts: u32,
    },
    /// 本次 turn 被取消。
    #[error("turn 被取消")]
    Cancelled,
}
