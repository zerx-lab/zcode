//! 跨提供商统一的消息、工具与流式事件模型。
//!
//! 每个提供商适配器负责把这里的类型翻译成自己的线格式，再把线响应翻译回
//! [`StreamEvent`]。上层只认这一套词汇，不感知 Anthropic / OpenAI 的差异。

use std::fmt;

/// 推理努力档位。规范定义在 `zcode-catalog`——模型目录才是档位集合的事实来源，
/// 本 crate 只是把它用在请求签名上（见 `rule://zcode-architecture` 的 catalog 导入边界）。
pub use zcode_catalog::effort::Effort;

/// 支持的提供商标识。
///
/// 同一家可能有多个条目：`xai` 走 Chat Completions + API key，`xai-oauth` 走
/// Responses + 设备码登录，两者凭据与线格式都不同，必须分开。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ProviderId {
    /// Anthropic 官方 Messages API（API key 或 Claude Code OAuth）。
    Anthropic,
    /// OpenAI 平台 API key（Chat Completions 与 Responses 共用）。
    OpenAi,
    /// ChatGPT 订阅 OAuth，走 `chatgpt.com/backend-api/codex/responses`。
    OpenAiCodex,
    /// xAI 平台 API key，走 Chat Completions。
    Xai,
    /// xAI SuperGrok 设备码 OAuth，走 Responses。
    XaiOAuth,
}

impl ProviderId {
    /// 持久化与日志用的稳定标识串。
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::OpenAi => "openai",
            Self::OpenAiCodex => "openai-codex",
            Self::Xai => "xai",
            Self::XaiOAuth => "xai-oauth",
        }
    }

    /// 从持久化标识串还原。
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "anthropic" => Some(Self::Anthropic),
            "openai" => Some(Self::OpenAi),
            "openai-codex" => Some(Self::OpenAiCodex),
            "xai" => Some(Self::Xai),
            "xai-oauth" => Some(Self::XaiOAuth),
            _ => None,
        }
    }

    /// 可作为 Bearer 凭据读取的环境变量，按优先级排列。
    ///
    /// `xai-oauth` 允许直接注入订阅 token（`XAI_OAUTH_TOKEN`），取不到时回落到
    /// 平台 API key；`openai-codex` 只能走 OAuth，没有环境变量入口。
    #[must_use]
    pub fn bearer_env(self) -> &'static [&'static str] {
        match self {
            Self::Anthropic => &["ANTHROPIC_API_KEY"],
            Self::OpenAi => &["OPENAI_API_KEY"],
            Self::Xai => &["XAI_API_KEY"],
            Self::XaiOAuth => &["XAI_OAUTH_TOKEN", "XAI_API_KEY"],
            Self::OpenAiCodex => &[],
        }
    }
}

impl fmt::Display for ProviderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 图片内容：一律以 base64 内联，不引用远端 URL。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageContent {
    /// MIME 类型，如 `image/png`。
    pub media_type: String,
    /// base64（标准字母表，带 padding）编码的原始字节。
    pub data: String,
}

impl ImageContent {
    /// 拼成 OpenAI 系列要求的 `data:` URL。
    #[must_use]
    pub fn to_data_url(&self) -> String {
        let mut url = String::with_capacity(self.media_type.len() + self.data.len() + 13);
        url.push_str("data:");
        url.push_str(&self.media_type);
        url.push_str(";base64,");
        url.push_str(&self.data);
        url
    }
}

/// 一段思考内容。
///
/// `signature` 是提供商侧的不透明凭证，回放历史时必须原样送回：Anthropic 放
/// `signature` 字符串，OpenAI Responses 放整个 `reasoning` item 的 JSON 文本。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ThinkingContent {
    /// 明文思考文本（可能是摘要）。
    pub text: String,
    /// 提供商签名/回放载荷。
    pub signature: Option<String>,
}

/// 一次工具调用。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCall {
    /// 提供商分配的调用 id，工具结果靠它配对。
    pub id: String,
    /// 工具名。
    pub name: String,
    /// 参数的 JSON 文本——保持字符串，避免反复解析/序列化丢失精度。
    pub arguments: String,
}

/// 用户消息里允许的内容块。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserContent {
    /// 纯文本。
    Text(String),
    /// 图片。
    Image(ImageContent),
}

/// 助手消息里允许的内容块。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssistantContent {
    /// 纯文本。
    Text(String),
    /// 思考。
    Thinking(ThinkingContent),
    /// 被提供商加密屏蔽的思考，只能原样回放。
    RedactedThinking(String),
    /// 工具调用。
    ToolCall(ToolCall),
}

/// 工具结果里允许的内容块。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolResultContent {
    /// 纯文本。
    Text(String),
    /// 图片。
    Image(ImageContent),
}

