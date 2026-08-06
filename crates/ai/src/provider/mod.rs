//! 提供商适配器。
//!
//! 每个适配器把 [`CompletionRequest`] 翻成自家线格式，再把 SSE 响应翻回
//! [`StreamEvent`]。共用的 HTTP、SSE、错误分类在 [`crate::http`] 与
//! [`crate::sse`]，适配器里只放**该家独有**的东西。
//!
//! xAI 没有独立适配器：它的两条线路分别复用 Chat Completions 与 Responses，
//! 差异（base URL、缓存亲和头、`reasoning.summary` 强制为空、effort 允许名单）
//! 由 [`openai_chat::ChatConfig`] / [`openai_responses::ResponsesConfig`] 承载。

pub mod anthropic;
pub mod openai_chat;
pub mod openai_codex;
pub mod openai_responses;
pub mod xai;

mod block;

use std::pin::Pin;

use async_trait::async_trait;
use futures_core::Stream;

use crate::error::AiError;
use crate::types::{CompletionRequest, ProviderId, StreamEvent};

/// 流式事件流。
pub type EventStream = Pin<Box<dyn Stream<Item = Result<StreamEvent, AiError>> + Send>>;

/// 一家提供商的推理适配器。
#[async_trait]
pub trait Provider: std::fmt::Debug + Send + Sync {
    /// 归属的提供商。
    fn id(&self) -> ProviderId;

    /// 发起一次流式补全。
    ///
    /// 返回的流一定以 [`StreamEvent::Done`] 收尾，除非中途产出错误项。
    async fn stream(&self, request: &CompletionRequest) -> Result<EventStream, AiError>;
}
