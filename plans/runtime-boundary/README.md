# 运行时 / UI 边界：是否做 server 解耦

调研日期 2026-08-06。三仓证据原文见同目录 `reference-*.md`。
本文只做**综合与决策**，不重复证据细节。

## 0. 一句话结论

**"server 解耦 → UI 秒开"这个因果不成立。** 秒开来自"客户端不做重活 + 第一帧先画"，
这两件事在单进程里同样能做。daemon 换来的是**多客户端共享会话**与**agent 脱离 UI 存活**，
代价是一整套跨进程正确性问题。

所以真正要决定的不是"要不要 server"，而是**边界什么时候协议化、传输什么时候跨进程**。

## 1. 三仓事实对照

| | oh-my-pi (TS) | jcode (Rust + ratatui) | opencode (TS) |
|---|---|---|---|
| 进程模型 | **单进程自包含**。TUI 直接持有 `AgentSession` 对象 | **真 daemon + 瘦客户端**。`jcode serve` 常驻，TUI 是纯 client | v1 逻辑 client-server、物理同进程双线程；v2 才有真 daemon |
| 传输 | 无（同进程函数回调） | NDJSON over Unix socket / named pipe | HTTP/1.1 + SSE + JSON |
| 事件形状 | 增量 event 回调 | 增量 delta，全量只在 `History`（连接引导） | 增量 event，首帧 `server.connected` |
| session 归属 | client 进程（JSONL 文件） | **server 唯一真源**，client 只有 UI-local 状态 | server 侧，client 纯投影 |
| attach / 重连 | **不存在**。`--resume` 是新进程重读 JSONL | 有，含接管仲裁三元判据 | 有，但一律全量 refetch |
| 是否为秒开而做 | 从未把常驻化当候选，把冷启动当**可观测性问题** | **是**，有三处明确设计 | **不是**，文档明写目标是多端 + 可编程接入 |

关键坐标：
- oh-my-pi 单进程链路 `packages/coding-agent/src/main.ts:1604-1608` → `:428-436` → `modes/interactive-mode.ts:716`；
  事件是直接回调 `modes/controllers/event-controller.ts:428-430`。
- jcode daemon 判定/拉起 `src/cli/dispatch.rs:890-947`、`:1250-1281`；单实例 `flock` `crates/jcode-app-core/src/server/socket.rs:159-193`。
- opencode 的定性证据：`packages/web/src/content/docs/server.mdx:57`（目标是多客户端 + 可编程接入）；
  v1 默认 TUI 每次都新建 Worker 重载整个 server `packages/opencode/src/cli/cmd/tui.ts:210`，**秒开收益为零**。

## 2. 秒开真正来自哪里

jcode 是三仓里唯一为首帧延迟做过设计的，三招全部**与 daemon 无关**：

1. **客户端不初始化 provider、不初始化 tool registry**
   `crates/jcode-tui/src/tui/app/tui_lifecycle.rs:1266-1268`：`InertRuntimeProvider` + `Registry::empty()`；
   `InertRuntimeProvider::complete()` 直接返 `Err`，从架构上禁止 UI 进程调 provider（`crates/jcode-tui/src/tui/app.rs:1617-1663`）。
2. **第一帧在连 socket 之前画**
   `crates/jcode-tui/src/tui/app/run_shell.rs:679-707`：先 `draw_full` 打 `first_frame` 埋点，再 `connect_with_retry`。
3. **resume 时本地预填 transcript，不等 server 的 History**
   `crates/jcode-tui/src/tui/app/tui_lifecycle.rs:1196-1258`（渲染完立刻 strip + `shrink_to_fit` + 归还 arena 页），
   随后 `Subscribe` 带 `client_has_local_history=true` 让 server 走轻量元数据路径（`backend.rs:310-316,342-357`）。

外加两条反雷群/反阻塞设计：模型目录默认不在 attach 后拉（OpenRouter 可达 ~800 KB/client，`backend.rs:363-379`）；
凭据探测默认跳过，注释指明 Windows 安全软件让凭据读变得极慢（`src/cli/dispatch.rs:1144-1156`）。

