调研主题：agent 运行时与 UI 的进程解耦
仓库：C:/Users/zero/Desktop/code/github/jcode
日期：2026-08-06

# jcode（Rust + ratatui）— agent 运行时与 TUI 的解耦方式

**结论先行：jcode 的 UI↔运行时边界已经是完全协议化的进程间边界，不是"可以改成进程间"，而是"本来就是进程间"。** 默认 `jcode` 命令启动的 TUI 是一个**纯客户端**：它连 Unix socket（Windows 上是 named pipe）、收 NDJSON `ServerEvent`、发 `Request`，进程内不持有 provider、不持有 tool registry、不跑 agent 循环。真正的 agent 运行时活在一个独立的常驻 daemon（`jcode serve`）里。

但同时有两条重要的破口：（a）`jcode run` 头less 路径**绕过 daemon 直接进程内跑 Agent**；（b）crate 依赖方向仍然是 `jcode-tui → jcode-app-core`（TUI crate `pub use jcode_app_core::*` 整体转出运行时），所以**协议化的是运行时边界，不是编译边界**。

---

## 1. 是否有 server/daemon 层：有，且是唯一的交互路径

进程模型 = **单 server 多 client**，client 与 server 是同一个 `jcode` 二进制的不同子命令。

| 事实 | 坐标 |
| --- | --- |
| 架构自述："one server process manages all sessions and state; TUI clients connect over a Unix socket and can reconnect transparently" | `docs/SERVER_ARCHITECTURE.md:9-12` |
| 启动决策：先探活，活着就直接连；没有就 spawn `jcode serve` 再连 | `src/cli/dispatch.rs:890-947` |
| 探活 = ping + 活监听双判据（ping 弱于 listener，避免重复起 daemon） | `crates/jcode-app-core/src/server_spawn.rs:33-36`、`crates/jcode-app-core/src/server/socket.rs:150-157` |
| spawn daemon：同一个 exe + `serve` 子命令，`stdout=null`、`stderr=piped` | `src/cli/dispatch.rs:1250-1281` |
| daemon 就绪握手：匿名 pipe 传 `JCODE_READY_FD`，accept loop 起来后写 1 字节 `R` | `crates/jcode-app-core/src/server/socket.rs:229-274,436-450`、`crates/jcode-app-core/src/server.rs:1204-1205` |
| 单实例锁：`jcode-daemon.lock` 上 `flock(LOCK_EX|LOCK_NB)`，持有整个生命周期 | `crates/jcode-app-core/src/server/socket.rs:159-193` |
| 空闲退出：无 client 连接 300s 后 `exit(44)` | `crates/jcode-app-core/src/server.rs:636-637,647-648,1807-1808` |
| server 模块规模：49 个子模块 | `crates/jcode-app-core/src/server.rs:1-49` |

**`jcode serve` 的进程是完整二进制**，因此 daemon 里也链接着 ratatui/crossterm —— `jcode-app-core` 自己就直接依赖它们（`crates/jcode-app-core/Cargo.toml:73-75`）。这是切分不彻底的直接证据。

---

## 2. 传输与协议：NDJSON over Unix socket / named pipe，增量 event 流

### 传输层是一个 `cfg` 分叉的类型别名，不是 trait

```rust
// crates/jcode-base/src/transport/unix.rs:1-6
pub use tokio::net::UnixListener as Listener;
pub use tokio::net::UnixStream  as Stream;
pub use tokio::net::unix::OwnedReadHalf  as ReadHalf;
pub use tokio::net::unix::OwnedWriteHalf as WriteHalf;
```

- 分叉入口：`crates/jcode-base/src/transport/mod.rs:1-8`（`#[cfg(unix)] pub use unix::*;` / `#[cfg(windows)] pub use windows::*;`）。
- Windows 侧手写 named pipe 包装成同名 `Listener`/`Stream`/`ReadHalf`/`WriteHalf`：`crates/jcode-base/src/transport/windows.rs:33-116`。pipe 名 = 路径 file_stem + 路径小写归一化后的 SHA256 前 16 hex（`windows.rs:11-30`），保证不同 socket 路径映射到不同 pipe。
- **这是移植性最高的一招**：上层（server accept loop、client connect、gateway）一行 cfg 都不写，全部只认 `crate::transport::Stream`。

### 协议本体

