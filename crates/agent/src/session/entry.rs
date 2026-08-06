//! 会话条目：JSONL 的一行，同时是会话树的一个节点。
//!
//! # 为什么是树而不是列表
//!
//! 每条条目带 `parent_id`，整个会话是一棵树，"当前上下文"是根到某个叶子的路径。
//! `/branch`、`/rewind`、重试都只是**在同一个文件里派生一条新分支**，旧历史一行不动。
//! 抄源 oh-my-pi `packages/coding-agent/src/session/session-entries.ts:58-62,245-260`。
//!
//! 代价必须写明（上游同样承担）：读会话时要重建 `id -> 条目` 索引与父子链，
//! 拿"当前上下文"是一次自叶向根的回溯 + 反转，**不是 O(1)**。换来的是分支零拷贝：
//! 线性方案（jcode 的 snapshot + journal，`crates/jcode-base/src/session/persistence.rs:317-425`）
//! 要实现同样的分支语义就得复制整个会话文件。
//!
//! # 为什么不做定宽标题槽
//!
//! 上游第一行是 256 字节定宽槽位，为的是"改标题不重写全文"
//! （`session-entries.ts:7,15-23`）。本仓改标题走一条 [`EntryKind::TitleChange`] 追加条目，
//! 读取时取路径上最后一条生效——同样是一次追加写，却不用维护定宽不变量，也不会因为标题
//! 超过 256 字节而退化成全文重写。

use serde::{Deserialize, Serialize};

use crate::id::EntryId;
use crate::session::message::StoredMessage;

/// 压缩的触发原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactionReason {
    /// 达到阈值，主动压缩。
    Threshold,
    /// 提供商已经报了上下文超限，被动压缩。
    Overflow,
    /// 用户显式请求。
    Manual,
}

/// 一条条目承载的内容。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EntryKind {
    /// 会话初始化：一个会话文件的第一条条目，`parent_id` 必为 `None`。
    SessionInit {
        /// 会话建立时的工作目录。
        cwd: String,
        /// 初始模型 id。
        model: String,
    },
    /// 一条消息。
    Message {
        /// 消息本体。
        message: StoredMessage,
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
    /// 一次上下文压缩：摘要本身，以及"从哪条消息开始保留原文"。
    ///
    /// `first_kept` 为 `None` 表示整段前缀都被摘要取代。重建上下文时，位于本条目之前、
    /// 且不在保留段内的消息全部由 `summary` 替代。
    Compaction {
        /// 摘要文本。
        summary: String,
        /// 保留段的第一条消息 id。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        first_kept: Option<EntryId>,
        /// 触发原因。
        reason: CompactionReason,
    },
}

/// JSONL 的一行。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionEntry {
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

impl SessionEntry {
    /// 用当前时刻与新生成的 id 构造一条条目。
    #[must_use]
    pub fn new(parent_id: Option<EntryId>, kind: EntryKind) -> Self {
        Self {
            id: EntryId::generate(),
            parent_id,
            timestamp_ms: crate::id::now_millis(),
            kind,
        }
    }

    /// 若本条目是消息，借出消息本体。
    #[must_use]
    pub fn message(&self) -> Option<&StoredMessage> {
        match &self.kind {
            EntryKind::Message { message } => Some(message),
            EntryKind::SessionInit { .. }
            | EntryKind::ModelChange { .. }
            | EntryKind::TitleChange { .. }
            | EntryKind::Compaction { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_flattens_its_kind_into_one_json_object() {
        let entry = SessionEntry {
            id: EntryId::from("ent_1".to_owned()),
            parent_id: None,
            timestamp_ms: 42,
            kind: EntryKind::TitleChange {
                title: "标题".to_owned(),
            },
        };
        let json = serde_json::to_string(&entry).expect("序列化不应失败");
        assert_eq!(
            json,
            r#"{"id":"ent_1","timestamp_ms":42,"type":"title_change","title":"标题"}"#
        );
        let back: SessionEntry = serde_json::from_str(&json).expect("反序列化不应失败");
        assert_eq!(back, entry);
    }

    #[test]
    fn root_entry_omits_parent_id() {
        let entry = SessionEntry::new(
            None,
            EntryKind::SessionInit {
                cwd: "/tmp".to_owned(),
                model: "claude-sonnet-4-6".to_owned(),
            },
        );
        let json = serde_json::to_string(&entry).expect("序列化不应失败");
        assert!(
            !json.contains("parent_id"),
            "根条目不该写空 parent_id：{json}"
        );
    }

    #[test]
    fn message_accessor_only_matches_message_entries() {
        let message = SessionEntry::new(
            None,
            EntryKind::Message {
                message: StoredMessage::user("hi"),
            },
        );
        assert!(message.message().is_some());
        let title = SessionEntry::new(
            None,
            EntryKind::TitleChange {
                title: "t".to_owned(),
            },
        );
        assert!(title.message().is_none());
    }
}