oh-my-pi 的同类优化也全在单进程内：模型目录同步读 SQLite 缓存、网络刷新推到 session 建好之后
（`config/model-registry.ts:862-899,1312-1321` + `main.ts:1519-1521`）；启动扫描 5s 死线且超时后台继续暖 cache
（`sdk.ts:1282,1599-1623`）。

**结论：这五条是"秒开"的全部内容，且一条都不需要 daemon。**

## 3. daemon 真正买到什么

只有这三样，单进程拿不到：

1. **多客户端同时看同一个会话**（TUI + 编辑器插件 + 移动端）；
2. **agent 脱离 UI 存活** —— 关掉终端任务继续跑，回来 attach。
   前提是 turn 属于 session scope 而非连接：opencode `packages/opencode/src/effect/runner.ts:88` +
   `session/run-state.ts:37`（HTTP 断了 agent 继续跑，只有显式 abort 才停）；
3. **暖进程复用** —— 但收益比想象小：opencode v2 唯一的复用收益是"已有 daemon 且版本一致直接返回 URL"
   （`packages/cli/src/services/daemon.ts:112-114`），且**非编译构建下禁用复用**（`:114-115`）。

## 4. daemon 的代价清单（全部有仓内证据）

不是"多写点代码"，是一批容易整批漏掉的正确性问题：

| 问题 | 证据 |
|---|---|
| 单实例互斥 + 陈旧 socket 回收（必须"无活监听 **且** 能拿独占锁"双条件） | jcode `server/socket.rs:88-137,159-193` |
| 就绪握手（别靠 stdout 文本匹配） | jcode `JCODE_READY_FD` `server/socket.rs:229-274`；反例 opencode `packages/sdk/js/src/v2/server.ts:55-70` |
| Windows named pipe：**不能连探两次**（第一次非阻塞探测占掉唯一 pipe 实例，接着 connect 在 `ERROR_PIPE_BUSY` 里永远等） | jcode `server/socket.rs:72-83` |
| Windows named pipe 无文件权限模型 → token 是唯一防线 | oh-my-pi `launch/paths.ts:8-11` + `client.ts:90` |
| 接管仲裁（防两个 client 抢同一 session） | jcode `server/client_session.rs:1264-1265,1417-1418,1485-1490` |
| 取消传播要三层，`CancellationToken` 不够 | jcode `crates/jcode-agent-runtime/src/lib.rs:30-115` + `turn_cancel_registry.rs:3-24` |
| cancel 必须**先于** Ack 分发（共享 writer 会让取消排在出站字节后） | jcode `server/client_lifecycle.rs:946-988` |
| 权限审批跨进程回环 —— **两家都有洞** | opencode：pending 只在内存、重连不重拉，SSE 在 `permission.asked` 后断开则工具永久挂着（`packages/opencode/src/permission/index.ts:98-107` + `packages/tui/src/context/sync.tsx:451-532` 无 `permission.list`）。jcode：协议里**根本没有** permission 类消息 |
| 背压：两家各错一半 | opencode v1 `Queue.unbounded`（撑爆 server）vs v2 `dropping(256)` 溢出即打挂整条流（`packages/core/src/event.ts:152-161`） |
| 大帧：持久 read buffer + scan 游标 + 帧上限 + 容量回缩，四个一起抄缺一即 bug | jcode `crates/jcode-tui/src/tui/backend.rs:230-296` |
| 版本兼容（常驻 daemon 必然遇到新 client + 旧 daemon） | oh-my-pi 软兼容 `launch/protocol.ts:82-83`；opencode 硬杀重启 `daemon.ts:74-78` |
| 两套执行路径 = 两套 bug | jcode `jcode run` 绕过 daemon 进程内跑 Agent（`src/cli/commands.rs:2362-2415`），headless 的 MCP 冷缓存问题要单独打补丁 + `JCODE_RUN_MCP_WAIT_MS` 兜底 |
| 回环 HTTP 的额外税 | opencode 文档专门警告必须把 localhost 加进 `NO_PROXY`，否则请求绕进企业代理形成路由环（`packages/web/src/content/docs/network.mdx:21-24`） |

