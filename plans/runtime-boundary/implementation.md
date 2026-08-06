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
| wire 变体 | `crates/protocol/src/wire/`（`types.rs` / `request.rs` / `event.rs` / `mod.rs`） | 单元测试 + `tests/wire_schema.rs` 形状快照（16 Request / 10 Reply / 19 Event，穷尽性哨兵双保险） |
| 双向握手 | `crates/protocol/src/version.rs`（`ClientHello` / `ServerHello` / `ClientAuth`） | 3 帧往返 + "客户端首帧不含凭据"断言 |
| 取消注册表与级联 | `crates/agent/src/cancel.rs` | 7 用例：多 turn 扇出、会话隔离、守卫复位、子会话递归、深链多轮、取消期间新增作业（cfg(test) pass hook）、同信号只 fire 一次 |
| stdin 回环 | `crates/agent/src/stdin.rs` | 7 用例：问答回环、重复回答、`cancel_all` 解挂、pending 清空、并发不串担 |
| daemon 端点原语 | `crates/utils/src/daemon.rs` | Windows 真机 10 用例：证明域分隔/nonce 绑定、密钥不进日志、注册文件原子往返、只删自己那份、锁互斥、活端点不回收、死端点回收、就绪令牌校验、子进程早死报错 |
| 依赖方向 ratchet | `.omp/checks/dep-boundary.check.ts` + CI `dep-boundary` job | 正反两跑：干净仓库 exit 0；临时给 `zcode-tui` 加 `zcode-agent` 依赖 exit 1 |

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

## 第 1 期已完成：落地时相对本计划的偏离

变体、握手、取消级联、daemon 原语已全部落盘（见"已落地"表）。下面逐条记录**与本文档原先
写法不一致的地方**，以及为什么改——不要按旧写法回改代码。

| 计划原写法 | 实际落地 | 理由 |
|---|---|---|
| `Request::PermissionReply` / `Event::PermissionAsked` / `Request::PermissionList` | `Request::ApprovalRespond` / `Event::ApprovalRequested` / `Request::PendingList` | 词汇随已落盘的领域层（`crates/agent/src/approval.rs` 的 `ApprovalGate` / `ApprovalReply` / `PendingApproval`），本文档 `:57-59` 自己也指定按那里映射；再引入一套 `Permission*` 就是与既有约定并行的第二套写法。`PendingList` 同时返回审批与 stdin 两类待回答项：它们是同一个失败模式（待回答状态挂在连接上），重连一次往返取齐比两条路径少一个"忘了调"的机会。**能力一条不少**：重连必须能重拉待审批，这条由 `PendingList` 与 `Reply::Subscribed.pending` 双路保证 |
| `Subscribe` 带 `client_has_local_history=true` | `Subscribe { client, has_local_history, takeover, since }` | jcode 那个布尔在 server 侧只当 takeover 授权凭证、并不裁剪 History 载荷（`server/client_session.rs:1343-1356`）。本仓把两件事拆开：三元判据管仲裁，`since` 游标管载荷 |
| 握手沿用 `Hello`（仅版本协商） | 三帧双向挑战应答 + `ErrorCode::Unauthorized` | Windows named pipe 没有文件权限模型，任何本机进程都能抢名占坑；明文 bearer 一次连接就被收走，占坑者随后既能冒充 daemon 也能去连真 daemon。让服务端先证明持有密钥，占坑者在第二帧被识破 |
| 就绪握手用父子 pipe（`JCODE_READY_FD`） | `ReadyChannel`：`transport` 上的一次性带令牌端点 | 语义等价（与本次 spawn 一一绑定），但跨平台且无 `unsafe`；jcode 那条路径整段 `#[cfg(unix)]`，Windows 侧只能退化成轮询 |
| 取消注册表照抄 jcode 的进程级 `static` | 可持有的 `CancelRegistry` 对象 | jcode 自己记了代价：同名 session 的测试无法并行。另外本表**登记后台作业**并沿子会话递归，jcode 只登记 turn |
| 后台作业级联"循环到无新增" | 同上，外加 64 轮安全阀 + `CancelReport::cascade_exhausted` | 无界循环在取消路径上会变活锁。触顶时 runner **照打**（它是新作业的唯一来源），但报告标志告诉调用方这次取消不干净 |

