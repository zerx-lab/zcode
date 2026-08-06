//! 跨进程传输的领域投影：会话条目树、消息、用量、审批。
//!
//! # 为什么要镜像一份，而不是转出 `zcode-agent` 的落盘类型
//!
//! 依赖方向必须是 `tui -> protocol <- runtime`。把 `zcode_agent::StoredMessage` 直接搬上线
//! 就等于要求每个客户端为了反序列化而依赖整个运行时，协议 crate 退化成通用 framing——
//! 这正是 jcode `crates/jcode-tui/src/lib.rs:23` 用 `pub use jcode_app_core::*` 造成的后果。
//!
//! 代价写明：本模块与 `zcode_agent::session` 是两套形状，互转是 host adapter 的职责。
//! 漂移风险由**穷尽 match** 兜住：领域侧加一个变体，adapter 立刻编译失败。
//! 参照 oh-my-pi 的同一分层（`packages/wire/src/index.ts` 独立于 runtime 类型）。
//!
//! # 未知变体的容忍边界
//!
//! 无字段枚举（[`StopReason`]、[`DisplayRole`]、[`CompactionReason`]、[`Tier`]、[`Policy`]、
//! [`ApprovalMode`]、[`ApprovalReply`]）一律带 `#[serde(other)]` 兜底：它们出现在推送路径上，
//! 认不出来时降级渲染远好于整帧解析失败。
//!
//! 带字段的内部 tag 枚举（[`Message`]、[`EntryKind`]、[`UserContent`] 等）**没有**兜底变体——
//! serde 不支持给数据变体做 `other`，而伪造一个空壳变体会让客户端把"没渲染出来的内容"
//! 当成"用户没说过的话"。因此新增这类变体属于**破坏性变更，必须 bump major**
//! （见 [`crate::version::PROTOCOL_VERSION`] 的变更规则）。

use std::fmt;

use serde::{Deserialize, Serialize};

/// 声明一个不透明字符串 id。
///
/// 用 newtype 而不是裸 `String`：host adapter 要在会话 id / 条目 id / 工具调用 id /
/// 审批 id 之间反复搬运，四者都是字符串，混用不会有任何编译期信号。
macro_rules! wire_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// 借出底层字符串。
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }

            /// 取回底层字符串。
            #[must_use]
            pub fn into_string(self) -> String {
                self.0
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_owned())
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

wire_id! {
    /// 会话 id。字典序即时间序。
    SessionId
}
wire_id! {
    /// 会话条目 id。字典序即时间序。
    EntryId
}
wire_id! {
    /// 提供商分配的工具调用 id。
    CallId
}
wire_id! {
    /// 一次审批询问的 id。回复时原样带回。
    ApprovalId
}
wire_id! {
    /// 一次 stdin 询问的 id。回复时原样带回。
    StdinId
}
wire_id! {
    /// 客户端实例 id。**同一个客户端进程重启后必须换一个新的**——它的用途是接管仲裁，
    /// 复用旧 id 会让运行时把新进程误认成还活着的老连接。
    ClientId
}

/// 内联图片。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Image {
    /// MIME 类型，如 `image/png`。
    pub media_type: String,
    /// base64（标准字母表，带 padding）编码的原始字节。
    pub data: String,
}

/// 用户消息的内容块。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UserContent {
    /// 纯文本。
    Text {
        /// 文本内容。
        text: String,
    },
    /// 图片。
    Image {
        /// 图片内容。
        image: Image,
    },
}

/// 助手消息的内容块。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AssistantContent {
    /// 纯文本。
    Text {
        /// 文本内容。
        text: String,
    },
    /// 思考。`signature` 是提供商侧的不透明凭证。
    Thinking {
        /// 明文思考文本。
        text: String,
        /// 提供商签名 / 回放载荷。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
    /// 被提供商加密屏蔽的思考。客户端只能整块显示或折叠，**绝不截断**。
    RedactedThinking {
        /// 提供商给的不透明载荷。
        data: String,
    },
    /// 工具调用。
    ToolCall {
        /// 调用 id。
        id: CallId,
        /// 工具名。
        name: String,
        /// 参数的 JSON 文本。保持字符串：客户端要按原样渲染，重新解析会丢数字精度。
        arguments: String,
    },
}

/// 工具结果的内容块。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolResultContent {
    /// 纯文本。
    Text {
        /// 文本内容。
        text: String,
    },
    /// 图片。
    Image {
        /// 图片内容。
        image: Image,
    },
}

/// token 用量。字段全 0 时整体省略。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Usage {
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

impl Usage {
    /// 是否所有字段都是 0。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// `skip_serializing_if` 的谓词签名固定为取引用。
#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde 的谓词签名固定为取引用"
)]
fn is_zero(value: &u64) -> bool {
    *value == 0
}

/// 助手消息的停止原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
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
    /// 对端比本端新。按 [`StopReason::Stop`] 渲染即可。
    #[serde(other)]
    Unknown,
}

