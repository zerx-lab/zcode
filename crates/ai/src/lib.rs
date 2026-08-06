//! `zcode-ai`：支持流式传输的多提供商 LLM 客户端。
//!
//! 三条推理线路 + 三套登录流程：
//!
//! | 提供商 | 线格式 | 鉴权 |
//! | --- | --- | --- |
//! | `anthropic` | Messages API | API key 或 Claude Code OAuth |
//! | `openai` | Chat Completions / Responses | API key |
//! | `openai-codex` | Codex Responses（`chatgpt.com/backend-api`） | ChatGPT 订阅 OAuth |
//! | `xai` | Chat Completions | API key |
//! | `xai-oauth` | Responses | SuperGrok 设备码 OAuth |
//!
//! 统一词汇在 [`types`]：请求用 [`CompletionRequest`]，响应是一串
//! [`StreamEvent`]。适配器负责翻译，上层不感知线格式差异。
//!
//! ```no_run
//! # async fn demo() -> Result<(), zcode_ai::AiError> {
//! use std::sync::Arc;
//! use futures_util::StreamExt as _;
//! use zcode_ai::{AuthStore, CompletionRequest, Message, Provider, ProviderId, StreamEvent};
//! use zcode_ai::provider::anthropic::AnthropicProvider;
//!
//! let auth = Arc::new(AuthStore::discover()?);
//! let provider = AnthropicProvider::new(auth)?;
//! let request = CompletionRequest::new("claude-sonnet-4-6", vec![Message::user("你好")]);
//!
//! let mut stream = provider.stream(&request).await?;
//! while let Some(event) = stream.next().await {
//!     if let StreamEvent::TextDelta { delta, .. } = event? {
//!         print!("{delta}");
//!     }
//! }
//! # let _ = ProviderId::Anthropic;
//! # Ok(())
//! # }
//! ```

pub mod auth;
pub mod error;
pub mod http;
pub mod provider;
pub mod sse;
pub mod types;

pub use crate::auth::AuthStore;
pub use crate::auth::credential::*;
pub use crate::error::*;
pub use crate::provider::{EventStream, Provider};
pub use crate::types::*;