- 定义在独立 crate `jcode-protocol`，`Request`（client→server）与 `ServerEvent`（server→client）两个 `#[serde(tag="type")]` 外部标签枚举：`crates/jcode-protocol/src/wire.rs:36-38`（`Request`）、`:727-729` 起（`ServerEvent`）。
- 帧格式：一行一个 JSON，`\n` 分隔。文件头自述："Uses newline-delimited JSON over Unix socket. Server streams events back to clients during message processing."：`crates/jcode-protocol/src/lib.rs:1-8`。
- 两个 socket：主 socket `jcode.sock` + 调试 socket `jcode-debug.sock`（后者由主路径派生）：`crates/jcode-app-core/src/server/socket.rs:7-24`。
- **事件流形状是增量 delta，不是全量快照**：`text_delta` / `reasoning_delta` / `tool_start` / `tool_input` / `tool_exec` / `tool_done` / `tokens` / `done`（`wire.rs:732-733,743-744,763-776,810-812,1005-1006`）。全量只在两处：`History`（连接引导，`wire.rs:1063-1066`）和 `SwarmPlan` / `SidePanelState` 这类子系统快照（`wire.rs:888-891,1178-1179`）。
- 规模：`Request` 约 68 个变体、`ServerEvent` 约 79 个，合计与官方文档说的 "~147 variants" 吻合（`docs/HARNESS_API_AND_DESKTOP_REWRITE.md:8-10`）。

### 客户端解帧有专门的性能与安全设计（值得抄）

`crates/jcode-tui/src/tui/backend.rs:230-296`：

- `read_buffer` 跨 `next_event()` 调用持久化 —— 因为读循环在 `tokio::select!` 里，future 被取消时不能丢半包（`backend.rs:232-237`）。
- `read_buffer_scan_start` 游标：History 事件可达数十 MB、跨几千次 socket read，每次从 0 找 `\n` 会让分帧变成 O(n²) 并反压 server writer（`backend.rs:238-245`）。
- 单帧硬顶 `256 MiB`，超限断连（`backend.rs:270-272`）。
- 缓冲区回缩阈值 `256 KiB` → 保留 `64 KiB`，避免一条大帧把容量钉死一整个连接生命周期（`backend.rs:273-281`）。

---

## 3. 会话与状态归属：server 是唯一真源；client 有一份"只读快取"

### 归属

- Session 是 **server 拥有的运行时**：会话历史、provider/model 状态、工具执行状态、持久化、后台任务、记忆抽取，全在 server。文档明确列举：`docs/MULTI_SESSION_CLIENT_ARCHITECTURE.md:110-124`。
- Client 只拥有 **surface-local UI 状态**：输入草稿、光标、滚动、选区、焦点、pane 布局：`docs/MULTI_SESSION_CLIENT_ARCHITECTURE.md:139-155`。
- 会话表在 server 内存：`SessionAgents = Arc<RwLock<HashMap<String, Arc<Mutex<Agent>>>>>`：`crates/jcode-app-core/src/server.rs:110-112`。

### 持久化：snapshot `.json` + 增量 `.journal.jsonl`

- 写路径先判断要不要 checkpoint（元数据变了 / 向量缩短了 / 首次），否则只 append 增量条目；journal 超过 `MAX_SESSION_JOURNAL_BYTES` 触发 checkpoint 折叠：`crates/jcode-base/src/session/persistence.rs:320-405`。
- 读路径 **容忍损坏**：逐行 replay，解析失败只跳这一行并尝试"粘连条目"打捞，不再像旧实现那样在第一个坏字节处截断整个 transcript（"my last prompt is missing" 那个 bug）：`crates/jcode-base/src/session/persistence.rs:66-125`。

### 重连 / attach 已有会话

- 重连循环 + 指数退避（1s→30s），会话状态在磁盘上所以能原样续：`docs/SERVER_ARCHITECTURE.md:113-121`。
- attach 是 `Request::Subscribe` 带 `target_session_id` + `client_instance_id` + `client_has_local_history` + `allow_session_takeover`：`crates/jcode-protocol/src/wire.rs:112-132`。
- **接管仲裁规则**（防止两个 client 抢同一个 session）：只有在 `allow_session_takeover && client_has_local_history && !distinct_client_instances` 时才允许抢占活会话；同实例 id 直接放行；无本地历史又不同实例则拒绝：`crates/jcode-app-core/src/server/client_session.rs:1264-1265,1417-1418,1485-1490`。
- 断线原因分类：`RemoteDisconnectReason::{PeerClosed, Io, Protocol}`：`crates/jcode-tui/src/tui/backend.rs:189-194`。
- attach 语义有一整套端到端测试目录，覆盖"带历史重连接管""同 client 接管""忙会话 attach""多客户端 live attach""无本地历史 attach"：`crates/jcode-app-core/src/server/client_session_tests/resume/`（7 个用例文件）。

