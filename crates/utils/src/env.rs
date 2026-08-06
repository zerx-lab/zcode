//! 进程环境：worker 子进程重入 CLI 入口所需的路径解析。
//!
//! Worker **必须重入 CLI 二进制**，绝不编译独立的 worker 目标——历史上每个 worker 一个
//! `[[bin]]`，导致 `cargo install` 与打包脚本要同步两套目标列表，漏一个就安装后静默失败。
//! 完整契约见 `rule://zcode-architecture`。

use std::io;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use thiserror::Error;

/// 解析 worker host 入口时的失败原因。
#[derive(Debug, Error)]
pub enum WorkerHostError {
    /// 当前进程没有把自己声明为 worker host：`cargo test`、库嵌入、独立运行的 `zcode-stats`
    /// 都属于这一类，调用方必须回退到进程内实现。
    #[error("当前进程未声明为 zcode worker host")]
    NotDeclared,
    /// 无法解析当前可执行文件路径（进程已被删除或权限不足）。
    #[error("无法解析当前可执行文件路径")]
    CurrentExe(#[source] io::Error),
}

static WORKER_HOST_ENTRY: OnceLock<PathBuf> = OnceLock::new();

/// 把当前进程声明为 worker host，并返回 worker 应当重入的二进制路径。
///
/// 只由 CLI 入口（`crates/coding-agent/src/main.rs`）在启动时、分发 worker selector 之前调用。
/// 幂等：重复调用返回首次声明的路径，不会重新解析 `current_exe`。
pub fn declare_worker_host_entry() -> Result<&'static Path, WorkerHostError> {
    if let Some(declared) = WORKER_HOST_ENTRY.get() {
        return Ok(declared.as_path());
    }
    let exe = std::env::current_exe().map_err(WorkerHostError::CurrentExe)?;
    Ok(WORKER_HOST_ENTRY.get_or_init(|| exe).as_path())
}

/// 返回 worker 子进程应当重入的二进制路径。
///
/// 不在 CLI host 中时返回 [`WorkerHostError::NotDeclared`]，调用方必须回退到进程内实现
/// （`tokio::spawn` + 同一份 worker 逻辑），而不是另找一个二进制。
pub fn worker_host_entry() -> Result<&'static Path, WorkerHostError> {
    WORKER_HOST_ENTRY
        .get()
        .map(PathBuf::as_path)
        .ok_or(WorkerHostError::NotDeclared)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 契约：声明后拿到的路径就是当前二进制本身——worker 靠它重入同一个可执行文件。
    #[test]
    fn declared_entry_is_the_running_executable() {
        let declared = declare_worker_host_entry().unwrap();
        assert_eq!(declared, std::env::current_exe().unwrap().as_path());
        assert_eq!(worker_host_entry().unwrap(), declared);
    }

    /// 契约：重复声明幂等。CLI 入口与嵌入场景可能各调一次，不能因此得到两个路径。
    #[test]
    fn declaration_is_idempotent() {
        let first = declare_worker_host_entry().unwrap();
        let second = declare_worker_host_entry().unwrap();
        assert_eq!(first, second);
    }
}
