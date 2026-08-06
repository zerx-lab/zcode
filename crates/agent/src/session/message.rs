//! 落盘态消息模型，以及它与 [`zcode_ai`] 传输态消息的互转。
//!
//! # 为什么要两套类型
//!
//! `zcode_ai::Message` 是**发给提供商的形状**，不带 serde、不带 id、不带用量。落盘态额外背
//! 三样东西：条目 id（UI 要按 id 定位并原地更新）、`display_role`（UI 角色与 API 角色解耦）、
//! `usage`（成本与压缩决策要读）。这三样**永远不进提供商请求**——它们一旦混进请求体，
//! prompt 缓存的前缀就会随 UI 状态漂移。
//!
//! 抄源 jcode `crates/jcode-session-types/src/lib.rs:228-278`（`StoredMessage` /
//! `to_message()` 的分离）。opencode 用 `Message` + `Part` 两层
//! （`packages/schema/src/session-message.ts:121-213`），本仓不抄那一层：两层的收益是
//! "按 part 增量更新 UI"，而本仓的增量更新走事件流（[`crate::event`]）而非落盘结构，
//! 多一层只会让 provider 请求体的转换多一次分配。
//!
//! # 不变量
//!
//! 每个 [`StoredAssistantContent::ToolCall`] 都必须有一条 `tool_call_id` 匹配的
//! [`StoredMessage::ToolResult`] 紧随其后。这是提供商的硬约束而非本仓的偏好：缺配对的
//! `tool_use` 会让后续每一次请求都 400。压缩切点、中断补位、崩溃恢复三条路径都必须维持它。

use serde::{Deserialize, Serialize};
use zcode_ai::{
    AssistantContent, ImageContent, Message, StopReason, ThinkingContent, ToolCall,
    ToolResultContent, Usage, UserContent,
};

use crate::id::EntryId;

/// 图片：base64 内联，与 [`zcode_ai::ImageContent`] 同形。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredImage {
    /// MIME 类型，如 `image/png`。
    pub media_type: String,
    /// base64（标准字母表，带 padding）编码的原始字节。
    pub data: String,
}

impl From<&ImageContent> for StoredImage {
    fn from(value: &ImageContent) -> Self {
        Self {
            media_type: value.media_type.clone(),
            data: value.data.clone(),
        }
    }
}

impl From<&StoredImage> for ImageContent {
    fn from(value: &StoredImage) -> Self {
        ImageContent {
            media_type: value.media_type.clone(),
            data: value.data.clone(),
        }
    }
}

/// 用户消息的内容块。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StoredUserContent {
    /// 纯文本。
    Text {
        /// 文本内容。
        text: String,
    },
    /// 图片。
    Image {
        /// 图片内容。
        image: StoredImage,
    },
}

/// 助手消息的内容块。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StoredAssistantContent {
    /// 纯文本。
    Text {
        /// 文本内容。
        text: String,
    },
    /// 思考。`signature` 是提供商侧的不透明凭证，回放时必须原样送回。
    Thinking {
        /// 明文思考文本。
        text: String,
        /// 提供商签名 / 回放载荷。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
    /// 被提供商加密屏蔽的思考。**绝不截断、绝不下钻**：改一个字节，回放即 400。
    RedactedThinking {
        /// 提供商给的不透明载荷。
        data: String,
    },
    /// 工具调用。
    ToolCall {
        /// 提供商分配的调用 id。
        id: String,
        /// 工具名。
        name: String,
        /// 参数的 JSON 文本——保持字符串，避免反复解析/序列化丢失精度。
        arguments: String,
    },
}

/// 工具结果的内容块。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StoredToolResultContent {
    /// 纯文本。
    Text {
        /// 文本内容。
        text: String,
    },
    /// 图片。
    Image {
        /// 图片内容。
        image: StoredImage,
    },
}

/// 落盘态的 token 用量，与 [`zcode_ai::Usage`] 同形但可 serde。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct StoredUsage {
    /// 未命中缓存的输入 token。
    #[serde(default, skip_serializing_if = "is_zero")]
    pub input: u64,
    /// 输出 token。
    #[serde(default, skip_serializing_if = "is_zero")]
    pub output: u64,
    /// 命中缓存的输入 token。
    #[serde(default, skip_serializing_if = "is_zero")]
    pub cache_read: u64,
    /// 写入缓存的输入 token。
    #[serde(default, skip_serializing_if = "is_zero")]
    pub cache_write: u64,
    /// 输出里属于思考的部分。
    #[serde(default, skip_serializing_if = "is_zero")]
    pub reasoning: u64,
}