/// 对话中的一条消息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    /// 用户输入。
    User {
        /// 内容块。
        content: Vec<UserContent>,
    },
    /// 助手输出。
    Assistant {
        /// 内容块。
        content: Vec<AssistantContent>,
    },
    /// 工具执行结果。
    ToolResult {
        /// 对应的 [`ToolCall::id`]。
        tool_call_id: String,
        /// 对应的工具名，部分提供商要求回填。
        tool_name: String,
        /// 内容块。
        content: Vec<ToolResultContent>,
        /// 是否为失败结果。
        is_error: bool,
    },
}

impl Message {
    /// 构造一条纯文本用户消息。
    #[must_use]
    pub fn user(text: impl Into<String>) -> Self {
        Self::User {
            content: vec![UserContent::Text(text.into())],
        }
    }

    /// 构造一条纯文本助手消息。
    #[must_use]
    pub fn assistant(text: impl Into<String>) -> Self {
        Self::Assistant {
            content: vec![AssistantContent::Text(text.into())],
        }
    }
}

/// 一个可供模型调用的工具。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tool {
    /// 工具名。
    pub name: String,
    /// 给模型看的说明。
    pub description: String,
    /// 参数的 JSON Schema（顶层必须是 `{"type":"object",...}`）。
    pub parameters: serde_json::Value,
    /// 是否要求提供商做严格 schema 校验；`None` 表示不下发该字段。
    pub strict: Option<bool>,
}

/// 工具选择策略。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ToolChoice {
    /// 由模型自行决定。
    #[default]
    Auto,
    /// 禁止调用工具。
    None,
    /// 必须调用某个工具。
    Required,
    /// 必须调用指定工具。
    Named(String),
}

/// 思考预算配置。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Thinking {
    /// 不开思考。
    #[default]
    Disabled,
    /// Anthropic 风格：给定 token 预算。
    Budget {
        /// `thinking.budget_tokens`。
        tokens: u32,
    },
    /// OpenAI / Grok 风格：给定努力档位。
    Effort(Effort),
}

/// 处理优先级 / 成本档位。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceTier {
    /// `auto`
    Auto,
    /// `default`
    Default,
    /// `flex`
    Flex,
    /// `scale`
    Scale,
    /// `priority`
    Priority,
}

impl ServiceTier {
    /// 线上取值。
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Default => "default",
            Self::Flex => "flex",
            Self::Scale => "scale",
            Self::Priority => "priority",
        }
    }
}

/// prompt 缓存保留时长偏好。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CacheRetention {
    /// 不打缓存断点。
    None,
    /// 默认短时缓存（Anthropic 5 分钟）。
    #[default]
    Short,
    /// 长时缓存（Anthropic 1 小时 / OpenAI 24h）。
    Long,
}

/// 一次补全请求。
#[derive(Debug, Clone)]
pub struct CompletionRequest {
    /// 线上的模型 id。
    pub model: String,
    /// system / developer 提示，按顺序拼接。
    pub system: Vec<String>,
    /// 对话历史。
    pub messages: Vec<Message>,
    /// 可用工具。
    pub tools: Vec<Tool>,
    /// 工具选择策略。
    pub tool_choice: ToolChoice,
    /// 输出 token 上限。
    pub max_output_tokens: Option<u32>,
    /// 采样温度。
    pub temperature: Option<f32>,
    /// 核采样阈值。
    pub top_p: Option<f32>,
    /// 停止序列（Anthropic 最多 4 条）。
    pub stop_sequences: Vec<String>,
    /// 思考配置。
    pub thinking: Thinking,
    /// 处理优先级。
    pub service_tier: Option<ServiceTier>,
    /// 会话 id：用于 prompt 缓存亲和与请求追踪。
    pub session_id: Option<String>,
    /// 显式 prompt 缓存键。
    pub prompt_cache_key: Option<String>,
    /// 缓存保留偏好。
    pub cache_retention: CacheRetention,
}

impl CompletionRequest {
    /// 用模型 id 与一组消息构造请求，其余字段取默认值。
    #[must_use]
    pub fn new(model: impl Into<String>, messages: Vec<Message>) -> Self {
        Self {
            model: model.into(),
            system: Vec::new(),
            messages,
            tools: Vec::new(),
            tool_choice: ToolChoice::Auto,
            max_output_tokens: None,
            temperature: None,
            top_p: None,
            stop_sequences: Vec::new(),
            thinking: Thinking::Disabled,
            service_tier: None,
            session_id: None,
            prompt_cache_key: None,
            cache_retention: CacheRetention::default(),
        }
    }
}

/// 停止原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StopReason {
    /// 正常结束。
    #[default]
    Stop,
    /// 触达输出上限。
    Length,
    /// 因为要调用工具而暂停。
    ToolUse,
    /// 提供商侧判定为错误（内容过滤、拒答等）。
    Error,
    /// 本地取消。
    Aborted,
}