---

## 4. 冷启动路径：为"秒开"做了明确设计，且第一帧在连接之前

### main → 第一帧之间的阻塞工作（全部有 startup_profile 埋点）

`src/cli/startup.rs:18-125` 顺序：panic hook → `logging::init` → 一堆依赖倒置注册（config reload 回调、provider runtime 注册、safety notifier、memory provider、session-list 缓存失效器、server spawner）→ `raise_nofile_limit` → `harden_user_config_permissions` → `perf::init_background` → telemetry → `Args::parse`。

耗时的东西都被推到后台线程：
- 老日志清理、memory-log 清理、session `.bak` 清理：`src/cli/startup.rs:26-38`
- 更新检查：`src/cli/startup.rs:263-266`
- launch-hotkey 烘焙（扫 session 历史，几百 ms）明确注明"keep it off the first-frame critical path"：`src/cli/dispatch.rs:830-845`
- **凭据探测被默认跳过**：只有显式 `JCODE_CLI_BOOTSTRAP_LOGIN` 才做，注释直接写 Windows 上安全软件会让凭据读变得极慢：`src/cli/dispatch.rs:1144-1156`

然后 `run_tui_client`：终端初始化 → 标题 → `App::new_for_remote_with_options` → `run_remote`：`src/cli/tui_launch.rs:91-148`。

### 三个关键的"秒开"设计

**(a) 客户端不初始化 provider、不初始化 tool registry。**
```rust
// crates/jcode-tui/src/tui/app/tui_lifecycle.rs:1266-1268
let provider: Arc<dyn Provider> = Arc::new(InertRuntimeProvider::new(AppRuntimeMode::RemoteClient));
let registry = Registry::empty();
```
`InertRuntimeProvider::complete()` 直接返回 Err，从架构上禁止 TUI 进程调 provider：`crates/jcode-tui/src/tui/app.rs:1617-1663`。`Registry::empty()` 注释："Used by remote-mode clients that don't execute tools locally"：`crates/jcode-app-core/src/tool/mod.rs:141-146`。

**(b) 第一帧在连 socket 之前画。**
`run_remote` 的循环体先 `draw_full`、打 `first_frame` 埋点，再调 `connect_with_retry`：`crates/jcode-tui/src/tui/app/run_shell.rs:679-707`。连接期间还有独立的 redraw tick 保证 UI 不冻：`crates/jcode-tui/src/tui/app/remote/reconnect.rs:392-403`。

**(c) resume 时客户端本地预填 transcript，不等 server 的 History。**
`restore_remote_startup_history` 从磁盘 `load_for_remote_startup` 渲染出 display messages，然后**立刻 strip 掉 transcript 向量 + `shrink_to_fit` + 主动归还 arena 页**：`crates/jcode-tui/src/tui/app/tui_lifecycle.rs:1196-1258`。随后 Subscribe 带 `client_has_local_history=true`，server 据此走轻量元数据路径而不是重发全量：`crates/jcode-tui/src/tui/backend.rs:310-316,342-357`。

**(d) 反雷群设计**：模型目录（OpenRouter 可达 ~800 KB/client）默认**不**在 attach 后拉取，只有 `JCODE_REMOTE_BOOTSTRAP_MODEL_CATALOG` 才拉；平时用持久化缓存：`crates/jcode-tui/src/tui/backend.rs:363-379`。

### server 侧冷启动同样做了顺序设计

`finish_startup_after_bind`：先 spawn accept loop → 再 `signal_ready_fd()` → 再发布注册表元数据 → 再起 gateway → **最后**才做 headless 会话恢复（注释："Startup recovery can be expensive in multi-session reloads. Run it only after the replacement daemon is already accepting reconnects."）：`crates/jcode-app-core/src/server.rs:1191-1217`。

---

## 5. SDK / RPC 对外形态：一共四条入口，两条路径