还有一条**Rust 特有的首付**：oh-my-pi 能把 session 条目直接 `JSON.stringify` 上线，是因为它们本来就是 GC 管的 plain object。
ZCode 要跨进程，必须先给整棵事件/条目树定义 serde wire 契约并决定 owned vs borrowed —— 这是真实首付，不是可推后的细节
（参照 oh-my-pi 的分层：**wire 类型必须独立于 runtime 类型**，`packages/wire/src/index.ts`）。

## 5. 三仓都踩到的同一个坑：切分不彻底

- jcode：`jcode-app-core` 直接依赖 ratatui/crossterm（`crates/jcode-app-core/Cargo.toml:73-75`），daemon 白扛渲染栈；
  `crates/jcode-tui/src/lib.rs:23` 用 `pub use jcode_app_core::*` 整体转出，动机自陈是"让 `crate::<module>` 路径原样解析"
  —— **为了不改 import 而放弃编译边界**。结果协议边界靠自律而非编译器，且
  `scripts/check_dependency_boundaries.py:26-51` 只护 `*-types` crate，**不检查 tui→runtime**，CI 看不见这个洞。
- opencode：内部协议自陈 "unversioned, TUI-shaped, coupled to client rendering assumptions"
  （`docs/HARNESS_API_AND_DESKTOP_REWRITE.md:5-10`），于是又叠了一层 v1 facade + 独立 bridge 进程，permission 直接不支持。

**教训**：协议边界要么由编译器与 CI 强制，要么就不存在。

## 6. 已否决的备选：协议优先，传输后置

> **这一节记录的是被否决的方案，不是要执行的路线。**
> 实际裁决见第 9 节：daemon 独立进程、客户端跨进程连。
> 需要落地步骤时看 `implementation.md`，不要照本节施工。

当时的提案是把"协议化边界"与"跨进程传输"拆成两步：先只落 `crates/protocol` 与
`crate::transport`，客户端与运行时仍在同一进程内用 `mpsc` 传 `enum`（零序列化），
等确实需要多端时再新增一个跨进程 transport 实现。

**否决理由**：目标是多端共享会话与 agent 脱离 UI 存活，两者都要求 daemon 是独立进程 ——
TUI 关掉它还得活着。同进程方案下 server 就在 TUI 进程里，agent 活不过 UI，
这正是 opencode v1 的形状（Worker 线程 + 进程内 `Server.Default().app.fetch`）。
推迟跨进程只是推迟目标，不是推迟代价。

**其中被保留下来的部分**（已进入实际方案）：

- 协议边界由编译器与 CI 强制，依赖方向 `tui -> protocol <- runtime`；
- `enum` 直传零序列化 —— 位置改为 **daemon 内部**（连接 handler → agent runtime）。
  跨进程边界本来就必须付序列化成本；opencode 的 Worker RPC 把 body 整体
  `await request.text()` 字符串化（`packages/opencode/src/util/rpc.ts:8`）是反面教材；
- `stream_pair()` 复用同一个 `handle_client`（jcode `crates/jcode-base/src/gateway.rs:211-220`）——
  用途从"以后再加传输"变成"headless 与 TUI 共用一条执行路径"；
- 秒开靠第 2 节那五条，与传输形态无关，可独立推进。

## 7. 可以直接抄的清单（同语言，语义零落差）

来自 jcode，全部是 Rust：

1. `crate::transport` cfg 类型别名 + `stream_pair()`（`transport/mod.rs:1-8`、`unix.rs:1-6,17-19`、`windows.rs:11-116`）。
2. `Stream::pair()` 复用 `handle_client` 的传输适配模式（`gateway.rs:211-220`）。
3. `InterruptSignal`（`AtomicBool` + epoch + `Notify`）+ `turn_cancel_registry`
   （`crates/jcode-agent-runtime/src/lib.rs:30-115`、`turn_cancel_registry.rs`）。
   `CancellationToken` 不够用的具体理由：同一次取消要打到可能是多个实例的信号上，且延时 reset 不能抹掉新 fire。
