# 实施计划

决策依据见 `README.md`，硬约束见 `rule://zcode-architecture` 的"进程边界"章节。
本文只管**分期、抄源坐标、验证**。

## 已落地

| 交付 | 位置 | 验证 |
|---|---|---|
| 跨平台本机 IPC | `crates/utils/src/transport/`（`mod.rs` / `unix.rs` / `windows.rs`） | Windows 真机 3 用例：`stream_pair` 双向往返、`bind`+`accept`+`connect` 往返、同端点二次 `bind` 必败 |
| 协议版本与握手 | `crates/protocol/src/version.rs` | 5 用例：同 major 取小 minor、异 major 拒绝、协商对称、渲染、`Hello` 容忍未知字段 |
| 帧信封 | `crates/protocol/src/envelope.rs` | 5 用例：推送省略 `reply_to`、回应携带请求 id、缺字段解为 `None`、`map` 保留信封、id 从 1 递增 |
| 协议错误码 | `crates/protocol/src/error.rs` | 4 用例：snake_case 往返、未知错误码被吸收、`VersionMismatch` 转错误帧、unsupported 带请求名 |
| NDJSON 分帧 | `crates/protocol/src/frame.rs` | 9 用例：往返、逐字节跨 chunk 组帧、心跳空行跳过、坏行只跳该行、结构不符不挂起、超限报错、上限针对累积字节、大帧后容量回缩、`buffered` 计数 |

`crate::transport` 的 cfg 类型别名抄源 jcode `crates/jcode-base/src/transport/mod.rs:1-8` +
`unix.rs:1-19` + `windows.rs:11-116`；`stream_pair` 的用途抄源 `gateway.rs:211-220`。
解帧器四件套抄源 jcode `crates/jcode-tui/src/tui/backend.rs:230-296`。

### 落地时相对抄源做的改动

| 改动 | 理由 |
|---|---|
| `stream_pair` 是 `async` | Windows 上 client `open` 之后必须 `await server.connect()` 让服务端进入 connected 状态，否则读写会挂。jcode 用 dummy-waker + `unsafe` 同步 poll 绕开，本仓 `unsafe_code = deny` 且没必要 |
| `Stream::connect` 的重试**有界**（50ms × 100） | jcode `crates/jcode-app-core/src/server/socket.rs:72-83` 记录了无界重试的后果：一次探活占掉唯一 pipe 实例后，connect 永远等下去。因此本模块也**不提供探活**，探活归 daemon 层的注册文件 + 独占锁 |
| Windows `Listener::bind` 用 `first_pipe_instance(true)` | bind 本身就成为单实例互斥。Unix 侧没有这个性质，需要额外 lock 文件——差异写在模块文档里，避免上层误以为两平台等价 |
| 未知 `Request` 必须回 `UnsupportedRequest` | jcode 的兜底规则只写给 event（`crates/jcode-harness-api/src/events.rs:113-115`）。请求侧照抄会让 `reply_to` 的等待方永久挂着 |

## 待落地

### 第 1 期：Request / Event 变体

**前置条件**：`zcode-agent` 的运行时形状（session、turn、tool call、权限询问）落盘。
现在写变体是凭空发明——没有领域类型可映射。

变体归 `zcode-protocol` 所有，host adapter 负责与领域类型互转。形状参考
jcode `crates/jcode-protocol/src/wire.rs`（`Request` ~68 变体 / `ServerEvent` ~79 变体），
但**不要**照搬规模：jcode 自陈其内部协议 "unversioned, TUI-shaped, coupled to client rendering
assumptions"（`docs/HARNESS_API_AND_DESKTOP_REWRITE.md:5-10`），于是又叠了一层 v1 facade。
本仓从零起步，只要一套版本化协议。

必须包含且容易漏的：

- **权限审批**：`Request::PermissionReply` / `Event::PermissionAsked` +
  **`Request::PermissionList`（重连必调）**。opencode 有前两条、漏了第三条，后果是 SSE
  在询问后断开则服务端工具永久挂着而 UI 无显示。
  ruleset 语义抄 opencode `packages/opencode/src/permission/index.ts:28-38,131-165`：
  有序 allow/deny/ask、`findLast`（后写覆盖先写）、`always` 连锁放行同 session 其他 pending、
  `reject` 连坐、每次结算都推 `permission.replied` 让所有客户端同步移除 UI。
- **stdin 回环**：工具执行中要用户输入时的 oneshot-by-request-id
  （jcode `server/client_lifecycle.rs:633-666` + `wire.rs:347-356`）。这与权限审批是同一个机制。
- **`Event::Resync`**：`broadcast` 慢消费者 `RecvError::Lagged` 时**不断流**，
  推 resync 让客户端按游标补拉。这是对 opencode 两代都错的直接修正
  （v1 `Queue.unbounded` 撑爆 server、v2 `dropping(256)` 溢出即打挂整条流）。

### 第 2 期：连接处理与 daemon 生命周期