| 入口 | 传输 | 是否复用 daemon | 坐标 |
| --- | --- | --- | --- |
| TUI（默认） | Unix socket / named pipe，NDJSON 内部协议 | 是 | `crates/jcode-tui/src/tui/backend.rs:313-359` |
| ACP（Zed 等编辑器） | stdin/stdout **JSON-RPC 2.0**，内部转成内部协议打 daemon | 是（必要时自己 spawn daemon） | `src/cli/acp.rs:14,183-215,448-461` |
| WebSocket gateway（iOS/web） | TCP:7643 + WS 升级 + token 鉴权 | 是（**同一个 handle_client**） | `crates/jcode-base/src/gateway.rs:8-13,78-101,211-220` |
| harness API v1（desktop2、第三方） | 独立 socket `jcode-api.sock`，**独立进程 bridge** 翻译 | 是（bridge 再拨 daemon） | `crates/jcode-harness-api-server/src/lib.rs:1-16,86-146` |

### 最值得抄的一条：gateway 用 `Stream::pair()` 复用 handle_client

```
TCP :7643  →  WebSocket 升级  →  UnixStream::pair()  →  handle_client()
```
`crates/jcode-base/src/gateway.rs:8-9,211-220`。WS relay 任务把帧转成换行 JSON 写进 pair 的一端，另一端作为普通 `Stream` 交给 `handle_client`（`crates/jcode-app-core/src/server/runtime.rs:1,343-346`）。**结果是新增一种传输 = 写一个 relay + 造一对 pipe，server 一行不改。** 这就是"协议化边界"的可证明形式。

### ACP：JSON-RPC 语义映射

`initialize` / `session/new` / `session/load` / `session/resume` / `session/prompt` / `session/cancel` / `session/close`，每个 session 持一条独立 daemon 连接 + `active_prompt_id` + `prompt_running`：`src/cli/acp.rs:84-127,196-215`。JSON-RPC 错误码常量齐全：`src/cli/acp.rs:16-21`。

### harness API v1：明确的版本化契约（但尚未落地）

- 每帧带 `v`（协议主版本）、`id`（client 单调递增）、`reply_to`：`crates/jcode-harness-api/src/lib.rs:36-58`。
- `ApiEvent` 带 `#[serde(other)] Unknown` 兜底，规定"clients must skip this silently"：`crates/jcode-harness-api/src/events.rs:113-115`。
- 加性变更 bump minor、破坏性 bump major 并在握手协商：`crates/jcode-harness-api/src/lib.rs:11-15,31-34`。
- 有 schema snapshot 测试防意外破坏：`crates/jcode-harness-api/src/lib.rs:26-30`。
- socket 路径解析集中在 API crate（因为曾经 bridge 解析 `$XDG_RUNTIME_DIR`、desktop 解析 `~/.jcode`，两边永远连不上）：`crates/jcode-harness-api/src/sockets.rs:3-11`。

---

## 6. 取消传播与权限审批

### 取消：三层，且刻意不走 `CancellationToken`

**第 1 层 —— 原语。** `InterruptSignal` = `AtomicBool`（同步读）+ `AtomicU64` epoch + `tokio::sync::Notify`（异步唤醒）：`crates/jcode-agent-runtime/src/lib.rs:30-40`。

三个非平凡细节：
- `notified()` 先 `enable()` 再查 flag —— 因为 `notify_waiters()` 只唤醒已注册的 waiter，丢一次 wakeup 会把取消路径挂到下一个无关事件（issue #428）：`lib.rs:92-106`。
- `reset_if_epoch()`：延时 reset 只在 epoch 未变时生效，并且在 reset 后再查一次、发现有新 fire 就把 flag 恢复 —— 保证"取消永不被静默抹掉"：`lib.rs:76-91`。
- `same_instance()` 用 `Arc::ptr_eq`，供 fan-out 去重：`lib.rs:113-115`。

