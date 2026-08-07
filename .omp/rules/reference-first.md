---
description: 参考优先：非平凡实现动手前必须先调研 oh-my-pi / jcode / opencode 三个参考仓的对应实现并移植，而不是凭空设计。含三仓坐标、调研流程、移植规则、已探明线索表。新写子系统 / 选常量阈值 / 选并发模型 / 选第三方 crate 前必读。
---

# 参考优先：先调研，再实现

ZCode 是从零起步的 harness，绝大多数功能都已经有人在生产里做过一遍。
**默认动作是去读别人怎么做的，然后移植；不是自己想一个。**
凭空发明出来的设计，代价是本仓要独自承担所有边界条件的发现成本。

## 三个参考仓库

本机绝对路径（换机器时先确认存在；不存在就说明调研做不了，不要默默跳过）：

| 仓库 | 路径 | 性质 |
| --- | --- | --- |
| oh-my-pi | `C:/Users/zero/Desktop/code/github/oh-my-pi` | TypeScript/Bun。**你当前正在其中运行的 harness**。工具层参数经真实会话数据反事实调优（`scripts/session-stats/`），语义最贴近实际使用体感 |
| jcode | `C:/Users/zero/Desktop/code/github/jcode` | Rust，81 个 workspace 成员，ratatui TUI。**唯一同语言参考**：类型、并发原语、TUI 调度可直接移植 |
| opencode | `C:/Users/zero/Desktop/code/github/opencode` | TypeScript/Bun。分层与契约设计最完整：Schema→Protocol→Server 单向依赖、权限有序 ruleset、事件耐久化 |

**不预设哪家是某个子系统的权威。** 每次都三家都看，当场比较后取舍。
上游各自有各自的历史包袱，"jcode 是 Rust 所以听它的"是错误的默认。

### 许可

三仓均 MIT。**不需要逐处出处注释，不需要在提交信息里记录来源**——日常借鉴按这个来。
但 MIT 要求的是 "copies or substantial portions" 都保留版权与许可声明
（`<仓库>/LICENSE:12-14`），**改写并不自动免除**：判据是有没有把对方的实质性实现搬过来，
不是代码是否逐行一致。命中就把该仓的版权行与 MIT 全文集中记进根 `THIRD-PARTY-NOTICES.md` (planned)，
一仓一条即可，代码里不用标。只拿走思路、常量、接口形状而自己写实现的，不算。

## 什么时候必须先调研

满足任一即为**非平凡**，动手前必须调研：

- 新增子系统、新工具、新协议、新持久化格式；
- 涉及并发、取消、缓存、截断、节流、重试、背压等**有边界条件**的逻辑；
- 需要选定常量阈值（超时、上限、页大小、帧率、比例）；
- 需要引入第三方 crate 或决定自研。

不必调研：改错别字、补测试、重命名、纯本仓内部重构、修明确的 bug。

## 调研怎么做

- 用 `/ref <主题>` 派 `reference-scout`，它会并行读三仓并产出对照表与移植方案。
- 三仓全部要看。某仓没有对应实现，也是一条结论——**写出来**，不要沉默跳过。
- 每条结论必须带 `<仓库>/<相对路径>:<行号>`。没有行号的结论不算结论。
- 结论要包含"它为什么这么做"和"代价是什么"；只有"它这么做了"等于没调研。

## 移植规则

1. **先读懂再抄。** 抄不懂的代码等于抄进来一个你无法维护的 bug。
   移植前必须能回答三问：这段为什么这么写、边界条件是什么、失败时的行为是什么。
   答不上来就继续读，不要先合进来再说。

2. **常量必须连同它成立的前提一起抄。**
   例：oh-my-pi 的 read 默认 300 行 / 硬顶 3000 行 / 50KB
   （`packages/coding-agent/src/config/settings-schema.ts:3258`、`packages/coding-agent/src/session/streaming-output.ts:10-12`）
   是在 `tokens + 250*calls + 100000*extra_truncations` 这个目标函数下扫出来的
   （`scripts/session-stats/read_optimizer.py:560-570,708-712`）。
   换了目标函数这个数就不成立。抄数值时把前提写进代码注释；**不写出前提的魔数按缺陷处理**。

3. **TS→Rust 不是逐行翻译。** 上游靠 GC、Promise、结构化克隆成立的写法，在 Rust 里要重新决定
   谁持有、谁借用、在哪 `.await`、哪些状态要 `Send`。
   翻译结果里 `Arc<Mutex<_>>` 满天飞，说明翻的是语法不是语义，重来。

4. **性能取舍要么显式继承，要么显式推翻。**
   参考实现里每一处取舍都有代价（见线索表）。照抄就要知道代价并接受；不抄就要写明本仓为什么不适用。
   "看起来更简单所以简化了"不是理由。