/// UI 展示角色。仅影响渲染，不影响 API 角色。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplayRole {
    /// 系统提醒（上下文注入、压缩说明）。
    System,
    /// 后台作业回报。
    BackgroundTask,
    /// 对端比本端新。按普通角色渲染。
    #[serde(other)]
    Unknown,
}

/// 一条消息。
///
/// **新增 role 是破坏性变更**：内部 tag 的数据枚举无法做未知兜底，旧客户端遇到新 role 会
/// 整帧解析失败。加 role 必须 bump major。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum Message {
    /// 用户输入。
    User {
        /// 内容块。
        content: Vec<UserContent>,
        /// UI 展示角色；`None` 表示按 API 角色渲染。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        display_role: Option<DisplayRole>,
    },
    /// 助手输出。
    Assistant {
        /// 内容块。
        content: Vec<AssistantContent>,
        /// 提供商回报的实际模型 id。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        /// 本次请求的 token 用量。
        #[serde(default, skip_serializing_if = "Usage::is_empty")]
        usage: Usage,
        /// 停止原因。
        #[serde(default)]
        stop_reason: StopReason,
    },
    /// 工具执行结果。
    ToolResult {
        /// 对应的工具调用 id。
        tool_call_id: CallId,
        /// 对应的工具名。
        tool_name: String,
        /// 内容块。
        content: Vec<ToolResultContent>,
        /// 是否为失败结果。
        #[serde(default)]
        is_error: bool,
    },
}

/// 上下文压缩的触发原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactionReason {
    /// 达到阈值，主动压缩。
    Threshold,
    /// 提供商已经报了上下文超限，被动压缩。
    Overflow,
    /// 用户显式请求。
    Manual,
    /// 对端比本端新。
    #[serde(other)]
    Unknown,
}

/// 一条会话条目承载的内容。
///
/// 与 [`Message`] 同理：新增变体是破坏性变更。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EntryKind {
    /// 会话初始化，`parent_id` 必为 `None`。
    SessionInit {
        /// 会话建立时的工作目录。
        cwd: String,
        /// 初始模型 id。
        model: String,
    },
    /// 一条消息。
    Message {
        /// 消息本体。
        message: Message,
    },
    /// 切换模型。
    ModelChange {
        /// 新的模型 id。
        model: String,
    },
    /// 修改标题。路径上最后一条生效。
    TitleChange {
        /// 新标题。
        title: String,
    },
    /// 一次上下文压缩。
    Compaction {
        /// 摘要文本。
        summary: String,
        /// 保留段的第一条消息 id；`None` 表示整段前缀都被摘要取代。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        first_kept: Option<EntryId>,
        /// 触发原因。
        reason: CompactionReason,
    },
}

/// 会话树的一个节点。
///
/// 客户端拿到的是**同一棵树**而不是线性列表：`parent_id` 撑起分支，当前上下文是根到
/// head 的路径。`/rewind`、`/branch` 只是换 head，不产生新历史。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// 条目 id。
    pub id: EntryId,
    /// 父条目 id；`None` 表示这是根。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<EntryId>,
    /// 写入时刻的 Unix 毫秒。
    pub timestamp_ms: u64,
    /// 条目内容。
    #[serde(flatten)]
    pub kind: EntryKind,
}

/// 会话列表里的一行。
///
/// 只放列表渲染需要的字段：拉列表时**绝不**附带条目树，否则打开会话选择器要传输全部历史。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSummary {
    /// 会话 id。
    pub id: SessionId,
    /// 标题；未设置过标题时为 `None`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// 会话建立时的工作目录。
    pub cwd: String,
    /// 当前生效的模型 id。
    pub model: String,
    /// 根条目时间戳（Unix 毫秒）。
    pub created_ms: u64,
    /// 最后一条条目的时间戳（Unix 毫秒）。
    pub updated_ms: u64,
}

/// 工具执行过程中的增量输出。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolProgress {
    /// 一段新的输出文本。
    Chunk {
        /// 新增文本。
        text: String,
    },
    /// 工具自报的一行状态。
    Status {
        /// 状态文本。
        text: String,
    },
}

/// 工具的能力档位。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    /// 只读：不改变任何外部状态。
    Read,
    /// 写入：修改工作区文件。
    Write,
    /// 执行：跑任意命令、访问网络、驱动外部程序。
    Exec,
    /// 对端比本端新。客户端**必须按最危险处理**——档位是 fail-safe 语义，
    /// 认不出来时当成 [`Tier::Exec`] 渲染，绝不当成只读。
    #[serde(other)]
    Unknown,
}

/// 一次审批的结论。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Policy {
    /// 直接放行。
    Allow,
    /// 直接拒绝，不询问。
    Deny,
    /// 询问用户。
    Prompt,
    /// 对端比本端新。
    #[serde(other)]
    Unknown,
}