**第 2 层 —— 进程级注册表。** `turn_cancel_registry` 是 `LazyLock<Mutex<HashMap<String, Vec<(u64, InterruptSignal)>>>>`，每个 turn 开始时 RAII 注册自己的 `graceful_shutdown` 信号：`crates/jcode-app-core/src/turn_cancel_registry.rs:31-71`。存在理由写得非常清楚（`:3-24`）：收到 Esc 的那条连接持有的 `SessionControlHandle` 里的信号，**可能和真正在流式生成的 agent 的信号不是同一个实例**（reload 后重连、server 主动发起的 turn、headless 恢复、agent mutex 忙时构造的 `cancel_only` 兜底句柄）。打错实例的后果是 UI 显示 "Interrupting..." 而模型继续生成好几分钟，每按一次 Esc 只是再叠一个 `Interrupted`（"Interrupted [x66]"）。`Drop` 里必须 `reset()`，否则残留的 flag 会瞬间中止下一个 turn（`:84-110`）。

**第 3 层 —— 请求分发的优先级。** `Request::Cancel` **在拿共享 writer 发 Ack 之前**就分发：Ack 走事件 channel 排队，取消先打信号。注释指明重流式/history replay/客户端背压时，共享 writer 可能忙到让已解码的取消排在出站字节后面：`crates/jcode-app-core/src/server/client_lifecycle.rs:946-988`。`request_cancel()` 同时 fire 自己的信号和注册表里该 session 所有活 turn 的信号：`crates/jcode-app-core/src/server/state.rs:601-628`。

**软中断是另一条路**：`Request::SoftInterrupt`（不取消，在下个安全点注入）+ `Request::CancelSoftInterrupts`：`crates/jcode-protocol/src/wire.rs:59-72`；队列是 `Arc<std::sync::Mutex<Vec<SoftInterruptMessage>>>`，用 std 锁以便不持 agent 锁就能入队：`crates/jcode-agent-runtime/src/lib.rs:20-22`。

### 权限审批：**jcode 没有跨进程的交互式逐工具审批**

必须明确写下来，因为这是 ZCode 决策的关键差异：

1. `jcode-protocol` 里**搜不到任何 permission/approval/safety 相关的 Request 或 ServerEvent**（全 crate grep `(?i)permission|approval|safety` 零命中，`crates/jcode-protocol/src/`）。
2. 危险命令走的是**确定性 server 端 gate**，产出的是给模型看的拒绝文本，不是给用户看的弹窗：`crates/jcode-app-core/src/tool/bash_destructive_gate.rs:13-40`。灾难性目标（`/`、`$HOME`、凭据库、设备节点）直接 Deny；中危 `Confirm` 转成"反思提示"，模型必须给 `justification` 才能重发（`bash_destructive_gate.rs:9-12`，schema 见 `:75-78`）。
3. `SafetySystem` 的审批队列是**文件落盘 + 离线批处理**，服务的是 ambient 模式，不在交互回路里：`crates/jcode-base/src/safety.rs:145-193`（队列/历史持久化到 `queue_path()`/`history_path()`）。审批 UI 是一个独立的一次性 TUI，通过 `record_permission_via_file` 写文件决策：`crates/jcode-tui-permissions/src/lib.rs:52-56,66-70`。
4. harness API v1 **定义了** `ApiEvent::PermissionRequest`（`crates/jcode-harness-api/src/events.rs:90-96`），但 bridge 明确回错误："permission_response not yet supported by bridge"：`crates/jcode-harness-api-server/src/translate.rs:228-238`。

### 但 jcode 有一个形状完全正确的"跨进程 UI 回环"可以直接照抄：stdin 请求

工具（bash）执行中需要用户输入时：

1. 工具通过 `ctx.stdin_request_tx` 发 `StdinInputRequest { request_id, prompt, is_password, response_tx: oneshot::Sender<String> }`：`crates/jcode-app-core/src/tool/bash.rs:773,817-821`。
2. server 的 forwarder 把 `response_tx` 存进 `Arc<Mutex<HashMap<String, oneshot::Sender<String>>>>`，然后把 `ServerEvent::StdinRequest { request_id, prompt, is_password, tool_call_id }` 推给 client：`crates/jcode-app-core/src/server/client_lifecycle.rs:633-666`。
3. client 回 `Request::StdinResponse { id, request_id, input }`：`crates/jcode-protocol/src/wire.rs:347-356`。
4. server 按 `request_id` 从 map 取出 oneshot 并 complete。

**这就是 permission approval 需要的全部机制**（关联 id + server 端 oneshot 表 + 双向事件）。jcode 把它建好了却没用在权限上。

---

## 7. 常量与阈值

