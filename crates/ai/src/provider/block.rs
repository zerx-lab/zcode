//! 内容块累加器：把各家的增量事件拼成有序的 [`StreamEvent`]。
//!
//! 三家提供商标识内容块的方式各不相同（Anthropic 用数字 index，Chat Completions
//! 用 `tool_calls[].index`，Responses 用 `item_id` + `content_index`），但拼装
//! 逻辑完全一样：首见即 `*_start`，增量即 `*_delta`，收尾即 `*_end`，且下标必须
//! 与最终助手消息的 `content` 数组一致。这里用字符串 key 抹平差异，各适配器只
//! 负责给出稳定的 key。

use std::collections::HashMap;

use crate::types::{StreamEvent, ThinkingContent, ToolCall};

/// 内容块类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BlockKind {
    /// 文本。
    Text,
    /// 思考。
    Thinking,
    /// 被提供商加密屏蔽的思考：整块一次到齐，没有增量。
    RedactedThinking,
    /// 工具调用。
    ToolCall,
}

#[derive(Debug)]
struct Block {
    kind: BlockKind,
    /// 文本 / 思考的正文，或工具调用的参数 JSON 片段。
    body: String,
    signature: Option<String>,
    tool_id: String,
    tool_name: String,
    open: bool,
}

/// 按出现顺序累积内容块。
#[derive(Debug, Default)]
pub(crate) struct Blocks {
    blocks: Vec<Block>,
    index_by_key: HashMap<String, usize>,
}

impl Blocks {
    /// 新建空累加器。
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// 是否累积到过工具调用。
    ///
    /// 部分提供商在有工具调用时仍回 `finish_reason: stop`，需要据此提升为
    /// `tool_use`。
    pub(crate) fn has_tool_calls(&self) -> bool {
        self.blocks
            .iter()
            .any(|block| block.kind == BlockKind::ToolCall)
    }

    /// 开一个文本块（不产生增量事件）。
    pub(crate) fn open_text(&mut self, key: &str, out: &mut Vec<StreamEvent>) {
        let _index = self.ensure(key, BlockKind::Text, None, out);
    }

    /// 开一个思考块（不产生增量事件）。
    pub(crate) fn open_thinking(&mut self, key: &str, out: &mut Vec<StreamEvent>) {
        let _index = self.ensure(key, BlockKind::Thinking, None, out);
    }

    /// 追加文本增量。空增量只建块，不发事件。
    pub(crate) fn text_delta(&mut self, key: &str, delta: &str, out: &mut Vec<StreamEvent>) {
        let index = self.ensure(key, BlockKind::Text, None, out);
        if delta.is_empty() {
            return;
        }
        let Some(block) = self.blocks.get_mut(index) else {
            return;
        };
        block.body.push_str(delta);
        out.push(StreamEvent::TextDelta {
            index,
            delta: delta.to_owned(),
        });
    }

    /// 追加思考增量。空增量只建块，不发事件。
    pub(crate) fn thinking_delta(&mut self, key: &str, delta: &str, out: &mut Vec<StreamEvent>) {
        let index = self.ensure(key, BlockKind::Thinking, None, out);
        if delta.is_empty() {
            return;
        }
        let Some(block) = self.blocks.get_mut(index) else {
            return;
        };
        block.body.push_str(delta);
        out.push(StreamEvent::ThinkingDelta {
            index,
            delta: delta.to_owned(),
        });
    }

    /// 记录一整块加密思考。内容不可读，只能原样回放。
    pub(crate) fn redacted_thinking(&mut self, key: &str, data: &str, out: &mut Vec<StreamEvent>) {
        let index = self.ensure(key, BlockKind::RedactedThinking, None, out);
        let Some(block) = self.blocks.get_mut(index) else {
            return;
        };
        block.body.clear();
        block.body.push_str(data);
    }

