调研主题：agent 运行时与 UI 的进程解耦
仓库：C:/Users/zero/Desktop/code/github/opencode
日期：2026-08-06

结论：opencode 的 client-server 不是为「UI 秒开」做的。文档明写目标是「支持多客户端 + 可编程接入」(packages/web/src/content/docs/server.mdx:57)。v1 默认 TUI 每次启动都新建 Worker 线程重新加载整个 server(packages/opencode/src/cli/cmd/tui.ts:210)，不复用进程也不复用状态，秒开收益为零。真正的常驻 daemon 只在 v2 CLI(packages/cli)出现，其动机在注释里是「让被发现的客户端跨 server 重启复用同一份凭据」(packages/cli/src/services/daemon.ts:50-51)与 Electron 桌面端复用同一后端(packages/desktop/src/main/background-cli.ts:31-45)，仍是多端共享而非启动延迟。唯一与启动体感相关的 OPENCODE_FAST_BOOT 只是跳过 loading 遮罩，不减少任何工作(packages/tui/src/app.tsx:278, packages/tui/src/context/sync.tsx:558-561, packages/tui/src/app.tsx:1129-1131)。目标 = 多端 / 远程 / 插件与 SDK 生态；v2 daemon 追加的目标 = desktop 与 CLI 共享同一后端进程。

