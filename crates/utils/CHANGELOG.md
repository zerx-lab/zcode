# Changelog

格式与归属标注规则见 `rule://zcode-workflow`。

## [Unreleased]
### Breaking Changes

- 项目许可证从 `MIT OR Apache-2.0` 切换为 `AGPL-3.0-only`。

### Added

- `env::declare_worker_host_entry` / `env::worker_host_entry`：worker 子进程重入 CLI 二进制的
  路径解析，非 host 进程返回 `WorkerHostError::NotDeclared` 以便回退到进程内实现。
- `transport`：跨平台本机 IPC。Unix domain socket 与 Windows named pipe 包装成同名
  `Listener` / `Stream` / `ReadHalf` / `WriteHalf`，上层零 `cfg` 分叉。`stream_pair()` 造一对
  进程内已连接的流，用于把进程内客户端接到与跨进程客户端完全相同的连接处理函数上。
- `daemon`：daemon 端点原语四件套。
  - `Registration`：注册文件（id / version / endpoint / pid / secret），临时文件 + `rename`
    原子替换，Unix 上 0600。`remove_if_mine` 只删自己那份——无条件删会把后来者的注册文件
    一起删掉，让活 daemon 变成无人可达的孤儿。`pid` 只作诊断，PID 会被 OS 复用。
  - `SingleInstanceLock`：`std::fs::File::try_lock`（1.89 稳定，无 `unsafe`、无新依赖）。
    锁绑定在已打开的文件对象上，进程无论怎么死内核都会释放。**不删锁文件**：jcode 在守卫
    `Drop` 里删，那会让"A 删掉锁文件时 B 已打开旧 inode"变成两个进程各锁各的 inode，
    互斥静默失效。
  - `probe_live_listener` + `reap_stale_endpoint`：陈旧端点回收的双条件（无活监听 **且**
    持有独占锁），且拿锁后**再探一次**——刚 spawn、还没走到拿锁那行的新 daemon 可能已经
    bind 上了。回收函数取 `&SingleInstanceLock` 而非路径，用类型把"必须持锁"变成无法绕过的
    前提。探活只探一次且探完只走 bind：Windows 上探针会占掉唯一 pipe 实例，紧接着的真
    connect 会掉进 `ERROR_PIPE_BUSY` 重试循环。超时一律判定为"活"（误判为死会删活端点）。
  - `ReadyChannel` / `signal_ready`：一次性就绪握手。父进程用 `transport` 造一个带随机令牌的
    临时端点，子进程在监听可 accept 之后连上来回写令牌。**不用注册文件**（只证明"曾写入"，
    不与本次 spawn 绑定，且 PID 可复用）、**不用 stdout 文本匹配**（opencode 的反例，
    格式一改就断）。等待期间每轮 `try_wait` 子进程，崩溃立即报错而不是等满 120s。
  - `Secret` / `Nonce` / `proof` / `verify_proof`：daemon 握手的 HMAC-SHA256 双向挑战应答，
    带域分隔串防反射。`Secret` 的 `Debug` 手写成 `<redacted>`——进日志一次就是泄露。

### Fixed

- `transport::stream_pair`（Unix 侧）的 `#[expect(clippy::unused_async)]` 改为 `#[allow(...)]`：
  该 lint 会被 `mod.rs` 的 `pub use ... stream_pair` 当成"async fn 被当值使用"而整体静默
  （clippy#13466 同一机制），expect 于是永不满足，在 `cfg(unix)` 上被 `-D warnings` 打成错误。