    /// 追加思考签名分片：Anthropic 的 `signature_delta` 是分多帧来的。
    pub(crate) fn append_thinking_signature(
        &mut self,
        key: &str,
        signature: &str,
        out: &mut Vec<StreamEvent>,
    ) {
        let index = self.ensure(key, BlockKind::Thinking, None, out);
        let Some(block) = self.blocks.get_mut(index) else {
            return;
        };
        block
            .signature
            .get_or_insert_with(String::new)
            .push_str(signature);
    }

    /// 覆盖思考签名。
    ///
    /// Responses 对同一个 reasoning item 会先后发 `output_item.added` 与
    /// `output_item.done`，两次都携带完整 item。追加会拼成 `{…}{…}` 这种非法 JSON，
    /// 回放时解析失败、`encrypted_content` 静默丢失，所以必须以最后一次为准。
    pub(crate) fn set_thinking_signature(
        &mut self,
        key: &str,
        signature: &str,
        out: &mut Vec<StreamEvent>,
    ) {
        let index = self.ensure(key, BlockKind::Thinking, None, out);
        let Some(block) = self.blocks.get_mut(index) else {
            return;
        };
        block.signature = Some(signature.to_owned());
    }

    /// 开一个工具调用块，或补齐已有块的 id / 名字。
    ///
    /// 首帧常常只给 id 与名字，后续帧只给参数；也有提供商反过来。两种都要能接。
    pub(crate) fn tool_start(
        &mut self,
        key: &str,
        id: &str,
        name: &str,
        out: &mut Vec<StreamEvent>,
    ) {
        let index = self.ensure(key, BlockKind::ToolCall, Some((id, name)), out);
        let Some(block) = self.blocks.get_mut(index) else {
            return;
        };
        if !id.is_empty() {
            block.tool_id.clear();
            block.tool_id.push_str(id);
        }
        if !name.is_empty() {
            block.tool_name.clear();
            block.tool_name.push_str(name);
        }
    }

    /// 追加工具调用参数片段。
    pub(crate) fn tool_delta(&mut self, key: &str, delta: &str, out: &mut Vec<StreamEvent>) {
        let index = self.ensure(key, BlockKind::ToolCall, None, out);
        let Some(block) = self.blocks.get_mut(index) else {
            return;
        };
        block.body.push_str(delta);
        out.push(StreamEvent::ToolCallDelta {
            index,
            delta: delta.to_owned(),
        });
    }

    /// 直接落一份完整参数（非流式返回，或提供商在 `done` 事件里一次性给全）。
    pub(crate) fn set_tool_arguments(
        &mut self,
        key: &str,
        arguments: &str,
        out: &mut Vec<StreamEvent>,
    ) {
        let index = self.ensure(key, BlockKind::ToolCall, None, out);
        let Some(block) = self.blocks.get_mut(index) else {
            return;
        };
        if block.body == arguments {
            return;
        }
        // 已经流过增量时不要重复计入，直接以完整值为准。
        block.body.clear();
        block.body.push_str(arguments);
    }

    /// 收尾指定块。
    pub(crate) fn close(&mut self, key: &str, out: &mut Vec<StreamEvent>) {
        let Some(&index) = self.index_by_key.get(key) else {
            return;
        };
        self.close_at(index, out);
    }

    /// 收尾所有还开着的块，顺序与开启顺序一致。
    pub(crate) fn close_all(&mut self, out: &mut Vec<StreamEvent>) {
        for index in 0..self.blocks.len() {
            self.close_at(index, out);
        }
    }

    fn close_at(&mut self, index: usize, out: &mut Vec<StreamEvent>) {
        let Some(block) = self.blocks.get_mut(index) else {
            return;
        };
        if !block.open {
            return;
        }
        block.open = false;
        match block.kind {
            BlockKind::Text => {
                out.push(StreamEvent::TextEnd {
                    index,
                    text: block.body.clone(),
                });
            }
            BlockKind::RedactedThinking => {
                out.push(StreamEvent::RedactedThinking {
                    index,
                    data: block.body.clone(),
                });
            }
            BlockKind::Thinking => out.push(StreamEvent::ThinkingEnd {
                index,
                content: ThinkingContent {
                    text: block.body.clone(),
                    signature: block.signature.clone(),
                },
            }),
            BlockKind::ToolCall => out.push(StreamEvent::ToolCallEnd {
                index,
                tool_call: ToolCall {
                    id: block.tool_id.clone(),
                    name: block.tool_name.clone(),
                    // 没有任何参数增量时补一个空对象，下游解析才不会炸。
                    arguments: if block.body.is_empty() {
                        "{}".to_owned()
                    } else {
                        block.body.clone()
                    },
                },
            }),
        }
    }

