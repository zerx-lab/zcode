# Changelog

格式与归属标注规则见 `rule://zcode-workflow`。

## [Unreleased]

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


### Fixed

- 硬取消覆盖 provider 交互的**每一个**可挂起点：建流（`stream()` 未返回前）、
  逐帧消费、压缩请求的建流与消费、以及等待审批。此前只在两帧之间查取消位，
  提供商停摆或审批无人应答时 turn 会永久挂死。流消费收敛成唯一的 `drain_stream`——
  取消并等最初只写在主流程上、压缩那条流漏改，同构漏洞出现过两次。
  四个可挂起点各有一条 `tokio::time::timeout` 回归测试，且都用"临时退回旧写法必失败"
  验证过它们不是空转。