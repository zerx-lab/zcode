//! `zcode-ai`：多提供商 LLM 客户端：请求构造、流式解码、重试与限流。
//!
//! 注意导入边界：本 crate 只 re-export 自身签名用到的模型/effort **类型**；
//! 模型目录的**值**一律从 `zcode_catalog` 直接导入（见 `rule://zcode-architecture`）。
//!
//! 职责边界见 `rule://zcode-architecture` 的 crate 职责表。当前只落了 crate 骨架，尚无公开 API。