4. cancel 请求优先于 Ack 分发（`server/client_lifecycle.rs:946-988`）。
5. 持久 read buffer + scan 游标 + 帧上限 + 容量回缩，**四个一起**（`crates/jcode-tui/src/tui/backend.rs:230-296`）。
6. oneshot-by-request-id 的 UI 回环（`server/client_lifecycle.rs:633-666` + `wire.rs:347-356`）。
   ZCode 的权限审批照这个形状做 —— jcode 把机制建好了却没用在权限上，opencode 用了但重连会丢。
7. 就绪握手 + `flock` 单实例 + 陈旧 socket 双条件回收（`server/socket.rs:229-274,88-137,159-193`）。
8. **持久化：已改判为 JSONL 条目树（用户拍板）。** 采用 oh-my-pi
   `packages/coding-agent/src/session/session-entries.ts:58-62,245-260` 的形状：
   每条条目带 `parent_id` 构成树，当前上下文 = 根到 head 的路径，`/branch` 与 `/rewind`
   只是换 head，零拷贝。落地在 `crates/agent/src/session/`。
   - 被否决的方案与代价：jcode 的 snapshot `.json` + 增量 `.journal.jsonl`
     （`crates/jcode-base/src/session/persistence.rs:66-125,320-405`）写放大最小，但只有线性历史，
     要做分支就得整份复制会话文件；opencode 的 SQLite 事件溯源
     （`packages/core/src/event.ts:234-370`）可重放可审计，但每次非 delta 状态变更都是一次同步
     事务，且已付出约 40 条 migration 的维护债（`packages/core/src/database/migration/`）。
   - **仍然照抄**：读路径**单行损坏只跳这一行**并继续（同上 jcode 坐标）。首错即停的表现是
     "用户丢了整个尾部"。

来自 opencode：

9. turn 属于 session scope 而非请求（`packages/opencode/src/effect/runner.ts:88` + `session/run-state.ts:37`）。
   这是 UI 可随时断开重连的**唯一必要条件**，且在单进程里也该这么写。
10. **权限：只抄回环，不抄 ruleset（用户拍板）。**
    - **抄**：`always` 连锁放行 / `reject` 连坐 / 每次结算都广播 replied
      （`packages/opencode/src/permission/index.ts:98-167`）。jcode 这块没有可抄的
      （`crates/jcode-base/src/safety.rs:180-193` 的审批从不阻塞，三个结算变体是死代码）。
    - **不抄**：有序 allow/deny/ask ruleset + `findLast`（`:28-38`）。改用 oh-my-pi 的
      **tier × policy**，默认模式 `yolo`（`packages/coding-agent/src/tools/approval.ts:29-185`、
      `packages/coding-agent/src/config/settings-schema.ts:3675-3678`）。
      代价对比：ruleset 表达力更强、支持路径级 pattern，但默认 `ask` 意味着开箱即用每个工具
      都弹窗，且必须先有完整审批 UI 才可用；tier × policy 开箱零摩擦，代价是粒度只到工具级。
      本仓面向单人 power-user，取后者。
    - 粒度补偿：`always` 的授权键是 `(工具名, 工具声明的作用域)`，工具可自行收窄
      （`bash` 返回 `bash:git`），使"总是允许"不等于放行整类工具。
      已落地在 `crates/agent/src/approval.rs`。
11. 取消要级联到后台作业：先递归取消所有指向本 session 的 background job，循环到无新增，再取消 runner
    （`packages/opencode/src/session/run-state.ts:108-140`）。