```json
{
  "repo": "opencode (C:/Users/zero/Desktop/code/github/opencode, 默认分支 dev)",
  "topic": "agent 运行时与 UI 的进程解耦（server/daemon + 多客户端）",
  "verdict_on_acceptance_question": "opencode 的 client-server 不是为「UI 秒开」做的。文档明写目标是「支持多客户端 + 可编程接入」(packages/web/src/content/docs/server.mdx:57)。v1 默认 TUI 每次启动都新建 Worker 线程重新加载整个 server(packages/opencode/src/cli/cmd/tui.ts:210)，不复用进程也不复用状态，秒开收益为零。真正的常驻 daemon 只在 v2 CLI(packages/cli)出现，其动机在注释里是「让被发现的客户端跨 server 重启复用同一份凭据」(packages/cli/src/services/daemon.ts:50-51)与 Electron 桌面端复用同一后端(packages/desktop/src/main/background-cli.ts:31-45)，仍是多端共享而非启动延迟。唯一与启动体感相关的 OPENCODE_FAST_BOOT 只是跳过 loading 遮罩，不减少任何工作(packages/tui/src/app.tsx:278, packages/tui/src/context/sync.tsx:558-561, packages/tui/src/app.tsx:1129-1131)。目标 = 多端 / 远程 / 插件与 SDK 生态；v2 daemon 追加的目标 = desktop 与 CLI 共享同一后端进程。",
  "s1_server_daemon_layer": {
    "summary": "两套架构并存：v1(packages/opencode) 是逻辑 client-server、物理可折叠成同进程双线程；v2(packages/cli + packages/server + packages/core) 是真 daemon。",
    "v1_process_forms": [
      {
        "form": "opencode(默认 TUI)",
        "server_host": "主线程 new Worker(worker.ts)，server 跑在 Worker 线程",
        "transport": "postMessage JSON-RPC 伪造的 fetch",
        "coords": "packages/opencode/src/cli/cmd/tui.ts:210-247, packages/opencode/src/cli/tui/worker.ts:31-51"
      },
      {
        "form": "opencode --port/--hostname/--mdns",
        "server_host": "同上 Worker，额外 Server.listen() 开真 TCP",
        "transport": "HTTP + SSE",
        "coords": "packages/opencode/src/cli/cmd/tui.ts:234-247, packages/opencode/src/cli/tui/worker.ts:52-57"
      },
      {
        "form": "opencode serve",
        "server_host": "前台进程，无 TUI",
        "transport": "HTTP + SSE",
        "coords": "packages/opencode/src/cli/cmd/serve.ts:6-25"
      },
      {
        "form": "opencode attach <url>",
        "server_host": "不起，连别人的",
        "transport": "HTTP + SSE",
        "coords": "packages/opencode/src/cli/cmd/attach.ts:8-13,120-140"
      },
      {
        "form": "opencode acp",
        "server_host": "起 TCP server，自己再用 SDK 连回去",
        "transport": "stdio JSON-RPC ↔ 回环 HTTP",
        "coords": "packages/opencode/src/cli/cmd/acp.ts:25-31"
      }
    ],
    "v1_two_entrypoints": "Server.Default().app.fetch(request) 是不经 TCP 的进程内 web handler(packages/opencode/src/server/server.ts:56-69)；Server.listen(opts) 才开真 socket(packages/opencode/src/server/server.ts:73-115)。默认 TUI 走前者。",
    "v1_serve_no_ambient_instance": "serve 显式 instance:false，注释写明 server 按 x-opencode-directory 逐请求加载 instance(packages/opencode/src/cli/cmd/serve.ts:12-13) — server 侧冷启动便宜的根因。",
    "v2_daemon": "默认命令 = 确保 daemon 在跑再把 TUI 接上(packages/cli/src/commands/handlers/default.ts:6-13, packages/cli/src/tui.ts:7-20)。生命周期命令 service start/restart/status/stop/password(packages/cli/src/commands/commands.ts:29-42)，实现 packages/cli/src/services/daemon.ts；server 本体 createRoutes(password) 在 packages/server/src/routes.ts:39-64，由 packages/cli/src/commands/handlers/serve.ts:18-28 拉起。"
  },
  "s2_transport_and_protocol": {
    "stack": "HTTP/1.1 + SSE + JSON。无 Unix socket / named pipe / stdio JSON-RPC 作为主传输。协议栈是 Effect HttpApi(typed route tree + 自动 OpenAPI)，装配点 packages/opencode/src/server/server.ts:104-115，路由规约 packages/opencode/src/server/routes/instance/httpapi/AGENTS.md(普通端点与 SSE 用 HttpApiBuilder.group；仅 WebSocket 升级与兜底 UI 用 handleRaw/HttpRouter.use)。",
    "event_shape": "增量 event，不是全量 state 快照。v1 GET /event 首帧固定 server.connected，10s 一次 server.heartbeat(packages/opencode/src/server/routes/instance/httpapi/handlers/event.ts:63-67,70)。v2 GET /api/event 首帧同样 server.connected，15s 一次 SSE 注释心跳 ': heartbeat'(packages/server/src/handlers/event.ts:36-37)。",
    "v1_filtering_and_termination": "SSE 按 instance 目录 + workspaceID 过滤(packages/opencode/src/server/routes/instance/httpapi/handlers/event.ts:37-42)，收到 server.instance.disposed 主动结束流(同文件:59-61)，逼客户端重连并重新 bootstrap。",
    "subscribe_before_first_frame": "注释明确：监听器注册是 eager 的，先 Queue.unbounded + events.listen 再构造响应流，所以首帧与流启动之间发布的事件不会丢(packages/opencode/src/server/routes/instance/httpapi/handlers/event.ts:29-30)。",
    "websocket": "仅用于 PTY 转发与 workspace 代理(packages/opencode/src/server/routes/instance/httpapi/middleware/workspace-routing.ts:135, packages/opencode/src/server/routes/instance/httpapi/websocket-tracker.ts)。",
    "worker_rpc": "手写 60 行 JSON-RPC，{type:'rpc.request'|'rpc.result'|'rpc.event'}，JSON 全量序列化、无背压、无超时、pending Map 只 resolve 不 reject(packages/opencode/src/util/rpc.ts:6-14,44-56)。请求体被 await request.text() 整体拉成字符串(packages/opencode/src/cli/cmd/tui.ts:26-33, packages/opencode/src/cli/tui/worker.ts:32-51)，流式响应在这条路径被压平；事件走单独 rpc.event 通道绕开(packages/opencode/src/cli/cmd/tui.ts:41-49, worker.ts:25-27)。"
  },
  "s3_session_state_and_persistence": {
    "ownership": "状态全部在 server 侧，client 是纯投影。",
    "v1_instance_cache": "每工作目录一个 InstanceContext，缓存在 server 进程内 Map<directory, Entry>，首次请求懒加载(packages/opencode/src/project/instance-store.ts:107-124)。中间件从 ?directory= 或 x-opencode-directory 头解目录(packages/opencode/src/server/routes/instance/httpapi/middleware/workspace-routing.ts:97-99)再 store.load(packages/opencode/src/server/routes/instance/httpapi/middleware/instance-context.ts:24-35)。",
    "v1_persistence": "一 key 一个 .json 文件，根 Global.Path.data/storage，带 TxReentrantLock 读写锁(packages/opencode/src/storage/storage.ts:63-65,225-233,247-289)，有 migration marker 与两条迁移(同文件:79-205,236-250)。",
    "run_owned_by_server": "SessionRunState 把 runner fiber Effect.forkIn(scope) 到 instance scope(packages/opencode/src/effect/runner.ts:88, scope 来自 packages/opencode/src/session/run-state.ts:37)。HTTP 请求断了 agent 继续跑；只有 POST /session/:id/abort → promptSvc.cancel 才停(packages/opencode/src/server/routes/instance/httpapi/handlers/session.ts:232-235, packages/opencode/src/session/run-state.ts:76-85)。",
    "prompt_vs_promptAsync": "prompt 阻塞到 turn 结束返回完整 message(packages/opencode/src/server/routes/instance/httpapi/handlers/session.ts:295-309)；promptAsync fork 进 server scope 并 204 返回，失败只发 Session.Event.Error(同文件:311-333)。TUI 主输入框用阻塞版 prompt(packages/tui/src/component/prompt/index.tsx:1093-1122)，只有新建 workspace/移动会话用 promptAsync(packages/tui/src/component/dialog-workspace-create.tsx:139-143, packages/tui/src/component/prompt/move.tsx:139-143)。",
    "v2_event_sourcing": "SQLite(drizzle) + 约 40 条 migration(packages/core/src/database/migration/)，Database.node 进 applicationServices(packages/server/src/routes.ts:19,28)。事件写 EventTable/EventSequenceTable，按 aggregate_id(=sessionID) + 单调 seq(packages/core/src/event.ts:21-31,62-104)。",
    "v2_resume_primitive": "durable({aggregateID, after}) 先注册 wake 订阅、再从 DB 读 seq>after、live 段由 wake 触发再读，顺序保证无缝无重(packages/core/src/event.ts:573-609)。对外 GET /api/session/:id/events?after=<seq>(packages/server/src/handlers/session.ts:358-363)与分页 session.history?after=&limit=(同文件:334-357，签名 packages/core/src/session.ts:133-141,346-356)。",
    "actual_client_reconnect": "v1 TUI SSE 断开后指数退避 1s→30s 重连(packages/tui/src/context/sdk.tsx:53-54,107-112)，不带任何游标；收到 server.instance.disposed 直接重跑 bootstrap()(packages/tui/src/context/sync.tsx:172-174)。Web/Desktop 同样是 server.connected → bootstrap.refetch() + 逐目录重排队(packages/app/src/context/server-sync.tsx:547-548,563-572, packages/app/src/context/global-sync/event-reducer.ts:42-45)。结论：v2 已具备 per-session 游标续传能力，但没有任何客户端用它（已逐目录核实 packages/app/src/context 与 packages/tui/src/context 均无 session.events / seq 型 after 的引用）。"
  },
  "s4_ui_startup_and_cold_boot": {
    "v1_default_path_steps": [
      "win32InstallCtrlCGuard() — packages/opencode/src/cli/cmd/tui.ts:181",
      "解析目录 + process.chdir — packages/opencode/src/cli/cmd/tui.ts:193-200",
      "new Worker(worker.js)：整个 server bundle 在 Worker 线程重新加载，含 Heap.start() — packages/opencode/src/cli/cmd/tui.ts:210-213, packages/opencode/src/cli/tui/worker.ts:15",
      "await input(args.prompt)：非 TTY 时阻塞读完 stdin — packages/opencode/src/cli/cmd/tui.ts:66-71,229",
      "TuiConfig.get() 读配置 — packages/opencode/src/cli/cmd/tui.ts:230",
      "--session 时一次阻塞 session.get 预检 — packages/opencode/src/cli/cmd/tui.ts:249-262, packages/opencode/src/cli/tui/validate-session.ts:22-29",
      "SyncProvider.onMount → bootstrap()：6 个并发但整体阻塞的调用(config.providers/provider.list/experimental.capabilities/app.agents/config.get/project.sync，--continue 时加 session.list)，全部 resolve 才 loading→partial — packages/tui/src/context/sync.tsx:548-550,451-473",
      "partial 后再并发 11 个非阻塞调用(command/lsp/mcp/resource/formatter/session.status/provider.auth/vcs/workspace.sync/console/session.list)才 complete — packages/tui/src/context/sync.tsx:511-532",
      "启动 1s 后异步 checkUpgrade — packages/opencode/src/cli/cmd/tui.ts:264-266"
    ],
    "server_side_instance_bootstrap": "config.get() → plugin.init()(串行，因为 plugin 会改 config) → lsp/shareNext/format/vcs/snapshot/project 并发 init，各服务自己 forkScoped 慢活(packages/opencode/src/project/bootstrap.ts:33-47)。在第一个带目录的请求上同步触发。",
    "no_fast_open_design": "没有任何为秒开做的懒加载/预热/缓存/进程复用。相关的仅：Server.Default 用 lazy() 包住，webHandler 首次 fetch 才构造(packages/opencode/src/server/server.ts:56)；v2 CLI 命令 handler 全部 ()=>import() 懒加载(packages/cli/src/index.ts:11-27, packages/cli/src/framework/runtime.ts:63-77)；OPENCODE_FAST_BOOT 只影响 ready 判定与 loading 遮罩(packages/tui/src/app.tsx:278, packages/tui/src/context/sync.tsx:558-561, packages/tui/src/app.tsx:1129-1131)。",
    "v2_daemon_cold_boot": "无 daemon 时 healthy() 失败 → spawn(execPath,['serve','--register'],{detached:true,stdio:'ignore'}).unref() → 50ms 轮询最多 100 次(≈5s 上限)等 compatible()(packages/cli/src/services/daemon.ts:110-131)。已有 daemon 且版本一致直接返回 URL(同文件:112-114) — 唯一复用收益。非编译构建(bun 跑源码)永远不复用，每次杀掉重启(同文件:114-115)。",
    "desktop_cold_boot": "--version 一次 + 每个候选 XDG_STATE_HOME 各一次 service status + service start + service password，全是 execFile 子进程(packages/desktop/src/main/background-cli.ts:23-46)；另一条 sidecar 路径还要探测登录 shell 环境(packages/desktop/src/main/server.ts:45-56, packages/desktop/src/main/shell-env.ts:36-48)。"
  },
  "s5_sdk_rpc_surface": {
    "same_path_as_tui": "是。TUI 用的就是生成的 SDK：createOpencodeClient({baseUrl, fetch, headers, directory})(packages/tui/src/context/sdk.tsx:1,25-32)。attach 与内嵌 Worker 的差别只是注入不同的 fetch/events 实现(packages/opencode/src/cli/cmd/tui.ts:238-247)，上层 TUI 代码零感知。",
    "openapi": "server 发布 OpenAPI 3.1(packages/opencode/src/server/server.ts:71 的 openapi()；文档端点 /doc，见 packages/web/src/content/docs/server.mdx:75-81)。生成产物 packages/sdk/openapi.json(1.0MB)。",
    "generated_sdks": "packages/sdk/js/src/v2/gen/*.gen.ts(构建脚本 packages/sdk/js/script/build.ts)；packages/client/src/generated*(由 packages/httpapi-codegen 生成，根 AGENTS.md:2 禁止手改)；packages/sdk-next。",
    "other_clients": "ACP(编辑器插件 packages/opencode/src/cli/cmd/acp.ts:25-31)、plugin 宿主(packages/opencode/src/plugin/index.ts:142-161，本地注入 Server.Default().app.fetch，远程用 URL)、Electron 桌面(packages/desktop)、Web App(packages/app)、opencode web、v2 的 opencode api <operationId|METHOD path>(packages/cli/src/commands/commands.ts:11-27)。",
    "programmatic_spawn": "createOpencodeServer() fork opencode serve 子进程，靠 stdout 匹配 'opencode server listening' + 正则抠 URL，5s 超时(packages/sdk/js/src/v2/server.ts:23-96，尤其 55-70)。",
    "server_drives_client": "POST /tui/* 系列(append-prompt/submit/open-models/show-toast…)实现就是发普通 SSE 事件给 TUI(packages/opencode/src/server/routes/instance/httpapi/handlers/tui.ts:34-104)。IDE 插件靠它(packages/web/src/content/docs/server.mdx:63)。",
    "discovery_and_single_instance": {
      "v1": "无单实例锁、无端口文件。port=0 时先试 4096 再随机(packages/opencode/src/server/server.ts:117-122)。发现只有可选 mDNS _http._tcp 广播，服务名 opencode-<port>、host 默认 opencode.local(packages/opencode/src/server/mdns.ts:11-25)，loopback 主机名下跳过并 warning(packages/opencode/src/server/server.ts:161-166)。文档明说 TUI 随机分配端口，已有 TUI 时 opencode serve 会再起一个新 server(packages/web/src/content/docs/server.mdx:59-61,67)。",
      "v2": "有。注册文件 Global.Path.state/server.json({id,version,url,pid}，mode 0600，temp+rename 原子写)，端口 4096 起递增至 65535(packages/cli/src/services/daemon.ts:40-41,159-170, packages/cli/src/commands/handlers/serve.ts:30-37)。健康检查=带密码调 /api/health 且 2s 超时(daemon.ts:64-72)。注册方每 10s 自查注册文件 id 是否仍是自己，不是就给自己发 SIGTERM — 这就是单实例互斥(daemon.ts:171-177)。"
    }
  },
  "s6_costs_and_lessons": {
    "6_1_permission_forwarding_is_the_weakest_link": "模型=工具侧 Deferred 阻塞 + 事件通知 client + client 回 reply。ask 把 Deferred 塞进进程内 Map、发 Event.Asked、Deferred.await 挂住工具(packages/opencode/src/permission/index.ts:98-107)；reply 唤醒，'always' 追加内存 approved 并顺带放行同 session 已满足的 pending，'reject' 连坐拒绝同 session 全部 pending(同文件:109-165)；instance dispose 时 finalizer 全部 reject(同文件:56-63)。v2 同构(packages/core/src/permission.ts:119-128,178-188,220-284)。缺口：pending 只在内存，且客户端重连后不会重新拉取待审批列表 — v1 bootstrap 无任何 permission.list 调用(packages/tui/src/context/sync.tsx:451-532)；v2 写了 session.permission.refresh(packages/tui/src/context/data.tsx:438-441)但全仓无调用点。后果：SSE 在 permission.asked 后、reply 前断开，服务端工具永久挂着而 UI 无显示。服务端其实提供了 GET /permission(packages/opencode/src/server/routes/instance/httpapi/handlers/permission.ts:22-24)与 v2 permission.request.list(packages/server/src/handlers/permission.ts:17-22)，只是没人在重连时调。ZCode 若采用同架构，这是必须补的第一个洞。",
    "6_2_no_cursor_resume": "v2 底层已有 ?after=<seq>(packages/core/src/event.ts:573-609)，但客户端一律走 server.connected → 全量 refetch(packages/tui/src/context/sync.tsx:172-174, packages/app/src/context/server-sync.tsx:563-572)。每次网络抖动要重放 6+11 次 HTTP。",
    "6_3_overflow_policy": "v2 每订阅者一条 Queue.dropping(256)，offer 被拒即 Queue.fail(SubscriberOverflowError) 把整条流打挂(packages/core/src/event.ts:152-161，容量常量 packages/server/src/handlers/event.ts:9) — 慢客户端=断线。v1 反用 Queue.unbounded(packages/opencode/src/server/routes/instance/httpapi/handlers/event.ts:31) — 无上限，慢客户端把 server 内存吃穿。两边各错一半。",
    "6_4_loopback_http_real_cost": "文档专门警告：TUI 通过本地 HTTP 与 server 通信，必须把 localhost 加进 NO_PROXY，否则请求绕进企业代理形成路由环(packages/web/src/content/docs/network.mdx:21-24)。opencode acp 为跟进程内 server 说话开了真 TCP 端口再连回去(packages/opencode/src/cli/cmd/acp.ts:25-31)，而 Server.Default().app.fetch 就在手边。Worker RPC 把所有 body 序列化成字符串整体搬运(packages/opencode/src/util/rpc.ts:8,45-54, packages/opencode/src/cli/cmd/tui.ts:28-36)。",
    "6_5_default_no_auth": "不设 OPENCODE_SERVER_PASSWORD 时鉴权中间件退化成恒等函数(packages/opencode/src/server/routes/instance/httpapi/middleware/authorization.ts:100-102,120-122)，opencode serve 只打一行 warning(packages/opencode/src/cli/cmd/serve.ts:19-21)。配合 --mdns 会把 hostname 默认改成 0.0.0.0(packages/opencode/src/cli/network.ts:71-75) — 广播一个无密码的 agent 执行端点到局域网。v2 修正：daemon 强制生成 32 字节随机密码存 mode 0600 文件(packages/cli/src/services/daemon.ts:45-56)，server 启动必带(packages/cli/src/commands/handlers/serve.ts:20-21)。",
    "6_6_single_tui_assumption": "tui-control.ts 的请求/响应队列是模块级全局单例、无 client 标识(packages/opencode/src/server/shared/tui-control.ts:12-13)。两个 TUI 连同一 server 会互相抢答 /tui/control/next。且生产代码无 submitTuiRequest 调用方，全仓仅测试 harness 使用(packages/opencode/test/server/httpapi-exercise/runner.ts:185)。已是死代码+错误抽象。",
    "6_7_shutdown_semantics": "listener.stop(close) 要区分优雅关闭与强杀：monkey-patch server.close 以便在 finalizer 里 closeAllConnections()(packages/opencode/src/server/server.ts:178-196，注释 180-182)，外加独立 WebSocket 追踪器逐个关(每个 1s 超时，packages/opencode/src/server/routes/instance/httpapi/websocket-tracker.ts:33-42)，HTTP gracefulShutdownTimeout 1s(server.ts:189)。另有 Effect 特有坑：必须为每个 listener 装新的 ConfigProvider，否则 Effect 把首次读到的 process.env 快照缓存在模块级单例上(server.ts:107-113，五行注释)。",
    "6_8_version_compat": "daemon 复用前检查 info.version === InstallationVersion，不匹配杀掉重启(packages/cli/src/services/daemon.ts:74-78,112-115)。stale 注册文件里的 PID 可能被复用，所以先认证健康再发信号(注释 daemon.ts:152-153)。SIGTERM → 50ms×100 轮询 → 复检 → SIGKILL(daemon.ts:90-107)。",
    "6_9_observed_inconsistencies": [
      "Electron 调 ['service','get','password'](packages/desktop/src/main/background-cli.ts:46)，但 CLI 声明的是 service password(packages/cli/src/commands/commands.ts:36-39, packages/cli/src/index.ts:24)，多出的 get 无对应子命令。",
      "openThemes 处理器发的是 session.list 而非主题选择器(packages/opencode/src/server/routes/instance/httpapi/handlers/tui.ts:51-54)，与 openSessions 完全相同。",
      "v2 TUI 启动器塞了 gracefulFetch：把 404 的 /config/providers、/provider、/agent、/config 伪造成空对象(packages/cli/src/tui.ts:22-27,36-45) — v1 TUI 接 v2 server 的兼容垫片，协议迁移期的直接税单。"
    ],
    "constants_table": [
      {
        "value": "4096(默认端口)",
        "coords": "packages/opencode/src/server/server.ts:121; packages/cli/src/commands/handlers/serve.ts:37",
        "premise": "v1: port=0 时先试 4096 再随机；v2: 4096 起逐个+1 至 65535。前提未知(无 issue/bench 依据)"
      },
      {
        "value": "SSE 心跳 10s / 15s",
        "coords": "packages/opencode/src/server/routes/instance/httpapi/handlers/event.ts:63; packages/server/src/handlers/event.ts:37",
        "premise": "两代不一致且无注释。前提未知，抄前需按自己的代理/NAT 空闲超时验证"
      },
      {
        "value": "subscriberCapacity = 256",
        "coords": "packages/server/src/handlers/event.ts:9",
        "premise": "每 SSE 连接 dropping queue 深度；溢出→整条流 fail。前提未知"
      },
      {
        "value": "重连退避 1000ms → 30000ms",
        "coords": "packages/tui/src/context/sdk.tsx:53-54,110-111",
        "premise": "指数 1000*2^(n-1)，封顶 30s"
      },
      {
        "value": "事件合批窗 16ms",
        "coords": "packages/tui/src/context/sdk.tsx:74-80",
        "premise": "距上次 flush <16ms 才排队否则立即 flush — 为一帧一次 Solid 渲染，注释在 76-77"
      },
      {
        "value": "worker shutdown 超时 5000ms",
        "coords": "packages/opencode/src/cli/cmd/tui.ts:203",
        "premise": "超时后直接 worker.terminate()"
      },
      {
        "value": "HTTP 优雅关停 1s / WS 关停 1s",
        "coords": "packages/opencode/src/server/server.ts:189; packages/opencode/src/server/routes/instance/httpapi/websocket-tracker.ts:36",
        "premise": "前提未知"
      },
      {
        "value": "daemon 健康检查 2000ms",
        "coords": "packages/cli/src/services/daemon.ts:69",
        "premise": "AbortSignal.timeout"
      },
      {
        "value": "daemon 轮询 50ms × 100",
        "coords": "packages/cli/src/services/daemon.ts:98-99,104-106,127-130",
        "premise": "启动等待与停止等待各 ≈5s 上限"
      },
      {
        "value": "daemon 密码 32 字节 base64url，文件 0o600",
        "coords": "packages/cli/src/services/daemon.ts:53,57",
        "premise": "temp + rename 原子写"
      },
      {
        "value": "session 列表窗口 30 天",
        "coords": "packages/tui/src/context/sync.tsx:163",
        "premise": "Date.now() - 30*24*60*60*1000。前提未知"
      },
      {
        "value": "sidecar 启动 60s / 停止 6s",
        "coords": "packages/desktop/src/main/server.ts:20-21",
        "premise": "Electron utilityProcess"
      },
      {
        "value": "createOpencodeServer 超时 5000ms",
        "coords": "packages/sdk/js/src/v2/server.ts:26",
        "premise": "靠 stdout 文本匹配判就绪"
      }
    ],
    "do_not_copy": [
      {
        "item": "模块级全局 TUI 控制队列",
        "coords": "packages/opencode/src/server/shared/tui-control.ts:12-13",
        "reason": "与多客户端目标直接矛盾，且生产无调用方"
      },
      {
        "item": "v1 SSE 的 Queue.unbounded",
        "coords": "packages/opencode/src/server/routes/instance/httpapi/handlers/event.ts:31",
        "reason": "无背压，慢客户端撑爆 server"
      },
      {
        "item": "ACP 为进程内通信开真 TCP",
        "coords": "packages/opencode/src/cli/cmd/acp.ts:25-31",
        "reason": "Server.Default().app.fetch 就在手边"
      },
      {
        "item": "靠 stdout 文本匹配判断 server 就绪",
        "coords": "packages/sdk/js/src/v2/server.ts:55-70",
        "reason": "日志格式一改就断；应用注册文件或父子 pipe 握手"
      },
      {
        "item": "默认无鉴权 + mDNS 自动 0.0.0.0",
        "coords": "packages/opencode/src/server/routes/instance/httpapi/middleware/authorization.ts:100-102 + packages/opencode/src/cli/network.ts:71-75",
        "reason": "抄 v2 的强制密码模型"
      },
      {
        "item": "gracefulFetch 式 404 伪造垫片",
        "coords": "packages/cli/src/tui.ts:36-45",
        "reason": "版本协商该显式做，不该靠状态码猜"
      }
    ]
  },
  "s7_portability_to_rust": {
    "copy_directly": [
      {
        "design": "传输可插拔，上层 client 零感知",
        "coords": "packages/opencode/src/cli/cmd/tui.ts:238-247 + packages/tui/src/context/sdk.tsx:25-32",
        "rust": "trait Transport { async fn call(..); fn events(..) -> impl Stream }，TUI 只持有 Arc<dyn Transport>"
      },
      {
        "design": "turn fiber 属于 server scope 而非请求",
        "coords": "packages/opencode/src/effect/runner.ts:88 + packages/opencode/src/session/run-state.ts:37",
        "rust": "tokio::spawn 到 session-owned JoinHandle，用既有 InterruptSignal 取消；绝不把 agent future 绑在连接 future 上。这是 UI 可随时断开重连的唯一必要条件"
      },
      {
        "design": "同步与异步两种提交语义并存",
        "coords": "packages/opencode/src/server/routes/instance/httpapi/handlers/session.ts:295-333",
        "rust": "ZCode 该只保留 async 语义 + 事件流，同步版由 client 侧组合"
      },
      {
        "design": "durable 事件 + after=<seq> 游标续传",
        "coords": "packages/core/src/event.ts:573-609, packages/server/src/handlers/session.ts:358-363",
        "rust": "顺序是关键：先注册 wake、再读 DB、live 段由 wake 触发再读"
      },
      {
        "design": "daemon 注册文件 + 健康认证 + 版本比对 + 自杀式互斥",
        "coords": "packages/cli/src/services/daemon.ts:40-41,64-78,110-131,159-177",
        "rust": "比端口扫描/PID 文件都稳；尤其先健康认证再对 PID 发信号(注释 152-153)避免 PID 复用误杀"
      },
      {
        "design": "命令 handler 全懒加载",
        "coords": "packages/cli/src/index.ts:11-27, packages/cli/src/framework/runtime.ts:63-77",
        "rust": "Rust 无模块解析开销，对应的是别在 CLI 入口初始化 provider/LSP/MCP；v1 的 serve.ts:12-13 instance:false 才是正解"
      },
      {
        "design": "单向依赖 Schema → Protocol/Core → Server",
        "coords": "packages/schema/AGENTS.md:7, 根 AGENTS.md:3",
        "rust": "zcode-schema crate 不依赖任何 runtime crate，用 CI ratchet 卡住"
      },
      {
        "design": "有序 allow/deny/ask ruleset + always 连锁放行 / reject 连坐",
        "coords": "packages/opencode/src/permission/index.ts:28-38,131-165",
        "rust": "findLast 语义(后写覆盖先写) + 回复一条自动结算同 session 其他 pending，直接可搬"
      }
    ],
    "ts_to_rust_semantic_gaps": [
      {
        "gap": "Deferred 阻塞工具 → Rust 用什么",
        "detail": "TS 侧 ask 直接 Deferred.await 挂住工具(packages/opencode/src/permission/index.ts:98-107)。Rust 对应 oneshot::Sender/Receiver：pending 表 Mutex<HashMap<PermissionId, oneshot::Sender<Reply>>>，工具侧 rx.await。oneshot::Receiver 在 sender drop 时返回 Err(RecvError)，正好对应 instance dispose → 全部 reject(permission/index.ts:56-63)，免费拿到正确语义别再包一层。注意 Effect.ensuring(..., pending.delete(id))(permission/index.ts:101-105)在 Rust 里是必须的 scopeguard/Drop，否则取消时泄漏条目。"
      },
      {
        "gap": "谁持有 session 状态",
        "detail": "Rust 必须定 InstanceStore = Arc<DashMap<PathBuf, Arc<Instance>>>，Instance 内含 CancellationToken 或既有 InterruptSignal。opencode 的 InstanceStore.load 用 Deferred 做并发去重(同目录并发请求只 boot 一次，packages/opencode/src/project/instance-store.ts:107-124)，Rust 对应 tokio::sync::OnceCell 或 Shared<BoxFuture>，不要用裸 HashMap::entry + .await(会跨 await 持锁)。"
      },
      {
        "gap": "事件流的 Send 边界与 Lagged 处理",
        "detail": "SSE handler 返回 Stream<Item=Bytes> + Send + 'static。Rust 用 BroadcastStream<Event>。opencode 的 dropping-256(packages/core/src/event.ts:152-161)对应 broadcast::channel(256) 的 RecvError::Lagged — Lagged 不该断流，应向客户端发 resync 事件让它按 after=<last_seq> 补拉。这是对 6.3 的直接修正。"
      },
      {
        "gap": "await request.text() 全量化",
        "detail": "Worker RPC 把 body 整体字符串化(packages/opencode/src/util/rpc.ts:8, packages/opencode/src/cli/cmd/tui.ts:28-33)。Rust 进程内传输应直接传结构体(enum Request/enum Response)走 mpsc，不要为了和 HTTP 一致而先序列化再解；只有跨进程边界才付序列化成本。"
      },
      {
        "gap": "关停顺序",
        "detail": "opencode 需 monkey-patch 才能表达优雅关+强杀两档(packages/opencode/src/server/server.ts:178-196)。Rust 用 axum::serve(...).with_graceful_shutdown(token.cancelled()) + 独立连接追踪天然分层，别照抄补丁思路。"
      }
    ],
    "boundary_conditions_checklist": [
      "事件订阅必须先于首帧发送与首次 DB 读(packages/opencode/src/server/routes/instance/httpapi/handlers/event.ts:29-30, packages/core/src/event.ts:575-586)。顺序错=丢事件。",
      "server.connected 首帧是客户端『该重新 bootstrap 了』的信号(handlers/event.ts:70, packages/tui/src/context/sync.tsx:172-174)；ZCode 必须定义等价物，否则重连后状态永久陈旧。",
      "instance dispose 要主动切断该目录的 SSE(handlers/event.ts:44-61)，否则客户端连着已死实例。",
      "权限 reply 三条连锁：reject 连坐同 session 全部 pending(packages/opencode/src/permission/index.ts:127-140)；always 追加规则后重算同 session 全部 pending 并批量放行(同文件:143-165)；每次结算都要发 permission.replied 让所有客户端同步移除 UI(同文件:115-119,132-137,155-160)。",
      "取消要级联到后台作业：cancel 先递归取消所有 metadata.sessionId/parentSessionId 指向本 session 的 background job，循环直到无新增(packages/opencode/src/session/run-state.ts:108-140)，再取消 runner。",
      "端口 0 的语义是『先试 4096 失败再随机』而非直接随机(packages/opencode/src/server/server.ts:117-122)。",
      "daemon 停止前必须先健康认证再对 PID 发信号(packages/cli/src/services/daemon.ts:152-153)。",
      "注册方要周期性自查注册文件归属，被顶替就自杀(packages/cli/src/services/daemon.ts:171-177)，finalizer 里只删属于自己的注册(同文件:178-184)。",
      "非编译构建(开发模式)下 daemon 复用要禁用(packages/cli/src/services/daemon.ts:114-115)，否则改了代码还连着老 daemon。",
      "mDNS 只在非 loopback hostname 下发布，且必须在 scope finalizer 里 unpublish(packages/opencode/src/server/server.ts:158-170)。"
    ],
    "ecosystem_specific_not_transferable": [
      {
        "point": "『起个 HTTP server 很便宜』在 Rust 里同样便宜(axum/hyper 编译进二进制，启动 <1ms)，但回环 HTTP 的序列化+系统调用+代理干扰成本(packages/web/src/content/docs/network.mdx:21-24)一分不少。结论：协议边界照抄，物理传输默认走进程内 channel，HTTP 只作可选的远程/多端出口。"
      },
      {
        "point": "OpenAPI → SDK 自动生成在 TS 生态几乎零成本(packages/httpapi-codegen, packages/sdk/js/script/build.ts)；Rust 侧要么手写 client crate，要么接受 utoipa + 外部 generator 的额外构建步骤。ZCode 若不做第三方 SDK 生态，这条不该抄 — 它是 opencode 架构复杂度的主要来源之一，收益全在生态侧。"
      },
      {
        "point": "Worker 线程=同进程隔离堆是 JS 特有的廉价隔离。Rust 没有也不需要：同进程直接 tokio::spawn，隔离靠类型与 CancellationToken，不必为模仿 opencode 的 Worker 引入子进程。"
      },
      {
        "point": "SQLite 事件溯源(packages/core/src/database/)在 Rust 用 rusqlite/sqlx 成本相当，但 opencode 为此付了 40 条 migration 的维护债(packages/core/src/database/migration/)。对照 jcode 的 snapshot + journal 方案再决定。"
      }
    ]
  },
  "s8_unknowns": [
    {
      "item": "opencode web 命令未读，可能是第三种 server 启动形态",
      "search_path": "packages/opencode/src/cli/cmd/web.ts"
    },
    {
      "item": "control-plane / workspace 远程执行只读了路由中间件(packages/opencode/src/server/routes/instance/httpapi/middleware/workspace-routing.ts:118-148 的 proxyRemote + Fence.wait)，未读 adapter 与 sync 协议",
      "search_path": "packages/opencode/src/control-plane/workspace-adapter-runtime.ts, packages/opencode/src/server/shared/fence.ts"
    },
    {
      "item": "v2 SessionExecution 的 local/remote 分叉(packages/server/src/routes.ts:57 的 [[SessionExecution.node, SessionExecutionLocal.node]])说明执行后端是可替换 layer，但 SessionExecutionRemote 是否存在未确认",
      "search_path": "packages/core/src/session/execution/"
    },
    {
      "item": "OPENCODE_FAST_BOOT 由谁设置：全仓 grep 只有 packages/tui/src/app.tsx:278 一处读取、无写入点(可能由 e2e/storybook 外部注入)。因此『它是测试用逃生门』是 [INFERENCE] 而非证实",
      "search_path": "已 grep 全仓 OPENCODE_FAST_BOOT，仅一处命中"
    },
    {
      "item": "已关闭：客户端是否用 session.events?after= 游标续传 — 对 packages/app/src/context 与 packages/tui/src/context 定向 grep(session.events|sessionEvents|after 型 seq)零命中，确认无客户端使用游标续传",
      "search_path": "packages/app/src/context; packages/tui/src/context"
    },
    {
      "item": "与 ZCode 既有契约的冲突：未发现。rule://zcode-architecture 的 worker 重入 CLI 入口约定与 opencode 的 new Worker(worker.ts) 是不同机制(opencode 用独立入口文件而非 argv 重入，见 packages/opencode/src/cli/cmd/tui.ts:52-58 的 target())，但那是 ZCode 已定契约，本报告不提议改动",
      "search_path": "n/a"
    }
  ]
}
```
