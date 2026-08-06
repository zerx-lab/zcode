# Changelog

格式与归属标注规则见 `rule://zcode-workflow`。

## [Unreleased]

### Added

- `env::declare_worker_host_entry` / `env::worker_host_entry`：worker 子进程重入 CLI 二进制的
  路径解析，非 host 进程返回 `WorkerHostError::NotDeclared` 以便回退到进程内实现。
- `transport`：跨平台本机 IPC。Unix domain socket 与 Windows named pipe 包装成同名
  `Listener` / `Stream` / `ReadHalf` / `WriteHalf`，上层零 `cfg` 分叉。`stream_pair()` 造一对
  进程内已连接的流，用于把进程内客户端接到与跨进程客户端完全相同的连接处理函数上。