| 值 | 出处 | 成立前提 |
| --- | --- | --- |
| 共享 server 空闲 300s 退出 | `crates/jcode-app-core/src/server.rs:636-637` | **前提未知**，代码只有 "(5 minutes)" 注释，无 bench/issue 溯源 |
| 临时 server 空闲 30min | `crates/jcode-app-core/src/server/lifecycle.rs:11-12` | **前提未知** |
| 空闲退出码 `44` | `crates/jcode-app-core/src/server.rs:647-648` | 需与 supervisor 约定，属契约不属调优 |
| 重连退避 1s→30s | `docs/SERVER_ARCHITECTURE.md:113-119` | **前提未知**（文档陈述，未在代码中定位到常量定义） |
| 单帧上限 `256 MiB` | `crates/jcode-tui/src/tui/backend.rs:270-272` | 前提明确：History 可含图像所以要大，但必须防恶意 peer 无界撑爆 client |
| read buffer 回缩阈值 `256 KiB` / 保留 `64 KiB` | `crates/jcode-tui/src/tui/backend.rs:273-281` | 前提明确：单条多 MB 帧不应钉死连接期容量；保留值"comfortably above typical streaming line sizes"以免抖动。**具体数值本身无 bench** |
| 杂散协议行上限 32 | `crates/jcode-tui/src/tui/backend.rs:268` | **前提未知** |
| detached 请求超时 2s | `crates/jcode-tui/src/tui/backend.rs:267` | **前提未知** |
| reload 优雅关闭 2s | `crates/jcode-app-core/src/server/reload.rs:14` | **前提未知** |
| Windows daemon 就绪等待 120s | `src/cli/dispatch.rs:1291-1299` | 前提明确：issue #503，Windows Server VPS 上 auth preflight + provider init 实测 15–60s |
| Windows 主线程栈 8 MiB | `src/main.rs:76-80` | 前提明确：Windows 默认栈小于 Unix，CLI/provider setup 会 STATUS_STACK_OVERFLOW |
| 未聚焦空闲重绘最小间隔 1000ms | `crates/jcode-tui/src/tui/app/run_shell.rs:665-668` | 前提明确：共享 server 上一堆后台窗口收到 bus 广播就全速重绘，实测把 CPU 打满 |
| gateway 默认端口 7643、WS keepalive 20s | `crates/jcode-base/src/gateway.rs:40-42` | 端口是任选；keepalive **前提未知** |
| 终端 swarm 成员 GC 批 64 | `crates/jcode-app-core/src/server.rs:133` | **前提未知** |

---

## 8. 不要抄的部分

| 债 | 证据 | 为什么不要抄 |
| --- | --- | --- |
| **TUI 进程级全局渲染状态** | `docs/TUI_TEST_FLAKINESS.md:20-38`；`create_test_app` 不持锁就清全局 render state，~810 个调用点 | 测试必须 `--test-threads=1`（2006 个用例）。上游自己给的正解是"改成 thread-local，别加锁"（`:64-72`）。加锁版实测让套件从 12s 变 10min+ 后回滚（`:52-56`） |
| **`jcode run` 绕过 daemon，进程内直接跑 Agent** | `src/cli/commands.rs:2362-2415`（自己 `init_provider` + `Registry::new` + `Agent::new`） | 两套执行路径 = 两套 bug。headless 的 MCP 冷缓存问题得单独打补丁（`commands.rs:2380-2392` + `JCODE_RUN_MCP_WAIT_MS` 5000ms 兜底），这个坑在 daemon 路径根本不存在 |
| **`jcode-app-core` 直接依赖 ratatui/crossterm** | `crates/jcode-app-core/Cargo.toml:73-75` | 号称"non-presentation modules"的 crate 拉进了 TUI 依赖，daemon 进程白扛渲染栈。清晰的切分失败案例 |
| **`pub use jcode_app_core::*` 整体转出** | `crates/jcode-tui/src/lib.rs:23`；`src/lib.rs:22` | 上游自陈动机是"让 `crate::<module>` 路径原样解析"（`crates/jcode-tui/src/lib.rs:20-22`）——即**为了不改 import 而放弃编译边界**。结果 TUI 能直接摸到 `crate::server`、`crate::agent`，协议边界靠自律而非编译器 |
| **harness API bridge 是独立进程、Unix-only、且功能不全** | `crates/jcode-harness-api-server/src/lib.rs:23`（`use tokio::net::{UnixListener, UnixStream}` 无 cfg）；`translate.rs:228-238` | 多一跳进程 + 多一次 JSON 解析/重编码，permission 直接不支持。同时 Windows 上这个 workspace 成员编译不过（对比主路径的 `crate::transport` cfg 分叉） |
| **依赖边界检查只护 `*-types` crate** | `scripts/check_dependency_boundaries.py:3-8,26-51` | 只禁止 types crate 依赖运行时，**不检查 tui→runtime**。所以第 4 条那个洞 CI 是看不见的 |
| **`workspace_client.rs` 用进程级 `static Mutex`** | `docs/plans/CLIENT_CORE_PRESENTATION_SPLIT_PLAN.md`（"Workspace state is process-global"一节） | 上游自陈这是 client-core 抽取和多 surface 客户端的头号阻塞 |
| **Windows 上"不要探两次"的坑** | `crates/jcode-app-core/src/server/socket.rs:72-83` | 第一次非阻塞探测会临时占掉唯一已发布的 pipe 实例，紧接着的 connect 会在 `ERROR_PIPE_BUSY` 重试循环里**永远等下去**。这是平台特例修补，不是设计 |