5. **不抄参考仓的已知技术债。** 上游自己文档里承认的坑不要搬。
   例：jcode 的 TUI 用进程级全局渲染状态，导致测试必须 `--test-threads=1`
   （`docs/TUI_TEST_FLAKINESS.md`）——那是债，不是设计。
   同理 jcode `docs/dev/crate-splitting-plan.md:56-61` 自己反对的"每文件一 crate"。

## 已探明的线索（起点，不是结论）

以下落点已核实，用来**省掉定位时间**，不替代当场核对。路径相对各仓根目录。

| 主题 | 落点 |
| --- | --- |
| agent turn 循环、工具串行/并行 | jcode `crates/jcode-app-core/src/agent/turn_streaming_mpsc.rs:1220-1245`（默认串行）、`crates/jcode-app-core/src/tool/batch.rs:10,191-260`（batch 工具内 `MAX_PARALLEL=10`） |
| 取消 / 中断原语 | jcode `crates/jcode-agent-runtime/src/lib.rs:20-106`（`InterruptSignal` = AtomicBool + epoch + Notify，**没用** `CancellationToken`；epoch 是为了 Esc 不丢 wakeup） |
| provider 抽象与流式 | jcode `crates/jcode-provider-core/src/lib.rs:72-99`（`EventStream = Pin<Box<dyn Stream<Item=Result<StreamEvent>> + Send>>`） |
| 模型目录 | jcode `crates/jcode-provider-core/src/models.rs:13-82`（手写常量）、`crates/jcode-provider-metadata/src/catalog.rs:6-96`（静态 profile + 运行时可刷新） |
| 会话持久化 | **已定：JSONL 条目树**，见 `plans/runtime-boundary/README.md` 第 7 节第 8 条与 `crates/agent/src/session/`。抄源 oh-my-pi `packages/coding-agent/src/session/session-entries.ts:58-62,245-260`；jcode `crates/jcode-base/src/session/persistence.rs:66-125`（坏行只跳该行）仍然照抄；消息 tagged union 契约参考 opencode `packages/schema/src/session-message.ts:12-212` |
| 上下文压缩 | jcode `crates/jcode-compaction-core/src/lib.rs:6,9,13,19,43,58,63`（200k 预算、0.80 触发 / 0.95 紧急、保留近 10 turn、chars÷4 估 token（函数体 `:389-400`）、图像定额 1600、系统开销 18k） |
| 工具输出统一截断 | oh-my-pi `packages/coding-agent/src/session/streaming-output.ts:10-12`（3000 行 / 50KB / 512 列 + artifact 外溢） |
| read 工具分页与结构摘要 | oh-my-pi `packages/coding-agent/src/tools/read.ts:148-164,420-421,2699-2701`、`packages/coding-agent/src/config/settings-schema.ts:3258-3350` |
| grep 分页与大文件窗 | oh-my-pi `packages/coding-agent/src/tools/grep.ts:91-126`（20 文件 × 20 匹配、4MiB 窗、30s 超时）；引擎进程内而非 fork `rg` |
| bash 工具边界 | jcode `crates/jcode-app-core/src/tool/bash.rs:26-27,539-543,742,884-937`（30000 **字节**头部截断、默认 120s / 上限 600s 钳制、超时不杀而是把持有 `Child` 的 `JoinHandle` 交给 background manager 收养）+ `crates/jcode-app-core/src/agent/tools.rs:5-19`（入历史前二次 512KiB cap）。**已知债，不要抄**：前台路径没有 setpgid（对比后台 `bash.rs:1113-1122`），取消时只杀 `bash -c` 本身，孙进程成孤儿；且同一个 `timeout` 字段在前台/后台语义相反（`bash.rs:1161-1214`） |
| 权限模型 | **已定：tier × policy + opencode 式回环**，见 `plans/runtime-boundary/README.md` 第 7 节第 10 条与 `crates/agent/src/approval.rs`。裁决表抄 oh-my-pi `packages/coding-agent/src/tools/approval.ts:29-185`；回环（always 连锁 / reject 连坐 / 每次结算广播）抄 opencode `packages/opencode/src/permission/index.ts:98-167`。**不要再实现有序 allow/deny/ask ruleset**（`:28-38`），本仓已显式否决 |
| TUI 重绘节流 | jcode `crates/jcode-tui/src/tui/redraw_schedule.rs:16-20,211-214`（idle 250ms / deep-idle 5s / spinner 80ms / resize debounce 33ms） |
| TUI 缓存 | jcode `crates/jcode-tui-messages/src/cache.rs:8-59`（消息行 LRU 2048）、`crates/jcode-tui/src/tui/ui.rs:912-916`（BodyCache 8 条 / 32MiB） |
| 流式 reveal 节奏 | jcode `crates/jcode-tui-core/src/stream_buffer.rs:30-51`（到达≠显示，180→960 cps）；oh-my-pi 50ms 批 / ≤20 updates·s⁻¹，文本 reveal 30fps |
| 终端能力与平台坑 | jcode `docs/TERMINAL_CAPABILITIES.md`（ConPTY 延迟、BCE 渗色、emoji 宽度不一致） |
| crate 切分与编译 profile | jcode `docs/dev/crate-splitting-plan.md:3-20`、`docs/plans/COMPILE_PERFORMANCE_PLAN.md:45-48`（按**重编译易变性**切，不按目录）、根 `Cargo.toml:269-289`（release opt-level=1 + 热包钉 opt-level=3 + 单独 `release-lto`） |
| 依赖方向强制 | jcode `scripts/check_dependency_boundaries.py:3-51`（types 禁依赖 runtime）+ CI ratchet；opencode `packages/schema/AGENTS.md:7-8`（Schema→Core/Protocol→Server 单向） |
| worker / 子进程模型 | oh-my-pi `packages/coding-agent/src/cli.ts:125-132`（`__omp_worker_*` argv 重入同一入口）；jcode **无此模式**，用同进程 `tokio::spawn` + 完整 CLI 子会话（`crates/jcode-app-core/src/session_launch.rs:255-259`） |
| 本机 IPC 端点命名 / socket 路径长度 | **三仓一致：密钥绝不进端点名，且靠"名字里无变长成分"天然避开 `sun_path` 上限（macOS 104 / Linux 108），无一处显式长度处理。** oh-my-pi `packages/coding-agent/src/launch/paths.ts:7-14`（项目目录 wyhash 进**目录段**，文件名固定 `broker.sock`）+ `client.ts:77-95`（令牌单独放 0600 的 `broker.token`）；jcode `crates/jcode-app-core/src/server/socket.rs:7-24`（全固定字面量，生产零随机）；opencode 不用 unix socket，回环 TCP + `packages/cli/src/services/daemon.ts:164-173` 的 `server.json` 注册文件。唯一平台规避：oh-my-pi 对 DAP 在 macOS 上改回环 TCP（`packages/coding-agent/src/dap/client.ts:217-233`，因路径由第三方接口决定）。**已知债不要抄**：jcode `crates/jcode-base/src/registry.rs:199-211` 与 `browser.rs:83-85` 让不限长的名字直入 socket 文件名 |
| 就绪（ready）握手 | 三种形态，代价各异。jcode `crates/jcode-app-core/src/server/socket.rs:236-333`：`libc::pipe` + `JCODE_READY_FD` 传 fd 号，子进程写 1 字节；语义最干净但只有 Unix、要 `unsafe`。oh-my-pi **无就绪信号**，客户端 spawn 后 10s 内每 50ms 重试 connect（`launch/client.ts:303-316`）；托管子 daemon 才用 stdout banner 正则 ∧ 端口可连（`broker.ts:861-912`），LSP mux 进一步要求协议 ping 往返（`lsp/mux/daemon.ts:203-220`）。opencode 用 stdout 文本匹配（`packages/sdk/js/src/v2/server.ts:55-70`）—— **反例**：v2 CLI 换了 banner 格式后 SDK 的前缀匹配就失配了。本仓已定：socket + 一次性令牌，见 `crates/utils/src/daemon.rs` 的 `ReadyChannel` |