/// `skip_serializing_if` 要求谓词签名是 `fn(&T) -> bool`，因此这里必须收引用。
#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde 的谓词签名固定为取引用"
)]
fn is_zero(value: &u64) -> bool {
    *value == 0
}

impl StoredUsage {
    /// 是否所有字段都是 0。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

impl From<Usage> for StoredUsage {
    fn from(value: Usage) -> Self {
        Self {
            input: value.input,
            output: value.output,
            cache_read: value.cache_read,
            cache_write: value.cache_write,
            reasoning: value.reasoning,
        }
    }
}

impl From<StoredUsage> for Usage {
    fn from(value: StoredUsage) -> Self {
        Usage {
            input: value.input,
            output: value.output,
            cache_read: value.cache_read,
            cache_write: value.cache_write,
            reasoning: value.reasoning,
        }
    }
}

/// 落盘态的停止原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoredStopReason {
    /// 正常结束。
    #[default]
    Stop,
    /// 触达输出上限。
    Length,
    /// 因为要调用工具而暂停。
    ToolUse,
    /// 提供商侧判定为错误。
    Error,
    /// 本地取消。
    Aborted,
}

impl From<StopReason> for StoredStopReason {
    fn from(value: StopReason) -> Self {
        match value {
            StopReason::Stop => Self::Stop,
            StopReason::Length => Self::Length,
            StopReason::ToolUse => Self::ToolUse,
            StopReason::Error => Self::Error,
            StopReason::Aborted => Self::Aborted,
        }
    }
}

/// UI 展示角色。仅影响渲染，**不影响** API 角色。
///
/// 抄源 jcode `StoredDisplayRole`（`crates/jcode-session-types/src/lib.rs:244-249`）：
/// 软中断注入的系统提醒在 API 里是 user 消息，在 UI 里必须显示成系统消息，否则用户会以为
/// 那句话是自己说的。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplayRole {
    /// 系统提醒（上下文注入、压缩说明）。
    System,
    /// 后台作业回报。
    BackgroundTask,
}

/// 一条落盘态消息。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum StoredMessage {
    /// 用户输入。
    User {
        /// 内容块。
        content: Vec<StoredUserContent>,
        /// UI 展示角色；`None` 表示按 API 角色渲染。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        display_role: Option<DisplayRole>,
    },
    /// 助手输出。
    Assistant {
        /// 内容块。
        content: Vec<StoredAssistantContent>,
        /// 提供商回报的实际模型 id。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        /// 本次请求的 token 用量。
        #[serde(default, skip_serializing_if = "StoredUsage::is_empty")]
        usage: StoredUsage,
        /// 停止原因。
        #[serde(default)]
        stop_reason: StoredStopReason,
    },
    /// 工具执行结果。
    ToolResult {
        /// 对应的工具调用 id。
        tool_call_id: String,
        /// 对应的工具名，部分提供商要求回填。
        tool_name: String,
        /// 内容块。
        content: Vec<StoredToolResultContent>,
        /// 是否为失败结果。
        #[serde(default)]
        is_error: bool,
    },
}

impl StoredMessage {
    /// 构造一条纯文本用户消息。
    #[must_use]
    pub fn user(text: impl Into<String>) -> Self {
        Self::User {
            content: vec![StoredUserContent::Text { text: text.into() }],
            display_role: None,
        }
    }

    /// 构造一条以系统身份展示、但对 API 而言仍是 user 的注入消息。
    #[must_use]
    pub fn system_reminder(text: impl Into<String>) -> Self {
        Self::User {
            content: vec![StoredUserContent::Text { text: text.into() }],
            display_role: Some(DisplayRole::System),
        }
    }

    /// 翻译成提供商请求里的消息。丢弃 id / `display_role` / `usage`。
    #[must_use]
    pub fn to_provider(&self) -> Message {
        match self {
            Self::User { content, .. } => Message::User {
                content: content
                    .iter()
                    .map(|block| match block {
                        StoredUserContent::Text { text } => UserContent::Text(text.clone()),
                        StoredUserContent::Image { image } => UserContent::Image(image.into()),
                    })
                    .collect(),
            },
            Self::Assistant { content, .. } => Message::Assistant {
                content: content
                    .iter()
                    .map(|block| match block {
                        StoredAssistantContent::Text { text } => {
                            AssistantContent::Text(text.clone())
                        }
                        StoredAssistantContent::Thinking { text, signature } => {
                            AssistantContent::Thinking(ThinkingContent {
                                text: text.clone(),
                                signature: signature.clone(),
                            })
                        }
                        StoredAssistantContent::RedactedThinking { data } => {
                            AssistantContent::RedactedThinking(data.clone())
                        }
                        StoredAssistantContent::ToolCall {
                            id,
                            name,
                            arguments,
                        } => AssistantContent::ToolCall(ToolCall {
                            id: id.clone(),
                            name: name.clone(),
                            arguments: arguments.clone(),
                        }),
                    })
                    .collect(),
            },
            Self::ToolResult {
                tool_call_id,
                tool_name,
                content,
                is_error,
            } => Message::ToolResult {
                tool_call_id: tool_call_id.clone(),
                tool_name: tool_name.clone(),
                content: content
                    .iter()
                    .map(|block| match block {
                        StoredToolResultContent::Text { text } => {
                            ToolResultContent::Text(text.clone())
                        }
                        StoredToolResultContent::Image { image } => {
                            ToolResultContent::Image(image.into())
                        }
                    })
                    .collect(),
                is_error: *is_error,
            },
        }
    }