    /// 取已有块的下标，没有就新建并发出 `*_start`。
    ///
    /// `identity` 只对工具块有意义：三家提供商的首帧都带调用 id 与工具名，建块时
    /// 就填进 `ToolCallStart`，消费者才能在参数流完之前显示"正在调用 X"。
    fn ensure(
        &mut self,
        key: &str,
        kind: BlockKind,
        identity: Option<(&str, &str)>,
        out: &mut Vec<StreamEvent>,
    ) -> usize {
        if let Some(&index) = self.index_by_key.get(key) {
            return index;
        }
        let (id, name) = identity.unwrap_or_default();
        let index = self.blocks.len();
        self.blocks.push(Block {
            kind,
            body: String::new(),
            signature: None,
            tool_id: id.to_owned(),
            tool_name: name.to_owned(),
            open: true,
        });
        self.index_by_key.insert(key.to_owned(), index);
        // 加密思考整块一次到齐，没有增量阶段，因此**不发** `*_start`：它在 close
        // 时以单个自洽的 `RedactedThinking` 交付。发了 start 却没有配对的 end，
        // 会让按 start/end 维护块生命周期的消费者留下一个永不关闭的思考块。
        match kind {
            BlockKind::Text => out.push(StreamEvent::TextStart { index }),
            BlockKind::Thinking => out.push(StreamEvent::ThinkingStart { index }),
            BlockKind::RedactedThinking => {}
            BlockKind::ToolCall => out.push(StreamEvent::ToolCallStart {
                index,
                id: id.to_owned(),
                name: name.to_owned(),
            }),
        }
        index
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drain(
        blocks: &mut Blocks,
        act: impl FnOnce(&mut Blocks, &mut Vec<StreamEvent>),
    ) -> Vec<StreamEvent> {
        let mut out = Vec::new();
        act(blocks, &mut out);
        out
    }

    #[test]
    fn first_delta_opens_the_block_and_close_emits_the_whole_text() {
        let mut blocks = Blocks::new();
        let opened = drain(&mut blocks, |b, out| {
            b.text_delta("a", "he", out);
            b.text_delta("a", "llo", out);
        });
        assert_eq!(
            opened,
            vec![
                StreamEvent::TextStart { index: 0 },
                StreamEvent::TextDelta {
                    index: 0,
                    delta: "he".to_owned()
                },
                StreamEvent::TextDelta {
                    index: 0,
                    delta: "llo".to_owned()
                },
            ]
        );

        let closed = drain(&mut blocks, |b, out| b.close("a", out));
        assert_eq!(
            closed,
            vec![StreamEvent::TextEnd {
                index: 0,
                text: "hello".to_owned()
            }]
        );
    }

    #[test]
    fn distinct_keys_get_sequential_indices() {
        let mut blocks = Blocks::new();
        let events = drain(&mut blocks, |b, out| {
            b.thinking_delta("t", "think", out);
            b.text_delta("x", "say", out);
            b.tool_delta("call-1", "{}", out);
        });
        assert_eq!(
            events.first(),
            Some(&StreamEvent::ThinkingStart { index: 0 })
        );
        assert!(events.contains(&StreamEvent::TextStart { index: 1 }));
        assert!(events.contains(&StreamEvent::ToolCallStart {
            index: 2,
            id: String::new(),
            name: String::new()
        }));
    }

    #[test]
    fn thinking_signature_rides_along_to_the_end_event() {
        let mut blocks = Blocks::new();
        drop(drain(&mut blocks, |b, out| {
            b.thinking_delta("t", "reasoned", out);
            b.append_thinking_signature("t", "sig-", out);
            b.append_thinking_signature("t", "part2", out);
        }));
        let closed = drain(&mut blocks, |b, out| b.close("t", out));
        assert_eq!(
            closed,
            vec![StreamEvent::ThinkingEnd {
                index: 0,
                content: ThinkingContent {
                    text: "reasoned".to_owned(),
                    signature: Some("sig-part2".to_owned()),
                },
            }]
        );
    }

    #[test]
    fn set_thinking_signature_replaces_instead_of_concatenating() {
        let mut blocks = Blocks::new();
        drop(drain(&mut blocks, |b, out| {
            // Responses 的 `.added` 与 `.done` 各送一次完整 item。
            b.set_thinking_signature("t", r#"{"type":"reasoning","id":"rs_1"}"#, out);
            b.set_thinking_signature(
                "t",
                r#"{"type":"reasoning","id":"rs_1","encrypted_content":"x"}"#,
                out,
            );
        }));
        let closed = drain(&mut blocks, |b, out| b.close("t", out));
        let signature = match closed.first() {
            Some(StreamEvent::ThinkingEnd { content, .. }) => content.signature.clone(),
            other => panic!("期望 ThinkingEnd，实际 {other:?}"),
        };
        // 拼接会得到 `{…}{…}`，解析必然失败。
        assert_eq!(
            signature.as_deref(),
            Some(r#"{"type":"reasoning","id":"rs_1","encrypted_content":"x"}"#)
        );
        assert!(
            serde_json::from_str::<serde_json::Value>(signature.unwrap_or_default().as_str())
                .is_ok()
        );
    }

    #[test]
    fn tool_start_puts_the_identity_on_the_start_event() {
        let mut blocks = Blocks::new();
        let events = drain(&mut blocks, |b, out| {
            b.tool_start("c", "call_1", "search", out);
        });
        assert_eq!(
            events,
            vec![StreamEvent::ToolCallStart {
                index: 0,
                id: "call_1".to_owned(),
                name: "search".to_owned(),
            }]
        );
    }

    #[test]
    fn opening_a_block_emits_no_empty_delta() {
        let mut blocks = Blocks::new();
        assert_eq!(
            drain(&mut blocks, |b, out| b.open_text("a", out)),
            vec![StreamEvent::TextStart { index: 0 }]
        );
        assert_eq!(
            drain(&mut blocks, |b, out| b.text_delta("a", "", out)),
            Vec::new(),
            "空增量不该产生事件"
        );
    }

    #[test]
    fn a_redacted_block_emits_exactly_one_self_contained_event() {
        let mut blocks = Blocks::new();
        // 开块阶段不该有任何事件：加密思考没有增量阶段。
        assert_eq!(
            drain(&mut blocks, |b, out| b
                .redacted_thinking("r", "opaque", out)),
            Vec::new(),
            "redacted 块不该发 *Start —— 它没有配对的 *End"
        );
        assert_eq!(
            drain(&mut blocks, |b, out| b.close("r", out)),
            vec![StreamEvent::RedactedThinking {
                index: 0,
                data: "opaque".to_owned()
            }]
        );
    }

    #[test]
    fn every_start_has_a_matching_end_across_a_mixed_stream() {
        let mut blocks = Blocks::new();
        let mut events = drain(&mut blocks, |b, out| {
            b.text_delta("t", "hi", out);
            b.redacted_thinking("r", "opaque", out);
            b.thinking_delta("k", "think", out);
            b.tool_start("c", "call_1", "search", out);
        });
        events.extend(drain(&mut blocks, Blocks::close_all));

        // 逐块统计 start / end，任何一边落单都说明消费者会漏掉生命周期。
        let mut open: std::collections::BTreeMap<usize, i32> = std::collections::BTreeMap::new();
        let mut standalone = 0_u32;
        for event in &events {
            match event {
                StreamEvent::TextStart { index }
                | StreamEvent::ThinkingStart { index }
                | StreamEvent::ToolCallStart { index, .. } => {
                    *open.entry(*index).or_default() += 1;
                }
                StreamEvent::TextEnd { index, .. }
                | StreamEvent::ThinkingEnd { index, .. }
                | StreamEvent::ToolCallEnd { index, .. } => {
                    *open.entry(*index).or_default() -= 1;
                }
                StreamEvent::RedactedThinking { .. } => standalone += 1,
                _ => {}
            }
        }
        assert!(
            open.values().all(|count| *count == 0),
            "start/end 未配对：{open:?}\n事件：{events:#?}"
        );
        assert_eq!(standalone, 1, "redacted 块应当只产出一条独立事件");
        // redacted 块虽然不发 start/end，仍然占掉自己的内容下标。
        assert!(events.contains(&StreamEvent::RedactedThinking {
            index: 1,
            data: "opaque".to_owned()
        }));
    }

    #[test]
    fn tool_identity_can_arrive_before_or_after_the_arguments() {
        let mut blocks = Blocks::new();
        drop(drain(&mut blocks, |b, out| {
            b.tool_delta("c", "{\"a\":", out);
            b.tool_start("c", "call_1", "search", out);
            b.tool_delta("c", "1}", out);
        }));
        let closed = drain(&mut blocks, |b, out| b.close("c", out));
        assert_eq!(
            closed,
            vec![StreamEvent::ToolCallEnd {
                index: 0,
                tool_call: ToolCall {
                    id: "call_1".to_owned(),
                    name: "search".to_owned(),
                    arguments: "{\"a\":1}".to_owned(),
                },
            }]
        );
    }

    #[test]
    fn empty_arguments_become_an_empty_object() {
        let mut blocks = Blocks::new();
        drop(drain(&mut blocks, |b, out| {
            b.tool_start("c", "id", "noop", out);
        }));
        let closed = drain(&mut blocks, |b, out| b.close("c", out));
        assert!(matches!(
            closed.first(),
            Some(StreamEvent::ToolCallEnd { tool_call, .. }) if tool_call.arguments == "{}"
        ));
    }

    #[test]
    fn complete_arguments_replace_streamed_fragments_without_duplication() {
        let mut blocks = Blocks::new();
        drop(drain(&mut blocks, |b, out| {
            b.tool_delta("c", "{\"a\":", out);
            b.set_tool_arguments("c", "{\"a\":1}", out);
        }));
        let closed = drain(&mut blocks, |b, out| b.close("c", out));
        assert!(matches!(
            closed.first(),
            Some(StreamEvent::ToolCallEnd { tool_call, .. }) if tool_call.arguments == "{\"a\":1}"
        ));
    }

    #[test]
    fn closing_twice_emits_the_end_event_once() {
        let mut blocks = Blocks::new();
        drop(drain(&mut blocks, |b, out| b.text_delta("a", "x", out)));
        assert_eq!(drain(&mut blocks, |b, out| b.close("a", out)).len(), 1);
        assert!(drain(&mut blocks, |b, out| b.close("a", out)).is_empty());
    }

    #[test]
    fn close_all_finishes_leftovers_in_open_order() {
        let mut blocks = Blocks::new();
        drop(drain(&mut blocks, |b, out| {
            b.text_delta("a", "1", out);
            b.text_delta("b", "2", out);
            b.close("a", out);
        }));
        let closed = drain(&mut blocks, Blocks::close_all);
        assert_eq!(
            closed,
            vec![StreamEvent::TextEnd {
                index: 1,
                text: "2".to_owned()
            }]
        );
    }

    #[test]
    fn tool_call_presence_is_tracked_for_stop_reason_promotion() {
        let mut blocks = Blocks::new();
        drop(drain(&mut blocks, |b, out| b.text_delta("a", "hi", out)));
        assert!(!blocks.has_tool_calls());
        drop(drain(&mut blocks, |b, out| b.tool_delta("c", "{}", out)));
        assert!(blocks.has_tool_calls());
    }

    #[test]
    fn closing_an_unknown_key_is_a_no_op() {
        let mut blocks = Blocks::new();
        assert!(drain(&mut blocks, |b, out| b.close("nope", out)).is_empty());
    }
}