12. daemon 注册文件 + 健康认证 + 版本比对 + 自杀式互斥（`packages/cli/src/services/daemon.ts:40-41,64-78,110-131,159-177`）
    —— 只有真做 daemon 时才需要。注意"先健康认证再对 PID 发信号"以避免 PID 复用误杀（注释 `:152-153`）。

## 8. 明确不抄

| 不抄 | 出处 | 理由 |
|---|---|---|
| `pub use <runtime>::*` 整体转出 | jcode `crates/jcode-tui/src/lib.rs:23` | 为省 import 放弃编译边界，协议边界退化成自律 |
| runtime crate 依赖 ratatui/crossterm | jcode `crates/jcode-app-core/Cargo.toml:73-75` | daemon 白扛渲染栈 |
| headless 绕过统一执行路径 | jcode `src/cli/commands.rs:2362-2415` | 两套执行路径 = 两套 bug + 单独的 MCP 冷缓存补丁 |
| 内部协议 + 版本化 facade 二元结构 | opencode `docs/HARNESS_API_AND_DESKTOP_REWRITE.md:5-10` | 从零起步就该只有一套版本化协议 |
| 为进程内通信开真 TCP | opencode `packages/opencode/src/cli/cmd/acp.ts:25-31` | 进程内 handler 就在手边 |
| 默认无鉴权 + mDNS 自动 `0.0.0.0` | opencode `middleware/authorization.ts:100-102` + `cli/network.ts:71-75` | 等于把无密码的 agent 执行端点广播到局域网 |
| 靠 stdout 文本匹配判 server 就绪 | opencode `packages/sdk/js/src/v2/server.ts:55-70` | 日志格式一改就断；用注册文件或父子 pipe 握手 |
| OpenAPI → SDK 自动生成 | opencode `packages/httpapi-codegen` | TS 生态近乎零成本，Rust 要付额外构建步骤；收益全在第三方 SDK 生态侧 |
| 进程内 fs cache 无 TTL 无上限 | oh-my-pi `capability/fs.ts:4-5,104-107` | 长会话持有过期目录项且无失效信号 |
| token 竞争用 100×50ms 轮询 | oh-my-pi `launch/client.ts:81-103` | 慢盘下是 5s 静默阻塞；Rust 用文件锁或一次性 `create_new` |

## 9. 已裁决（2026-08-06）

| 议题 | 裁决 |
|---|---|
| 目标 | **现在就要多端共享会话** —— agent 运行时活在独立 daemon 进程，TUI / 编辑器插件 / 移动端同时连它，agent 脱离 UI 存活 |
| 客户端 ↔ daemon 传输 | 跨进程，`zcode_utils::transport`（Unix socket / Windows named pipe）+ NDJSON |
| `enum` 直传零序列化用在哪 | **daemon 内部**：连接 handler → session/agent runtime。跨进程边界本来就必须付序列化 |
| headless | 与 TUI **共用同一条执行路径**。daemon 在就连它；不在就同进程自托管，用 `stream_pair()` 把自己接上同一个 `handle_client` —— 不开真 socket，也不多一套执行路径 |
| 协议类型归属 | 新增 `crates/protocol`（`zcode-protocol`）。**所有 wire 类型归它所有**，依赖方向 `tui -> protocol <- runtime`，用 CI ratchet 卡住 |

第 6 节记录的"协议优先，传输后置"是**被否决的备选**，仅供追溯取舍，不是施工依据。

分期、抄源坐标与验证矩阵见 `implementation.md`。

### 裁决时被明确指出的两个设计错误（已修正）

1. **"变体定义权归 runtime"会毁掉边界。** 客户端为了反序列化 payload 就必须依赖 runtime，
   协议 crate 只剩通用 framing。修正：wire 类型全部归 `zcode-protocol`，
   领域类型互转是 host adapter 的职责。
2. **未知 `Request` 不能静默跳过。** 跳过会让等 `reply_to` 的调用方永久挂着。
   修正：只有明确可丢弃的推送 `Event` 才跳过，未知请求必须回结构化
   `ErrorCode::UnsupportedRequest`（已落在 `crates/protocol/src/error.rs`）。
