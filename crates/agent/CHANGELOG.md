# Changelog

格式与归属标注规则见 `rule://zcode-workflow`。

## [Unreleased]
### Breaking Changes

- 项目许可证从 `MIT OR Apache-2.0` 切换为 `AGPL-3.0-only`。
- `AgentRuntime::new` 新增 `cancels: Arc<CancelRegistry>` 参数（在 `store` 与 `config` 之间）。
  取消注册表**跨会话共享**：取消请求只带 session id，必须能从同一张表里找到目标信号。
  `TurnGuard` 相应改为只复位软取消信号，硬取消信号的复位归 `TurnRegistration::drop`，
  让每个信号只有一处复位点。

### Added

- 初始化 crate 骨架：继承 workspace 元数据与 lint 配置。
- `id`：`SessionId` / `EntryId`，`(毫秒 << 12 | 序号)` 装进一个 `AtomicU64`，
  字典序即时间序、进程内严格单调；同毫秒超过 4096 个自动向下一毫秒借位，
  不会像上游那样溢出污染时间位。
- `interrupt::InterruptSignal`：`AtomicBool` + epoch + `Notify`。同步可读、异步可等、
  延迟复位受 epoch 保护。刻意不用 `CancellationToken`——它给不了"先注册 waiter 再查 flag"
  与"reset 不得抹掉并发 fire"这两条语义。
- `event`：`AgentEvent` / `EventSink` / `EventStream`。慢消费者收到
  `AgentEvent::Resync { dropped }` 并**继续消费**，流不断；持久化不走这条通道。
- `session`：落盘态消息模型（与 `zcode-ai` 传输态互转）、条目树、JSONL 存储。
  `parent_id` 构成树，当前上下文 = 根到 head 的路径，`/branch` 与 `/rewind` 只是换 head。
  读取时坏行只跳该行并告警，不中断加载。`context()` 应用压缩且保证压缩切点不留孤儿
  `tool_use` / `tool_result`。
- `tool`：`Tool` trait（owned 参数，可 `tokio::spawn`）、`ToolRegistry`
  （注册期编译 schema、定义按名排序以命中 prompt 缓存、未知工具名三级 fuzzy 建议）、
  `schedule::execute_batch`（`Shared`/`Exclusive` 屏障链 + `MAX_SHARED_PARALLEL = 10` 限流，
  工具 panic 不毒化整批）。并发与审批的默认值都倒向保守侧：`Concurrency::Exclusive`、
  `Tier::Exec`。
- `approval`：tier × policy 裁决（默认 `ApprovalMode::Yolo`）+ 询问回环
  （`always` 连锁、`reject` 连坐、每次结算广播、`pending()` 供重连补拉）。
  `always` 的授权键是 `(工具名, 工具声明的作用域)`，工具可收窄使"总是允许"不等于放行整类工具。
- `context`：token 估算（图片按固定价而非 base64 长度）、Anthropic 分离计账与 OpenAI
  子集计账两套用量口径、压缩决策取 `max(本地估算, 提供商回报)`、切点落在工具配对边界。
- `turn`：turn 循环。助手条目 id 在开流前预分配，`MessageStart`、每条增量、落盘、
  `MessageEnd` 共用同一个 id。参数不合 schema、工具名不存在、审批被拒、取消——全部落成
  `is_error` 的工具结果喂回模型而不是错误路径；**每个 `tool_use` 恒有配对的 `tool_result`**。
  上下文超限自动压缩重试。压缩提示词存 `src/prompts/compaction.md`，`include_str!` 导入。
- `cancel::CancelRegistry`：会话 → 在飞 turn / 后台作业的中断信号表。取消请求只带 session id
  过来，必须从一张表里找到该会话所有信号；同一 session 可能有多个并发 turn，值因此是集合。
  取消**先递归打完后台作业、再打 runner**，且循环到一轮无新增才停——作业从收到取消到退出
  之间还能派生新作业，一次快照会漏掉它们并留下脱管进程。作业可声明自己拥有的子会话，
  取消沿它递归。级联触顶（64 轮）时 runner 照打（它是新作业的唯一来源），
  但 `CancelReport::cascade_exhausted` 会告诉调用方这次取消不干净。
  与 jcode 的两点不同：**不是进程级 `static`**（否则同名 session 的测试无法并行）；
  **登记后台作业**（jcode 只登记 turn）。
- `stdin::StdinGate`：工具执行途中要一行 stdin 时的 oneshot-by-request-id 回环，
  形状对齐 `ApprovalGate`。待回答状态挂在 session 上，断连不作废、重连可用 `pending()` 补拉；
  jcode 把 oneshot 存在每连接的 map 里，连接一断工具侧立刻收 `Err` 而子进程还卡在读 stdin。
- `AgentEvent::StdinRequested` / `StdinResolved`。


### Fixed

- 硬取消覆盖 provider 交互的**每一个**可挂起点：建流（`stream()` 未返回前）、
  逐帧消费、压缩请求的建流与消费、以及等待审批。此前只在两帧之间查取消位，
  提供商停摆或审批无人应答时 turn 会永久挂死。流消费收敛成唯一的 `drain_stream`——
  取消并等最初只写在主流程上、压缩那条流漏改，同构漏洞出现过两次。
  四个可挂起点各有一条 `tokio::time::timeout` 回归测试，且都用"临时退回旧写法必失败"
  验证过它们不是空转。
- rustdoc 在 `-D warnings` 下报的文档链接问题：`context.rs` / `id.rs` / `tool/registry.rs` /
  `turn.rs` 中指向私有项（`COMPACTION_THRESHOLD_PERCENT`、`DEFAULT_CONTEXT_WINDOW`、
  `safe_cutoff`、`next_stamp`、`MAX_SUGGESTIONS`、`StreamAccumulator`）的 intra-doc
  链接降级为代码 span。