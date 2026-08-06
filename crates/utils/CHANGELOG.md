# Changelog

格式与归属标注规则见 `rule://zcode-workflow`。

## [Unreleased]

### Added

- `env::declare_worker_host_entry` / `env::worker_host_entry`：worker 子进程重入 CLI 二进制的
  路径解析，非 host 进程返回 `WorkerHostError::NotDeclared` 以便回退到进程内实现。
