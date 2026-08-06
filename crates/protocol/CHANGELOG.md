# Changelog

格式与归属标注规则见 `rule://zcode-workflow`。

## [Unreleased]

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
