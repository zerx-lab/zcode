# Changelog

格式与归属标注规则见 `rule://zcode-workflow`。

## [Unreleased]

### Added

- 初始化 crate 骨架：继承 workspace 元数据与 lint 配置。
- 统一类型模型：`Message` / `Tool` / `CompletionRequest` / `StreamEvent` / `Usage` / `StopReason`，
  各提供商适配器只负责在它与自家线格式之间翻译。
- SSE 解码器：按 WHATWG 规范处理 `event` / `data` / 注释与跨 chunk 分片。
- 共享 HTTP 层：单例 `reqwest` 客户端、错误信封解析、`retry-after` 解析、
  以及限流与订阅额度耗尽的区分。
- 凭据存储：单 JSON 文件 + 排他文件锁，读-改-写全程互斥；OAuth 刷新走
  compare-and-swap，槽位被清空或换成别的凭据时拒绝写入，不让旧凭据复活。
- OAuth 登录：Anthropic（授权码 + PKCE，固定 54545 端口）、OpenAI Codex
  （授权码 + PKCE 固定 1455 端口，另有设备码流程）、xAI（RFC 8628 设备码 +
  OIDC discovery，token 端点强制校验仍在 `*.x.ai` 域下）。
- 本地回调服务器：CSRF `state` 校验、伪造回调不中断等待、浏览器回连不到本机时
  与手工粘贴通道竞速。
- Anthropic Messages 适配器：API key 与 Claude Code OAuth 两条鉴权分支
  （beta 列表、指纹头、身份 system 首块、`max_tokens` 夹取、空 `tools` 数组），
  以及 4 个上限的缓存断点分配。
- OpenAI Chat Completions 适配器：工具调用增量按 `index` / `id` / 数组位置三级对齐，
  推理字段多别名兼容，usage 扣除缓存命中。
- OpenAI Responses 适配器：扁平 function 工具、`input_text` / `output_text` 分工、
  `reasoning` item 原样回放以保住 `encrypted_content`。
- OpenAI Codex 适配器：落 `chatgpt.com/backend-api/codex/responses`，
  带 `chatgpt-account-id` / `originator` / `version` / `OpenAI-Beta`，
  并屏蔽后端会拒收的 `max_output_tokens` 与采样参数。
- xAI 适配器配置：API key 走 Chat Completions、SuperGrok OAuth 走 Responses，
  `x-grok-conv-id` 缓存亲和头、`reasoning.summary` 强制省略、
  `reasoning_effort` 按模型前缀白名单下发。
- `AiError::is_context_overflow()`：判别"上下文超出模型窗口"。三家都把它归在 400
  `invalid_request` 之下、没有独立状态码（OpenAI 给 `code: "context_length_exceeded"`，
  Anthropic 只在 message 里写 `prompt is too long`），因此只能靠 `code` / `message` 匹配。
  判别集中在错误类型上而不是各调用点，避免同一套匹配散落成多份各自漂移；
  `zcode-agent` 的 turn 循环据此决定压缩后重试而不是直接失败。

### Fixed

以下问题在落盘同轮的对抗式审查中发现并修掉，均有回归测试：

- Responses 的 reasoning 签名改为覆盖写入。上游对同一个 item 会先后发
  `output_item.added` 与 `.done`，原先的追加写法会拼出非法 JSON，导致每一轮历史
  回放都静默丢掉 `encrypted_content`，无状态多轮思考链完全失效。
- Anthropic 的连续工具结果合并进同一条 user 消息。并行工具调用时拆成多条会触发
  `tool_use ids were found without tool_result blocks immediately after`。
- Anthropic 的 `redacted_thinking` 独立成块（新增 `StreamEvent::RedactedThinking`），
  回放时编回 `{"type":"redacted_thinking","data":…}`；此前被塞进普通 thinking 块，
  服务端签名校验必失败。该事件**不发** start/end：内容一次到齐，发了 start 却没有
  配对的 end 会让按生命周期维护块的消费者留下永不关闭的思考块。`StreamEvent` 的
  配对不变量已写进类型文档，并有断言完整事件序列的回归测试。
- `is_error` 的工具结果不再内联图片，改为挂到 `tool_result` 块之后——Anthropic
  规定失败结果的 content 只能是文本。
- `StreamEvent::ToolCallStart` 带上调用 id 与工具名（此前恒为空串），消费者不必等到
  参数流完才能显示"正在调用 X"。
- Chat Completions 的工具结果图片改为紧随一条 user 消息送出，此前被替换成文本占位，
  视觉信息全丢。
- `reasoning_content` 改为按端点开关下发，默认关闭：OpenAI 与 xAI 的原生接口都不
  定义该字段。
- SSE 解码遵循 WHATWG 规范，data 缓冲为空时不派发事件；此前网关的
  `event: ping\n\n` 保活会让下游按空串解 JSON 而中断整条流。
- 开块不再产生 `delta: ""` 的空事件；Responses 的助手历史保持文本与工具调用的原始
  先后顺序。
