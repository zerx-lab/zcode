//! 工具契约：trait、执行上下文、输出形状。
//!
//! 注册表在 [`registry`]，批次调度在 [`schedule`]。
//!
//! # 为什么 `execute` 拿 owned 参数
//!
//! `args: Value` 与 `ctx: ToolContext` 都是所有权移交，不借用。理由是工具执行必须能
//! `tokio::spawn` 到独立任务——那要求 future 是 `'static`。这也是"把长跑工具转后台"这类
//! 能力的前提。抄源 jcode `crates/jcode-tool-core/src/lib.rs:117-140`。
//!
//! # trait 里没有超时，也没有校验
//!
//! - **超时**由工具自己实现：不同工具的合理上限差两个数量级（fetch 20s vs bash 3600s，
//!   oh-my-pi `packages/coding-agent/src/tools/tool-timeouts.ts:10-19`），塞进 trait 只会
//!   变成一个所有实现都要覆盖的假默认值。
//! - **参数校验**由 [`registry::ToolRegistry`] 在调度前统一做，校验失败当作 `is_error` 的
//!   工具结果喂回模型而不是错误路径——一次参数笔误不该中断整个 turn
//!   （opencode `packages/opencode/src/tool/tool.ts:25-33`）。

pub mod registry;
pub mod schedule;

use std::fmt;
use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::mpsc;

use crate::approval::ApprovalDecision;
use crate::error::ToolError;
use crate::event::ToolProgress;
use crate::id::{EntryId, SessionId};
use crate::interrupt::InterruptSignal;
use crate::session::message::{StoredImage, StoredToolResultContent};

/// 同一批工具调用之间的并发关系。
///
/// 抄源 oh-my-pi `packages/agent/src/agent-loop.ts:2691-2717` 的屏障链，
/// **不是** jcode 的数字上限（`crates/jcode-app-core/src/tool/batch.rs:10` 的 `MAX_PARALLEL=10`）。
/// 理由：`bash` 的并发性取决于参数——非 PTY 可以并跑，PTY 抢终端必须独占
/// （`packages/coding-agent/src/tools/bash.ts:605-606`）。固定数字表达不了这件事。
///
/// **默认与上游相反**：上游默认 `Shared`（`agent-loop.ts:2706`），因为它的工具全都显式声明过。
/// 本仓默认 [`Concurrency::Exclusive`]，与"没声明审批档位就按 `Exec` 处理"保持同一个方向——
/// 一个判定不了并发性的调用串行跑只是慢，并行跑可能是数据竞争。只读工具显式选 `Shared`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Concurrency {
    /// 可与同批其他 `Shared` 工具并跑。**只有**只读 / 无副作用的工具该选它。
    Shared,
    /// 全屏障：等前面所有工具结束才开始，且它结束前后面的都不开始。
    #[default]
    Exclusive,
}

/// 工具执行上下文。
#[derive(Debug, Clone)]
pub struct ToolContext {
    /// 所属会话。
    pub session_id: SessionId,
    /// 发起本次调用的助手消息 id。
    pub entry_id: EntryId,
    /// 提供商分配的调用 id。
    pub call_id: String,
    /// 工作目录。
    pub cwd: PathBuf,
    /// **硬取消**：用户中断或超出 deadline。所有工具都必须响应，收到就尽快收尾。
    pub cancel: InterruptSignal,
    /// **软取消**：用户在工具执行期间插话。协作式信号，**永不杀**已产生副作用的工具；
    /// 长跑工具可以据此提前收尾或自我后台化。
    ///
    /// 分层抄源 oh-my-pi `packages/agent/src/agent-loop.ts:2258-2267`：排队的插话绝不能
    /// 硬杀一个已经写了一半文件的工具。
    pub steering: InterruptSignal,
    /// 增量输出通道。
    pub progress: mpsc::UnboundedSender<ToolProgress>,
}

impl ToolContext {
    /// 派生一个子调用的上下文（batch / 子 agent 用），只换调用 id。
    #[must_use]
    pub fn for_subcall(&self, call_id: impl Into<String>) -> Self {
        Self {
            call_id: call_id.into(),
            ..self.clone()
        }
    }

    /// 推送一段增量输出。接收端已关闭时静默丢弃——没人看进度不该让工具失败。
    pub fn report(&self, progress: ToolProgress) {
        let _ = self.progress.send(progress);
    }
}

/// 工具执行的产出。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ToolOutput {
    /// 回给模型的内容块。
    pub content: Vec<StoredToolResultContent>,
    /// 给 UI 的一行摘要。
    pub title: Option<String>,
    /// 这条结果消费完即可在压缩时丢弃（零匹配的 grep、超时的等待）。
    ///
    /// 抄源 oh-my-pi `packages/agent/src/types.ts:688`：让工具自报"我这条没有长期价值"，
    /// 比压缩器事后猜哪条能丢准得多。`is_error` 时忽略此位——失败的原因模型往往还要看。
    pub useless: bool,
}