## 自研的门槛

三仓都没有对应实现时才自研。交付时必须写出：查了哪些路径、为什么都不适用、
自研方案的边界条件是什么。缺这三项的自研按未完成处理。

## 什么算"产品走向"，必须问用户

调研完成后先分类再裁决。命中任一即为产品走向级，**摆选项给用户，答复前不裁决、不实现**：

- 改变用户可见行为或核心交互（渲染形态、命令语义、默认开关）；
- 改变数据模型或持久化格式（会话结构、落盘布局、迁移代价）；
- 改变权限与安全边界（审批粒度、沙箱、凭据存放）；
- 改变发布形态（二进制数量、平台矩阵、默认 feature 的体积代价）；
- 与下节所述既定契约冲突。

提问给 2–4 个选项，每个写清**功能代价与性能代价**，标出推荐及理由。
其余一律是实现细节：自己按性能优先裁决，不要来回请示。

## 与既有契约的关系

本规则只管**新东西怎么定**。已在本仓落盘、并写进 `rule://zcode-architecture` 的契约
（worker 重入 CLI 入口、prompt 存静态 `.md`、生成物禁改）与 `plans/tui/` 的设计决策，
是既定事实源：**不因为某个参考仓做法不同就自动推翻**。
要改先单独提出并拿到确认，不要在实现某个功能的顺路上改掉。