仍然按计划落地、未打折的部分：`Event::Resync`（慢消费者不断流）、stdin 回环与审批共用一套
"挂在 session 上"的机制、`always` 连锁 / `reject` 连坐 / 每次结算都广播、
未知 `Event` 静默跳过而未知 `Request` 必回 `UnsupportedRequest`。

## 待落地

### 第 2 期：连接处理与 daemon 生命周期

**原语已全部落盘**（取消三层、级联、注册文件、单实例锁、双条件回收、就绪握手、握手证明），
见"已落地"表。剩下的都是**装配**，归 `crates/coding-agent`：

- `handle_client`：每 client 一个出站 forwarder；**cancel 请求先于 Ack 分发**
  （jcode `server/client_lifecycle.rs:946-988`——共享 writer 忙时取消会排在出站字节后面）。
  取消动作调 `zcode_agent::cancel::CancelRegistry::cancel_session`，三层与级联它已经做完了。
- session 表：`SessionId -> AgentRuntime`，加上把 `AgentEvent` 翻成 `wire::Event` 的
  host adapter（领域类型 ↔ wire 类型的互转在这里，两侧都不许绕过 `zcode-protocol`）。
- daemon 生命周期编排：`SingleInstanceLock::acquire` → `reap_stale_endpoint` → `Listener::bind`
  → `Registration::write_atomic` → `signal_ready`。**顺序不可交换**：拿锁必须先于一切副作用，
  回收的正确性依赖这条不变式。daemon 还要定期重读注册文件自查 `id`，被抢注就自行退出
  （opencode `packages/cli/src/services/daemon.ts:174-179`）。
- 客户端侧健康认证：读注册文件 → connect → 三帧握手（`verify_proof` 在
  `zcode_utils::daemon`）。**先认证再对 PID 发信号**，PID 会被复用
  （opencode `daemon.ts:152-159`）。
- 接管仲裁的**执行**：判据字段已在 `Subscribe` 里（`client` / `has_local_history` / `takeover`），
  仲裁逻辑与踢人动作待写（jcode `server/client_session.rs:1264-1265,1417-1418,1485-1490`；
  注意首次 Subscribe 与 Resume 两处判据不同，Resume 多一条"同实例直通"）。
- Windows 主线程栈设 8 MiB（jcode `src/main.rs:77-95`：Windows PE 默认主线程栈 1 MiB，
  provider setup 在 tokio 接管前就能吃穿它，得到**不可恢复**的 `STATUS_STACK_OVERFLOW`。
  用专线程而不是 `/STACK` 链接器参数：后者是 crate 级的，会波及每个辅助二进制）。

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

## CI 闸门

两条都已落地：

- **依赖方向 ratchet**：`.omp/checks/dep-boundary.check.ts` + CI `dep-boundary` job。
  三条规则：`zcode-protocol` 不依赖任何内部 crate；`zcode-tui` 不依赖运行时 crate；
  运行时 crate 不依赖 `ratatui` / `crossterm`。`crates/coding-agent` 是装配层，豁免后两条。
  jcode 的 `scripts/check_dependency_boundaries.py:26-51` 只护 `*-types` crate，
  看不见 `tui -> runtime` 这条边——本脚本覆盖了它。
- **协议 schema 快照**：`crates/protocol/tests/wire_schema.rs` + `tests/wire-schema.json`，
  外加三个穷尽性哨兵（逐行 `match` + 变体计数），新增变体时先编译报错、再计数报错。

## 待真机验证（本机 Windows 已覆盖的除外）

| 项 | Windows | macOS | Linux |
|---|---|---|---|
| `transport` 往返与单实例互斥 | 已验证 | **需真机** | **需真机** |
| 单实例文件锁互斥 | 已验证 | **需真机** | **需真机** |
| 注册文件原子覆盖写（目标已存在） | 已验证 | **需真机** | **需真机** |
| 就绪握手令牌校验与子进程早死报错 | 已验证 | **需真机** | **需真机** |
| 陈旧端点双条件回收 | 已验证（Windows 无文件节点，回收恒为 no-op） | **需真机**（真正会 unlink 的只有类 Unix） | **需真机** |
| daemon 就绪握手超时上限 | 待装配（jcode 在 Windows Server VPS 上实测 15–60s，本仓给到 120s） | 待装配 | 待装配 |