---

## 9. 给 ZCode 的可移植结论

### 可以直接抄（同语言，语义零落差）

1. **`crate::transport` 的 cfg 类型别名**（`crates/jcode-base/src/transport/mod.rs:1-8` + `unix.rs:1-6` + `windows.rs:11-116`）。ZCode 在 Windows 上开发，这是必需品且已验证。要连同 `stream_pair()` 一起抄（`unix.rs:17-19`），它是第 2 点的前提。
2. **`Stream::pair()` 复用 handle_client 的传输适配模式**（`crates/jcode-base/src/gateway.rs:211-220`）。这是"协议化边界"的可证形式：加 WebSocket / HTTP / TCP 时 server 零改动。
3. **`InterruptSignal`（AtomicBool + epoch + Notify）+ `turn_cancel_registry`**（`crates/jcode-agent-runtime/src/lib.rs:30-115`、`crates/jcode-app-core/src/turn_cancel_registry.rs`）。`CancellationToken` 在这里不够用的具体理由是"同一次取消要打到可能是多个实例的信号上、且延时 reset 不能抹掉新 fire" —— 这两点 `CancellationToken` 都不提供。
4. **cancel 请求优先于 Ack 分发**（`crates/jcode-app-core/src/server/client_lifecycle.rs:946-988`）。任何有共享出站 writer 的设计都会踩这个坑。
5. **持久 read buffer + scan 游标 + 帧上限 + 容量回缩**（`crates/jcode-tui/src/tui/backend.rs:230-296`）。四个约束一起抄，缺一个就是一类 bug。
6. **oneshot-by-request-id 的 UI 回环**（`client_lifecycle.rs:633-666` + `wire.rs:347-356`）。ZCode 的权限审批直接照这个形状做，别照 jcode 的 safety.rs。
7. **`JCODE_READY_FD` 就绪握手 + `flock` 单实例锁 + 陈旧 socket 双重校验回收**（`server/socket.rs:229-274,88-137,159-193`）。注意回收逻辑的安全性论证：只有"无活监听 **且** 能拿到独占锁"才判定陈旧，这个双条件不能省。

### 需要重新决定的（jcode 的取舍不一定适合 ZCode）

| 议题 | jcode 的选择 | ZCode 要考虑的 |
| --- | --- | --- |
| headless 是否复用 daemon | **不复用**，`jcode run` 进程内跑 | 与 `rule://zcode-architecture` 的 worker 重入 CLI 入口契约有交互，需单独裁决 |
| 对外 API 是内部协议还是版本化 facade | 两者并存（内部 147 变体 + v1 facade + 独立 bridge 进程） | jcode 自己都承认内部协议"unversioned, TUI-shaped, coupled to client rendering assumptions"（`docs/HARNESS_API_AND_DESKTOP_REWRITE.md:5-10`）。ZCode 从零起步，**应该一开始就只有一套版本化协议**，不要复制这个二元结构 |
| crate 依赖方向 | `tui → app-core` 整体 re-export | 应该反过来：`protocol` 独立、`tui` 和 `runtime` 都只依赖 `protocol`，且用 CI ratchet 强制（jcode 的 `check_dependency_boundaries.py` 覆盖面要扩到这条边） |
| 权限模型 | 无交互式审批，只有确定性 gate | 参考 opencode 的有序 ruleset，jcode 这块没有可抄的 |