/// 审批模式：决定"多高的档位可以免询问"。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ApprovalMode {
    /// 只有只读工具免询问。
    AlwaysAsk,
    /// 只读与写入免询问，执行类要问。
    Write,
    /// 全部免询问。
    #[default]
    Yolo,
    /// 对端比本端新。
    #[serde(other)]
    Unknown,
}

/// 用户对一次询问的答复。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalReply {
    /// 只放行这一次。
    Once,
    /// 放行，并且本会话内同作用域的后续调用不再问。
    Always,
    /// 拒绝。
    Reject,
    /// 对端比本端新。运行时收到它**必须**按 [`ApprovalReply::Reject`] 结算——
    /// 认不出来的授权一律不放行。
    #[serde(other)]
    Unknown,
}

/// 一条待审批请求。重连时靠它重建 UI。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingApproval {
    /// 审批请求 id。
    pub request_id: ApprovalId,
    /// 触发审批的工具调用 id。
    pub call_id: CallId,
    /// 工具名。
    pub tool_name: String,
    /// `Always` 的作用域：连锁放行只覆盖同作用域的其余待审批。
    pub scope: String,
    /// 展示给用户的提示体。
    pub prompt: String,
}

/// 一条正在等待用户输入的 stdin 询问。
///
/// 与 [`PendingApproval`] 同为"挂在 session 上的待回答项"：连接断掉不作废、换客户端能接手、
/// 重连能重拉。jcode 把 stdin 的 oneshot 存在**每连接**的 map 里
/// （`crates/jcode-app-core/src/server/client_lifecycle.rs:630-666`），连接一断整张表随栈帧
/// drop，工具侧收到 `Err` 退出，而子进程还卡在读 stdin —— 本协议不复制这个形状。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingStdin {
    /// stdin 请求 id。
    pub request_id: StdinId,
    /// 触发询问的工具调用 id。
    pub call_id: CallId,
    /// 子进程打出来的提示（可能为空，例如 `read -s` 什么都不打印）。
    pub prompt: String,
    /// 是否是密码类输入。客户端**必须**据此关闭回显与本地历史记录。
    #[serde(default)]
    pub is_password: bool,
}

#[cfg(test)]
mod tests {
    use super::{
        ApprovalReply, AssistantContent, CallId, Entry, EntryId, EntryKind, Message, StopReason,
        Tier, Usage,
    };

    #[test]
    fn entry_flattens_kind_into_one_object() {
        let entry = Entry {
            id: EntryId::from("ent_1"),
            parent_id: None,
            timestamp_ms: 7,
            kind: EntryKind::ModelChange {
                model: "gpt-5".to_owned(),
            },
        };
        let json = serde_json::to_value(&entry).expect("条目必须可序列化");
        assert_eq!(json["type"], "model_change");
        assert_eq!(json["model"], "gpt-5");
        assert!(json.get("parent_id").is_none(), "None 的父 id 不该占字节");
        assert_eq!(
            serde_json::from_value::<Entry>(json).expect("条目必须可回读"),
            entry
        );
    }

    #[test]
    fn assistant_message_omits_empty_usage_and_defaults_stop_reason() {
        let message = Message::Assistant {
            content: vec![AssistantContent::ToolCall {
                id: CallId::from("call_1"),
                name: "bash".to_owned(),
                arguments: r#"{"cmd":"ls"}"#.to_owned(),
            }],
            model: None,
            usage: Usage::default(),
            stop_reason: StopReason::ToolUse,
        };
        let json = serde_json::to_value(&message).expect("消息必须可序列化");
        assert!(json.get("usage").is_none(), "全 0 用量不该上线");
        assert!(json.get("model").is_none());
        assert_eq!(json["stop_reason"], "tool_use");

        let without_stop = serde_json::json!({
            "role": "assistant",
            "content": [{"type": "text", "text": "hi"}],
        });
        let decoded: Message = serde_json::from_value(without_stop).expect("缺省字段必须能吸收");
        assert!(matches!(
            decoded,
            Message::Assistant {
                stop_reason: StopReason::Stop,
                ..
            }
        ));
    }

    #[test]
    fn unknown_fieldless_variants_degrade_instead_of_failing() {
        // 无字段枚举来自更新的对端时必须降级，而不是让整帧解析失败。
        assert_eq!(
            serde_json::from_str::<StopReason>(r#""quantum_collapse""#).expect("必须吸收未知值"),
            StopReason::Unknown
        );
        assert_eq!(
            serde_json::from_str::<Tier>(r#""nuclear""#).expect("必须吸收未知值"),
            Tier::Unknown
        );
        assert_eq!(
            serde_json::from_str::<ApprovalReply>(r#""maybe""#).expect("必须吸收未知值"),
            ApprovalReply::Unknown
        );
    }

    #[test]
    fn ids_are_transparent_strings() {
        let id = EntryId::from("ent_01K");
        assert_eq!(
            serde_json::to_string(&id).expect("id 必须可序列化"),
            r#""ent_01K""#
        );
        assert_eq!(id.as_str(), "ent_01K");
    }
}
