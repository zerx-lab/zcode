# Changelog

格式与归属标注规则见 `rule://zcode-workflow`。

## [Unreleased]
### Breaking Changes

- 项目许可证从 `MIT OR Apache-2.0` 切换为 `AGPL-3.0-only`。

### Added

- crate 建立。它是客户端与 agent 运行时之间的**唯一**编译边界：依赖方向为
  `tui -> protocol <- runtime`，所有 wire 类型归本 crate 所有。
- `version`：`Version` / `PROTOCOL_VERSION` / `Hello` 握手。协商规则是主版本必须相等、
  生效 `minor` 取两端较小者；加性变更 bump `minor`，破坏性变更 bump `major`。
- `envelope`：`Envelope<T>`（`v` / `id` / `reply_to` / `payload`）与 `IdGen`。
  每条带 `id` 的请求必须收到恰好一帧 `reply_to` 指向它的回应。
- `error`：`ProtocolError` 与 `ErrorCode`。未知 `Event` 可静默跳过，未知 `Request`
  **必须**回 `ErrorCode::UnsupportedRequest` —— 跳过会让请求方永久挂着。
- `frame`：NDJSON `encode` 与 `FrameDecoder`。解帧器四个约束一体：缓冲区跨调用持久、
  扫描游标避免 $O(n^2)$ 重扫、帧长上限 256 MiB、大帧后容量回缩。单行 JSON 损坏只跳这一行。
- `wire`：payload 变体全套。`ClientFrame` / `ServerFrame` 用 `kind` 内部 tag 包住
  `Request`（16 变体）/ `Reply`（10）/ `Event`（19），内层各用 `type`，线上是一层扁平对象。
- `wire::types`：会话条目树的领域投影（`Entry` / `EntryKind` / `Message` / `Usage` /
  `SessionSummary` / `PendingApproval` / `PendingStdin` 等）。与 `zcode-agent` 的落盘类型
  **是两套形状**，互转是 host adapter 的职责——直接转出运行时类型会让每个客户端为了
  反序列化而依赖整个运行时。
- 请求只有客户端 → 运行时一个方向。需要用户回答的事（审批、stdin）走
  "推 `*Requested` 事件 + 客户端主动回请求 + `PendingList` 随时重拉"，**待回答状态挂在
  session 上而不是连接上**：opencode 的权限 pending 只在内存且重连不重拉、jcode 的 stdin
  oneshot 存在每连接的 map 里，两个缺陷同源。
- `Subscribe` 携带接管仲裁三元判据（`client` 实例 id、`has_local_history`、`takeover`）
  与**独立的**载荷游标 `since`。两者职责不同：前三个决定"能不能接管"，后者决定"回多少条目"。
  jcode 把它们混成一个 `client_has_local_history` 布尔，结果那个布尔并不裁剪 History 载荷。
- `RawEnvelope` 与 `FrameProbe`：payload 解析失败时仍能拿到帧序号回一帧带 `reply_to` 的
  错误。没有这一步，"未知请求必须收到回应"就是句空话。
- 握手改为**三帧双向挑战应答**（`ClientHello` → `ServerHello` → `ClientAuth`），新增
  `Nonce` / `Proof` / `ErrorCode::Unauthorized`。客户端首帧不带任何凭据：Windows named pipe
  没有文件权限模型，任何本机进程都能抢名占坑，明文 bearer 一次连接就被收走。密钥与 HMAC
  计算在 `zcode-utils` 的 `daemon` 模块，协议层只搬运不透明字符串。
- `tests/wire_schema.rs` + `tests/wire-schema.json`：协议形状快照 + 三个穷尽性哨兵
  （变体计数与逐行 `match` 双保险）。刷新快照用
  `ZCODE_UPDATE_WIRE_SCHEMA=1 cargo nextest run -p zcode-protocol`。

### Fixed

- rustdoc 在 `-D warnings` 下报的文档链接问题：`wire/request.rs` 里 `RawEnvelope` 的
  intra-doc 路径写成了 `crate::envelope::RawEnvelope`，该类型实际住在 `crate::wire`
  （`wire/mod.rs` 的类型别名），两处链接改为 `crate::wire::RawEnvelope`。