/// token 用量。
///
/// `input` 已扣除 `cache_read` 与 `cache_write`，四者相加才是计费的输入总量；
/// `reasoning` 是 `output` 的子集，不额外计数。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Usage {
    /// 未命中缓存的输入 token。
    pub input: u64,
    /// 输出 token。
    pub output: u64,
    /// 命中缓存的输入 token。
    pub cache_read: u64,
    /// 写入缓存的输入 token。
    pub cache_write: u64,
    /// 输出里属于思考的部分。
    pub reasoning: u64,
}

impl Usage {
    /// 计费意义上的输入总量。
    #[must_use]
    pub fn total_input(&self) -> u64 {
        self.input
            .saturating_add(self.cache_read)
            .saturating_add(self.cache_write)
    }
}

/// 流式事件。
///
/// `index` 是内容块在最终助手消息 `content` 数组里的下标，同一块的
/// `*_start` / `*_delta` / `*_end` 共享同一个 index。
///
/// # 配对不变量
///
/// 每个 `*Start` 都恰有一个同 index 的 `*End` 与之配对，消费者可以据此维护块的
/// 生命周期。唯一的例外是 [`StreamEvent::RedactedThinking`]：它整块一次到齐，
/// 既不发 start 也不发 end，本身就是一个自洽的完整事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamEvent {
    /// 流开始，携带提供商侧的响应 id 与实际模型。
    Start {
        /// 提供商响应 id。
        response_id: Option<String>,
        /// 提供商回报的模型 id。
        model: Option<String>,
    },
    /// 文本块开始。
    TextStart {
        /// 内容块下标。
        index: usize,
    },
    /// 文本增量。
    TextDelta {
        /// 内容块下标。
        index: usize,
        /// 新增文本。
        delta: String,
    },
    /// 文本块结束。
    TextEnd {
        /// 内容块下标。
        index: usize,
        /// 该块的完整文本。
        text: String,
    },
    /// 思考块开始。
    ThinkingStart {
        /// 内容块下标。
        index: usize,
    },
    /// 思考增量。
    ThinkingDelta {
        /// 内容块下标。
        index: usize,
        /// 新增文本。
        delta: String,
    },
    /// 思考块结束。
    ThinkingEnd {
        /// 内容块下标。
        index: usize,
        /// 该块的完整思考内容（含签名）。
        content: ThinkingContent,
    },
    /// 一整块被提供商加密屏蔽的思考。
    ///
    /// 内容不可读，只能在回放历史时原样送回，否则服务端签名校验会失败。
    ///
    /// **没有配套的 start / end**：内容一次到齐，这条事件即是完整交付。
    RedactedThinking {
        /// 内容块下标。
        index: usize,
        /// 提供商给的不透明载荷。
        data: String,
    },
    /// 工具调用块开始。
    ToolCallStart {
        /// 内容块下标。
        index: usize,
        /// 调用 id，首帧未给出时为空串。
        id: String,
        /// 工具名，首帧未给出时为空串。
        name: String,
    },
    /// 工具调用参数增量（原始 JSON 片段）。
    ToolCallDelta {
        /// 内容块下标。
        index: usize,
        /// 新增 JSON 片段。
        delta: String,
    },
    /// 工具调用块结束。
    ToolCallEnd {
        /// 内容块下标。
        index: usize,
        /// 完整调用。
        tool_call: ToolCall,
    },
    /// 流终止。
    Done {
        /// 停止原因。
        stop_reason: StopReason,
        /// 累计用量。
        usage: Usage,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_id_roundtrips_through_its_wire_string() {
        for id in [
            ProviderId::Anthropic,
            ProviderId::OpenAi,
            ProviderId::OpenAiCodex,
            ProviderId::Xai,
            ProviderId::XaiOAuth,
        ] {
            assert_eq!(ProviderId::parse(id.as_str()), Some(id));
        }
        assert_eq!(ProviderId::parse("gemini"), None);
    }

    #[test]
    fn env_bearer_lookup_matches_each_provider_login_story() {
        assert!(ProviderId::OpenAiCodex.bearer_env().is_empty());
        assert_eq!(ProviderId::Anthropic.bearer_env(), ["ANTHROPIC_API_KEY"]);
        assert_eq!(
            ProviderId::XaiOAuth.bearer_env(),
            ["XAI_OAUTH_TOKEN", "XAI_API_KEY"]
        );
    }

    #[test]
    fn image_renders_as_data_url() {
        let image = ImageContent {
            media_type: "image/png".to_owned(),
            data: "QUJD".to_owned(),
        };
        assert_eq!(image.to_data_url(), "data:image/png;base64,QUJD");
    }

    #[test]
    fn total_input_sums_cache_buckets() {
        let usage = Usage {
            input: 10,
            output: 5,
            cache_read: 3,
            cache_write: 2,
            reasoning: 4,
        };
        assert_eq!(usage.total_input(), 15);
    }
}