    /// 本条消息发起的所有工具调用。
    pub fn tool_calls(&self) -> impl Iterator<Item = &StoredAssistantContent> {
        let blocks = match self {
            Self::Assistant { content, .. } => content.as_slice(),
            Self::User { .. } | Self::ToolResult { .. } => &[],
        };
        blocks
            .iter()
            .filter(|block| matches!(block, StoredAssistantContent::ToolCall { .. }))
    }
}

/// 带 id 与时间戳的消息记录：这才是真正落盘的单位。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageRecord {
    /// 消息 id。
    pub id: EntryId,
    /// 消息本体。
    #[serde(flatten)]
    pub message: StoredMessage,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_message_round_trips_through_json() {
        let message = StoredMessage::user("你好");
        let json = serde_json::to_string(&message).expect("序列化不应失败");
        assert_eq!(
            json,
            r#"{"role":"user","content":[{"type":"text","text":"你好"}]}"#
        );
        let back: StoredMessage = serde_json::from_str(&json).expect("反序列化不应失败");
        assert_eq!(back, message);
    }

    #[test]
    fn empty_usage_is_omitted_from_the_wire() {
        let message = StoredMessage::Assistant {
            content: vec![StoredAssistantContent::Text {
                text: "ok".to_owned(),
            }],
            model: None,
            usage: StoredUsage::default(),
            stop_reason: StoredStopReason::Stop,
        };
        let json = serde_json::to_string(&message).expect("序列化不应失败");
        assert!(!json.contains("usage"), "全零用量不该占用落盘字节：{json}");
        // 省略字段必须能读回：缺 `serde(default)` 时这一步会失败，而只写不读的断言看不见。
        let back: StoredMessage = serde_json::from_str(&json).expect("省略用量后必须仍可读回");
        assert_eq!(back, message);
    }

    #[test]
    fn display_role_never_reaches_the_provider() {
        let stored = StoredMessage::system_reminder("上下文已压缩");
        let Message::User { content } = stored.to_provider() else {
            panic!("系统提醒在 API 侧必须仍是 user 角色");
        };
        assert_eq!(content, vec![UserContent::Text("上下文已压缩".to_owned())]);
    }

    #[test]
    fn thinking_signature_survives_the_round_trip() {
        // 签名是提供商的回放凭证，掉一个字节整条历史就废了。
        let stored = StoredMessage::Assistant {
            content: vec![StoredAssistantContent::Thinking {
                text: "推理".to_owned(),
                signature: Some("sig-abc".to_owned()),
            }],
            model: Some("claude-sonnet-4-6".to_owned()),
            usage: StoredUsage::default(),
            stop_reason: StoredStopReason::Stop,
        };
        let json = serde_json::to_string(&stored).expect("序列化不应失败");
        let back: StoredMessage = serde_json::from_str(&json).expect("反序列化不应失败");
        assert_eq!(back, stored);

        let Message::Assistant { content } = back.to_provider() else {
            panic!("助手消息必须翻译成 Assistant");
        };
        assert_eq!(
            content,
            vec![AssistantContent::Thinking(ThinkingContent {
                text: "推理".to_owned(),
                signature: Some("sig-abc".to_owned()),
            })]
        );
    }

    #[test]
    fn tool_calls_are_enumerated_from_assistant_messages_only() {
        let assistant = StoredMessage::Assistant {
            content: vec![
                StoredAssistantContent::Text {
                    text: "读一下".to_owned(),
                },
                StoredAssistantContent::ToolCall {
                    id: "call_1".to_owned(),
                    name: "read".to_owned(),
                    arguments: r#"{"path":"a.rs"}"#.to_owned(),
                },
            ],
            model: None,
            usage: StoredUsage::default(),
            stop_reason: StoredStopReason::ToolUse,
        };
        assert_eq!(assistant.tool_calls().count(), 1);
        assert_eq!(StoredMessage::user("hi").tool_calls().count(), 0);
    }
}