- `handle_client`：每 client 一个出站 forwarder；**cancel 请求先于 Ack 分发**
  （jcode `server/client_lifecycle.rs:946-988`——共享 writer 忙时取消会排在出站字节后面）。
- 取消三层：`InterruptSignal`（`AtomicBool` + epoch + `Notify`）+ 进程级 turn 注册表
  （jcode `crates/jcode-agent-runtime/src/lib.rs:30-115` + `turn_cancel_registry.rs:3-24`）。
  三个非平凡点：`notified()` 先 `enable()` 再查 flag（丢一次 wakeup 会把取消挂到下一个无关事件）；
  延时 reset 必须 epoch-guarded；`Drop` 里必须 `reset()`，否则残留 flag 瞬间中止下一个 turn。
- 取消级联到后台作业：递归取消所有指向本 session 的 job，循环到无新增，再取消 runner
  （opencode `packages/opencode/src/session/run-state.ts:108-140`）。
- daemon 生命周期：注册文件（原子 temp+rename、0600）+ 健康认证 + 版本比对 + 自杀式互斥
  （opencode `packages/cli/src/services/daemon.ts:40-41,64-78,110-131,159-177`）。
  **先健康认证再对 PID 发信号**，PID 会被复用（注释 `:152-153`）。
  就绪握手用父子 pipe（jcode `JCODE_READY_FD`，`server/socket.rs:229-274`），
  **不要**靠 stdout 文本匹配（opencode `packages/sdk/js/src/v2/server.ts:55-70` 的反例）。
- 陈旧端点回收**双条件**：无活监听 **且** 能拿到独占锁（jcode `server/socket.rs:88-137`）。
  单条件会删掉活着的接任者。
- 接管仲裁三元判据：takeover flag + 客户端本地历史 + client 实例 id
  （jcode `server/client_session.rs:1264-1265,1417-1418,1485-1490`）。
- Windows 主线程栈设 8 MiB（jcode `src/main.rs:76-80`：Windows 默认栈比 Unix 小，
  provider setup 会 `STATUS_STACK_OVERFLOW`）。

### 第 3 期：秒开

与传输形态无关，可与第 2 期并行。四条全部抄 jcode：

1. 客户端进程**不初始化** provider 与 tool registry。用一个从架构上禁止调用的惰性 provider
   （`InertRuntimeProvider::complete()` 直接返 `Err`，`crates/jcode-tui/src/tui/app.rs:1617-1663`）
   与空 registry（`crates/jcode-app-core/src/tool/mod.rs:141-146`）。
2. **第一帧在连接之前画**，连接期间保持独立 redraw tick
   （`app/run_shell.rs:679-707` + `app/remote/reconnect.rs:392-403`）。
3. resume 时本地预填 transcript，渲染完立刻 strip + `shrink_to_fit`；
   `Subscribe` 带 `client_has_local_history=true` 让 server 走轻量元数据路径
   （`app/tui_lifecycle.rs:1196-1258` + `backend.rs:310-316,342-357`）。
4. 反雷群：模型目录默认不在 attach 后拉（OpenRouter 可达 ~800 KB/client，`backend.rs:363-379`）；
   凭据探测默认跳过（Windows 安全软件让凭据读极慢，`src/cli/dispatch.rs:1144-1156`）。
   模型目录改为同步读本地缓存开局、网络刷新推到 session 建好之后
   （oh-my-pi `config/model-registry.ts:862-899,1312-1321` + `main.ts:1519-1521`）。
5. 未聚焦窗口的重绘节流（最小间隔 1000ms）。前提明确：共享 server 上一堆后台窗口收到广播就
   全速重绘，实测把 CPU 打满（jcode `app/run_shell.rs:665-668`）。多 client 共享必然踩。

### 第 4 期：额外传输出口

WebSocket / TCP 出口 = 写一个 relay + `stream_pair()` 造一对流交给同一个 `handle_client`，
**server 一行不改**（jcode `gateway.rs:211-220`）。这是"协议边界真的存在"的可证形式；
若届时发现必须改 `handle_client`，说明边界没做对。

## CI 闸门待补

- **依赖方向 ratchet**：禁止 `zcode-tui` 依赖任何运行时 crate、禁止运行时 crate 依赖
  `ratatui` / `crossterm`。jcode 的 `scripts/check_dependency_boundaries.py:26-51` 只护
  `*-types` crate，看不见 `tui -> runtime` 这条边——覆盖面必须扩到它。
- **协议 schema 快照测试**：防意外破坏 wire 兼容（jcode `crates/jcode-harness-api/src/lib.rs:26-30`）。
  变体落地后才有意义。

## 待真机验证（本机 Windows 已覆盖的除外）

| 项 | Windows | macOS | Linux |
|---|---|---|---|
| `transport` 往返与单实例互斥 | 已验证 | **需真机** | **需真机** |
| 陈旧端点双条件回收 | 待落地 | 待落地 | 待落地 |
| daemon 就绪握手超时上限 | 待落地（jcode 在 Windows Server VPS 上实测 15–60s，给到 120s） | 待落地 | 待落地 |