### Rust 具体落点（如果采用 daemon 架构）

- 事件流类型：`ServerEvent` 必须 `Send`；client 侧 `RemoteRead` 被标了 `#[expect(clippy::large_enum_variant)]`（`backend.rs:196-200`），jcode 选择"直接携带完整事件保持传输层简单"。ZCode 如果 event 更大要考虑 `Box`（jcode 自己在 `LineOutcome::Event(Box<ServerEvent>)` 里就 box 了，`backend.rs:203-208`）。
- server 端共享状态形状：`Arc<RwLock<HashMap<String, Arc<Mutex<Agent>>>>>`（`server.rs:110`）。注意 jcode 到处需要 "agent mutex 忙时的无锁兜底路径"（`SessionControlHandle::cancel_only`，`state.rs:565-577`），这是 `Arc<Mutex<Agent>>` 这个形状的直接代价。
- 边界条件清单（移植时最容易整批丢的）：
  1. 取消 future 被 `select!` 取消时不丢半包；
  2. 取消信号 fire 后必须在 turn 结束时 reset，否则毒化下一个 turn；
  3. 延时 reset 必须 epoch-guarded；
  4. Ack 与 cancel 的分发顺序；
  5. 接管仲裁的三元判据（takeover flag + 本地历史 + 实例 id）；
  6. 陈旧 socket 回收的双条件；
  7. Windows 上不能连探两次；
  8. reload exec 前必须 unlink socket、detach stdio（否则 SIGPIPE 杀掉替身 daemon，`server/reload.rs:16-30`）；
  9. journal 单行损坏只跳这一行；
  10. 未聚焦窗口的重绘节流（多 client 共享 bus 时必然踩）。

---

## 10. 悬而未决

1. **重连退避 1s→30s 的常量定义位置未定位。** 只在 `docs/SERVER_ARCHITECTURE.md:113-119` 见到文字描述；在 `crates/jcode-tui/src/tui/app/remote/reconnect.rs` 里我读到的是 `disconnected_redraw_interval(state.reconnect_attempts == 0)`（`:363`），未读到退避常量本体。搜索路径：grep `reconnect|backoff|from_secs` 于 `crates/jcode-tui/src/tui/app/remote/`——结果太多未逐条核实。
2. **`ServerEvent` / `Request` 精确变体数未逐个清点。** 我按 `#[serde(rename = "` 的 grep 命中数估算（Request ~68、ServerEvent ~79，合计约 147），与文档自述的 "~147 variants"（`docs/HARNESS_API_AND_DESKTOP_REWRITE.md:8`）一致，但没有做精确计数。
3. **harness API bridge 在 Windows 上是否真的编译失败，未验证。** 依据是 `crates/jcode-harness-api-server/src/lib.rs:23` 无条件 `use tokio::net::{UnixListener, UnixStream}` 且它是 workspace 成员（根 `Cargo.toml:32`）。按调研纪律不跑 build，所以这条标为**推断**，需要 ZCode 决策前自行确认。
4. **server 端 `handle_client` 主循环全貌未通读**（`crates/jcode-app-core/src/server/client_lifecycle.rs` 共 3181 行，我读了 599–1013、1362–1420、1617–1640、2889 附近）。关于"每个 client 一个 `mpsc::UnboundedSender<ServerEvent>` 出站 forwarder + biased select 优先直连 I/O"的结论来自 `:599-614,681-690`，可靠；但背压策略（unbounded channel 在慢客户端下的内存行为）我没有找到显式处理，**未确认是否存在**。
5. **`docs/plans/CLIENT_CORE_PRESENTATION_SPLIT_PLAN.md` 的行号未精确引用**（文件 878 行，我读了 1–300）。"workspace state is process-global"一节的具体行号未记录。
6. **与 ZCode 既有契约的潜在冲突**：jcode 的 `jcode run` 走进程内 Agent、而 ZCode 的 `rule://zcode-architecture` 规定 worker 走子进程重入 CLI 入口。二者不是同一件事（一个是 headless 单轮，一个是 worker 派生），但如果 ZCode 采纳 daemon 架构，"headless 单轮走 daemon 还是走进程内"会变成一个新的产品走向级决策——按 `rule://reference-first` 应由 Main 摆选项给用户，我在此仅标记冲突面。