impl ToolOutput {
    /// 纯文本产出。
    #[must_use]
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            content: vec![StoredToolResultContent::Text { text: text.into() }],
            ..Self::default()
        }
    }

    /// 图片产出。
    #[must_use]
    pub fn image(image: StoredImage) -> Self {
        Self {
            content: vec![StoredToolResultContent::Image { image }],
            ..Self::default()
        }
    }

    /// 附加一行 UI 摘要。
    #[must_use]
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// 标记为消费完即可丢弃。
    #[must_use]
    pub fn mark_useless(mut self) -> Self {
        self.useless = true;
        self
    }

    /// 把所有文本块拼起来，用于截断与 token 估算。
    #[must_use]
    pub fn text_len(&self) -> usize {
        self.content
            .iter()
            .map(|block| match block {
                StoredToolResultContent::Text { text } => text.len(),
                StoredToolResultContent::Image { image } => image.data.len(),
            })
            .sum()
    }
}

/// 一个可供模型调用的工具。
#[async_trait]
pub trait Tool: fmt::Debug + Send + Sync {
    /// 工具名，必须在注册表内唯一。
    fn name(&self) -> &str;

    /// 给模型看的说明。
    fn description(&self) -> &str;

    /// 参数的 JSON Schema，顶层必须是 `{"type":"object",...}`。
    fn parameters(&self) -> Value;

    /// 本次调用的审批声明。默认 [`Tier::Exec`](crate::approval::Tier::Exec)：
    /// **没声明就按最危险处理**。
    fn approval(&self, args: &Value) -> ApprovalDecision {
        let _ = args;
        ApprovalDecision::default()
    }

    /// 本次调用与同批其他调用的并发关系。默认 [`Concurrency::Exclusive`]。
    ///
    /// 解析过程若可能失败，实现方必须**倒向保守侧**返回 [`Concurrency::Exclusive`]——
    /// 一个判定不了并发性的调用串行跑只是慢，并行跑可能是数据竞争
    /// （oh-my-pi `packages/agent/src/agent-loop.ts:2700-2705`）。
    fn concurrency(&self, args: &Value) -> Concurrency {
        let _ = args;
        Concurrency::Exclusive
    }

    /// 本工具能否被软中断（插话）打断。
    ///
    /// 只有**纯等待**类工具（sleep、轮询、等待后台作业）该返回 `true`。任何会产生副作用的
    /// 工具都必须是 `false`，否则用户插一句话就可能把一次写到一半的编辑打断。
    fn interruptible(&self, args: &Value) -> bool {
        let _ = args;
        false
    }

    /// 执行。
    async fn execute(&self, args: Value, ctx: ToolContext) -> Result<ToolOutput, ToolError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct Noop;

    #[async_trait]
    impl Tool for Noop {
        fn name(&self) -> &'static str {
            "noop"
        }
        fn description(&self) -> &'static str {
            "什么也不做"
        }
        fn parameters(&self) -> Value {
            serde_json::json!({ "type": "object", "properties": {} })
        }
        async fn execute(&self, _args: Value, _ctx: ToolContext) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput::text("ok"))
        }
    }

    #[test]
    fn defaults_are_the_conservative_ones() {
        let tool = Noop;
        let args = Value::Null;
        assert_eq!(
            tool.approval(&args).tier,
            crate::approval::Tier::Exec,
            "没声明档位必须按最危险处理"
        );
        assert_eq!(
            tool.concurrency(&args),
            Concurrency::Exclusive,
            "没声明并发性必须串行——与审批默认同一个方向"
        );
        assert_eq!(Concurrency::default(), Concurrency::Exclusive);
        assert!(!tool.interruptible(&args), "默认不可被插话打断");
    }

    #[test]
    fn for_subcall_only_swaps_the_call_id() {
        let (progress, _rx) = mpsc::unbounded_channel();
        let ctx = ToolContext {
            session_id: SessionId::generate(),
            entry_id: EntryId::generate(),
            call_id: "call_1".to_owned(),
            cwd: PathBuf::from("/tmp"),
            cancel: InterruptSignal::new(),
            steering: InterruptSignal::new(),
            progress,
        };
        let child = ctx.for_subcall("call_2");
        assert_eq!(child.call_id, "call_2");
        assert_eq!(child.session_id, ctx.session_id);
        assert!(
            child.cancel.same_instance(&ctx.cancel),
            "子调用必须共享同一个取消信号，否则取消漏掉子调用"
        );
    }

    #[test]
    fn text_len_counts_image_payloads_too() {
        let output = ToolOutput::text("abcd");
        assert_eq!(output.text_len(), 4);
        let with_image = ToolOutput::image(StoredImage {
            media_type: "image/png".to_owned(),
            data: "AAAA".to_owned(),
        });
        assert_eq!(with_image.text_len(), 4);
    }
}
