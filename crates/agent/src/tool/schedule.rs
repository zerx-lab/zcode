//! 一批工具调用的屏障链调度：`Shared` 并跑，`Exclusive` 是全屏障。
//!
//! 语义抄源 oh-my-pi `packages/agent/src/agent-loop.ts:2631-2719`：按下标顺序遍历调用，
//! 维护 `lastExclusive`（上一个独占任务）与 `sharedTasks`（自上一个独占任务之后已发起的
//! 共享任务）两条链——
//!
//! - 一个 `Shared` 调用只等 `lastExclusive`，不等同批其他 `Shared`，因此连续的 `Shared`
//!   调用天然重叠执行（`agent-loop.ts:2706-2716`）。
//! - 一个 `Exclusive` 调用要等 `lastExclusive` **和** 当前所有 `sharedTasks` 都结束
//!   （`Promise.all([lastExclusive, ...sharedTasks])`，`agent-loop.ts:2708`），执行期间
//!   独占——它成为新的 `lastExclusive`，`sharedTasks` 清空，后面的调用重新在它之后排队。
//!
//! 上游对 `Shared` 并发**没有数字上限**：模型一次吐出多少个只读调用就开多少个 future。
//! 本仓加了 [`MAX_SHARED_PARALLEL`]，理由见其文档。
//!
//! # 移植时补的一课：`Promise.allSettled` vs `JoinHandle`
//!
//! TS 版本用 `Promise.allSettled` 收尾，一个工具内部抛异常只会让对应的 promise reject，
//! 不会波及其他 promise。Rust 的 `tokio::spawn` 则相反：一个任务内部 `panic!` 会让那个
//! `JoinHandle::await` 返回 `Err(JoinError)`，但**不会**自动波及其他任务——前提是每个
//! 工具调用各自跑在自己的 `tokio::spawn` 里，而不是全部挤在调度函数自身的 future 中
//! （后者一旦某个 `.await` 内部 panic，会直接把整个 `execute_batch` 的 future 带走）。
//! 因此这里对每个工具调用单独 `tokio::spawn`，把 `JoinError` 翻译成
//! [`crate::error::ToolError::Failed`]（或 `Cancelled`，当 `JoinError::is_cancelled()`），
//! 这一步在 TS 原版里不存在，是移植到 Rust 必须补上的部分。

use std::sync::Arc;

use futures_util::future::{BoxFuture, FutureExt, Shared};
use serde_json::Value;
use tokio::sync::{Semaphore, mpsc};

use crate::error::ToolError;
use crate::tool::{Concurrency, Tool, ToolContext, ToolOutput};

/// 同一批里 `Shared` 工具的并发上限。
///
/// 上游（oh-my-pi）没有这个数字，是三仓里唯一给出实测依据的是 jcode：
/// `crates/jcode-app-core/src/tool/batch.rs:10` 的 `MAX_PARALLEL = 10`，并且它的
/// batch 工具 schema 把 `tool_calls` 的 `maxItems` 也钉在 10（`batch.rs:33`）——
/// 两处独立地收敛到同一个数字，说明这不是随手写的常量。
///
/// 不采用"完全无上限"的原因：`Shared` 调用仍然会各自打开文件句柄、建立网络连接、
/// 甚至派生子进程（如 `fetch`/`grep`/`glob` 类只读工具），模型一次吐出几十个调用时
/// 无界并发会把这些有限资源在一瞬间打满，属于"每个调用都无害，凑一起变成资源耗尽"
/// 的经典问题；10 这个数字没有更强的理论依据，但至少是三仓中唯一有人验证过的取值，
/// 优于自己凭空拍一个。
pub const MAX_SHARED_PARALLEL: usize = 10;

/// 一个已定稿、待执行的工具调用。
///
/// "已定稿"指审批已经放行、参数已经通过 [`super::registry::ToolRegistry::validate`]——
/// 调度器不再做任何校验，只负责按并发关系把它排进执行顺序。
#[derive(Debug)]
pub struct PreparedCall {
    /// 提供商分配的调用 id，用于把结果匹配回对应的 `tool_use` 块。
    pub call_id: String,
    /// 工具名（已解析别名）。调度本身不使用它，留给调用方做日志/遥测关联。
    pub tool_name: String,
    /// 待执行的工具实现。
    pub tool: Arc<dyn Tool>,
    /// 已校验通过的参数。
    pub args: Value,
    /// 本次调用与同批其他调用的并发关系。
    pub concurrency: Concurrency,
}

/// 一次调用的结果，按原始下标回收。
///
/// [`execute_batch`] 内部各调用完成顺序不确定（`Shared` 调用天然乱序完工），
/// `index` 让调用方能把结果精确排回请求时的原始顺序——顺序错了，喂回模型的
/// `tool_result` 就会对不上对应的 `tool_use`。
#[derive(Debug)]
pub struct CallOutcome {
    /// 调用在输入 `Vec` 里的原始下标。
    pub index: usize,
    /// 提供商分配的调用 id，透传自对应 [`PreparedCall::call_id`]。
    pub call_id: String,
    /// 执行结果。
    pub outcome: Result<ToolOutput, ToolError>,
}

/// 按屏障链执行一批工具调用，返回按原始顺序排好的结果。
///
/// 屏障链语义、`MAX_SHARED_PARALLEL` 取值依据、panic 处理见模块文档。
///
/// `context_for` 在调度循环里同步调用（不进入被 `tokio::spawn` 的任务），为每个调用
/// 派生它自己的 [`ToolContext`]——典型实现是 [`ToolContext::for_subcall`]，只换
/// `call_id`，共享同一份取消/插话信号。
///
/// 返回的 `Vec` 长度恒等于 `calls.len()`：每个调用，无论成功、失败还是内部 panic，
/// 都会产出恰好一条 [`CallOutcome`]——少一条就是一个孤儿 `tool_use`，后续请求会被
/// 提供商直接拒绝。
pub async fn execute_batch(
    calls: Vec<PreparedCall>,
    context_for: impl Fn(&PreparedCall) -> ToolContext + Send + Sync,
) -> Vec<CallOutcome> {
    let total = calls.len();
    let semaphore = Arc::new(Semaphore::new(MAX_SHARED_PARALLEL));
    let (tx, mut rx) = mpsc::unbounded_channel::<CallOutcome>();

    // `lastExclusive` 与 `sharedTasks` 的 Rust 版本：`Shared<BoxFuture<'static, ()>>`
    // 是一个可以被多处 `.await` 的"完成信号"（不是结果本身——结果经 `tx` 单独送出）。
    let mut last_exclusive: Option<Shared<BoxFuture<'static, ()>>> = None;
    let mut pending_shared: Vec<Shared<BoxFuture<'static, ()>>> = Vec::new();

    for (index, call) in calls.into_iter().enumerate() {
        let concurrency = call.concurrency;
        let ctx = context_for(&call);
        let PreparedCall {
            call_id,
            tool,
            args,
            ..
        } = call;

        // `Shared` 只等上一个独占任务；`Exclusive` 还要等自那之后发起的所有 `Shared`。
        let wait_exclusive = last_exclusive.clone();
        let wait_shared = if concurrency == Concurrency::Exclusive {
            std::mem::take(&mut pending_shared)
        } else {
            Vec::new()
        };
        let semaphore = Arc::clone(&semaphore);
        let tx = tx.clone();

        let work: BoxFuture<'static, ()> = async move {
            if let Some(barrier) = wait_exclusive {
                barrier.await;
            }
            for barrier in wait_shared {
                barrier.await;
            }

            // 只有 `Shared` 受并发信号量约束；`Exclusive` 本来就独占，不需要许可证。
            let _permit = if concurrency == Concurrency::Shared {
                let Ok(permit) = semaphore.acquire_owned().await else {
                    // `Semaphore` 从不被 `close()`，这条分支在实践中不可达；即便如此也要
                    // 产出一条结果而不是静默吞掉——绝不能让一次调用凭空消失。
                    let _ = tx.send(CallOutcome {
                        index,
                        call_id,
                        outcome: Err(ToolError::Failed("共享并发信号量已关闭".to_owned())),
                    });
                    return;
                };
                Some(permit)
            } else {
                None
            };

            // 单独 `tokio::spawn`：工具内部 panic 只毒化这一个 `JoinHandle`，翻译成
            // `ToolError` 后照常送出结果，不会带垮整批（原因见模块文档）。
            let handle = tokio::spawn(async move { tool.execute(args, ctx).await });
            let outcome = match handle.await {
                Ok(result) => result,
                Err(join_error) if join_error.is_cancelled() => Err(ToolError::Cancelled),
                Err(join_error) => Err(ToolError::Failed(format!("工具执行 panic: {join_error}"))),
            };
            let _ = tx.send(CallOutcome {
                index,
                call_id,
                outcome,
            });
        }
        .boxed();

        let signal = work.shared();
        // 驱动 `signal` 真正跑起来；`signal` 的其余克隆只用来等它完成，不会重复执行。
        tokio::spawn(signal.clone());

        if concurrency == Concurrency::Exclusive {
            last_exclusive = Some(signal);
            pending_shared.clear();
        } else {
            pending_shared.push(signal);
        }
    }

    drop(tx);
    let mut outcomes = Vec::with_capacity(total);
    while let Some(outcome) = rx.recv().await {
        outcomes.push(outcome);
    }
    outcomes.sort_by_key(|outcome| outcome.index);
    outcomes
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Duration;

    use async_trait::async_trait;
    use serde_json::json;
    use tokio::sync::{Barrier, mpsc as tokio_mpsc};

    use super::*;
    use crate::id::{EntryId, SessionId};
    use crate::interrupt::InterruptSignal;

    /// 记录并发状态的假工具：进入时登记、退出时销记，可选择用 [`Barrier`] 证明"确实重叠"，
    /// 也可选择直接 panic 以验证批次不会被一个坏工具带垮。
    #[derive(Debug)]
    struct RecordingTool {
        concurrency: Concurrency,
        active: Arc<AtomicUsize>,
        peak: Arc<AtomicUsize>,
        exclusive_running: Arc<AtomicBool>,
        overlap_violation: Arc<AtomicBool>,
        barrier: Option<Arc<Barrier>>,
        should_panic: bool,
    }

    #[async_trait]
    impl Tool for RecordingTool {
        fn name(&self) -> &'static str {
            "recording"
        }

        fn description(&self) -> &'static str {
            "test-only"
        }

        fn parameters(&self) -> Value {
            json!({ "type": "object", "properties": {} })
        }

        fn concurrency(&self, _args: &Value) -> Concurrency {
            self.concurrency
        }

        async fn execute(&self, _args: Value, _ctx: ToolContext) -> Result<ToolOutput, ToolError> {
            if self.concurrency == Concurrency::Exclusive && self.active.load(Ordering::SeqCst) != 0
            {
                self.overlap_violation.store(true, Ordering::SeqCst);
            }
            if self.concurrency != Concurrency::Exclusive
                && self.exclusive_running.load(Ordering::SeqCst)
            {
                self.overlap_violation.store(true, Ordering::SeqCst);
            }
            if self.concurrency == Concurrency::Exclusive {
                self.exclusive_running.store(true, Ordering::SeqCst);
            }

            let now_active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(now_active, Ordering::SeqCst);

            if let Some(barrier) = &self.barrier {
                // 真正证明"并发"：如果调度器把这些调用串行化了，凑不齐参与者数量，
                // `wait()` 会一直挂着，外层的 `timeout` 会让测试失败而不是误报通过。
                barrier.wait().await;
            } else {
                tokio::time::sleep(Duration::from_millis(30)).await;
            }

            self.active.fetch_sub(1, Ordering::SeqCst);
            if self.concurrency == Concurrency::Exclusive {
                self.exclusive_running.store(false, Ordering::SeqCst);
            }

            assert!(!self.should_panic, "boom: 模拟工具内部 panic");
            Ok(ToolOutput::text("done"))
        }
    }

    fn make_ctx() -> ToolContext {
        let (progress, _rx) = tokio_mpsc::unbounded_channel();
        ToolContext {
            session_id: SessionId::generate(),
            entry_id: EntryId::generate(),
            call_id: "unused".to_owned(),
            cwd: PathBuf::from("/tmp"),
            cancel: InterruptSignal::new(),
            steering: InterruptSignal::new(),
            progress,
        }
    }

    fn prepared(index: usize, tool: Arc<dyn Tool>, concurrency: Concurrency) -> PreparedCall {
        PreparedCall {
            call_id: format!("call_{index}"),
            tool_name: "recording".to_owned(),
            tool,
            args: Value::Null,
            concurrency,
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn shared_calls_truly_overlap() {
        let barrier = Arc::new(Barrier::new(3));
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let exclusive_running = Arc::new(AtomicBool::new(false));
        let violation = Arc::new(AtomicBool::new(false));

        let calls: Vec<PreparedCall> = (0..3)
            .map(|i| {
                let tool = Arc::new(RecordingTool {
                    concurrency: Concurrency::Shared,
                    active: Arc::clone(&active),
                    peak: Arc::clone(&peak),
                    exclusive_running: Arc::clone(&exclusive_running),
                    overlap_violation: Arc::clone(&violation),
                    barrier: Some(Arc::clone(&barrier)),
                    should_panic: false,
                });
                prepared(i, tool, Concurrency::Shared)
            })
            .collect();

        // 若调度器错误地把这三个 `Shared` 调用串行化，三方 `Barrier` 永远凑不齐，
        // 这里会一直挂到超时——用超时代替误判为"通过"。
        let outcomes =
            tokio::time::timeout(Duration::from_secs(5), execute_batch(calls, |_| make_ctx()))
                .await
                .expect("三个 Shared 调用必须真正重叠，否则 Barrier 会挂住导致超时");

        assert_eq!(outcomes.len(), 3);
        assert!(outcomes.iter().all(|o| o.outcome.is_ok()));
        assert!(!violation.load(Ordering::SeqCst));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn exclusive_never_overlaps_its_neighbors() {
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let exclusive_running = Arc::new(AtomicBool::new(false));
        let violation = Arc::new(AtomicBool::new(false));

        let make_tool = |concurrency: Concurrency| {
            let tool: Arc<dyn Tool> = Arc::new(RecordingTool {
                concurrency,
                active: Arc::clone(&active),
                peak: Arc::clone(&peak),
                exclusive_running: Arc::clone(&exclusive_running),
                overlap_violation: Arc::clone(&violation),
                barrier: None,
                should_panic: false,
            });
            tool
        };

        // Shared, Shared, Exclusive, Shared, Shared —— 中间的 Exclusive 两侧都不能
        // 与它重叠，但两侧各自的 Shared 对之间可以重叠。
        let concurrencies = [
            Concurrency::Shared,
            Concurrency::Shared,
            Concurrency::Exclusive,
            Concurrency::Shared,
            Concurrency::Shared,
        ];
        let calls: Vec<PreparedCall> = concurrencies
            .into_iter()
            .enumerate()
            .map(|(i, concurrency)| prepared(i, make_tool(concurrency), concurrency))
            .collect();

        let outcomes = execute_batch(calls, |_| make_ctx()).await;

        assert_eq!(outcomes.len(), 5);
        assert!(outcomes.iter().all(|o| o.outcome.is_ok()));
        assert!(
            !violation.load(Ordering::SeqCst),
            "Exclusive 调用必须是全屏障：前后都不能有其他调用与它同时在跑"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn shared_concurrency_is_capped() {
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let exclusive_running = Arc::new(AtomicBool::new(false));
        let violation = Arc::new(AtomicBool::new(false));

        let calls: Vec<PreparedCall> = (0..(MAX_SHARED_PARALLEL * 2))
            .map(|i| {
                let tool = Arc::new(RecordingTool {
                    concurrency: Concurrency::Shared,
                    active: Arc::clone(&active),
                    peak: Arc::clone(&peak),
                    exclusive_running: Arc::clone(&exclusive_running),
                    overlap_violation: Arc::clone(&violation),
                    barrier: None,
                    should_panic: false,
                });
                prepared(i, tool, Concurrency::Shared)
            })
            .collect();

        let outcomes = execute_batch(calls, |_| make_ctx()).await;

        assert_eq!(outcomes.len(), MAX_SHARED_PARALLEL * 2);
        assert!(
            peak.load(Ordering::SeqCst) <= MAX_SHARED_PARALLEL,
            "并发峰值不得超过 MAX_SHARED_PARALLEL"
        );
        assert_eq!(
            peak.load(Ordering::SeqCst),
            MAX_SHARED_PARALLEL,
            "调用数是上限的两倍时应该真的跑满上限，而不是意外被序列化"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_panicking_tool_does_not_poison_the_batch() {
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let exclusive_running = Arc::new(AtomicBool::new(false));
        let violation = Arc::new(AtomicBool::new(false));

        let make_tool = |should_panic: bool| {
            let tool: Arc<dyn Tool> = Arc::new(RecordingTool {
                concurrency: Concurrency::Shared,
                active: Arc::clone(&active),
                peak: Arc::clone(&peak),
                exclusive_running: Arc::clone(&exclusive_running),
                overlap_violation: Arc::clone(&violation),
                barrier: None,
                should_panic,
            });
            tool
        };

        let calls: Vec<PreparedCall> = vec![
            prepared(0, make_tool(false), Concurrency::Shared),
            prepared(1, make_tool(true), Concurrency::Shared),
            prepared(2, make_tool(false), Concurrency::Shared),
        ];

        let outcomes = execute_batch(calls, |_| make_ctx()).await;

        assert_eq!(outcomes.len(), 3, "一个调用 panic 也不能丢失任何一条结果");
        let indices: Vec<usize> = outcomes.iter().map(|o| o.index).collect();
        assert_eq!(indices, vec![0, 1, 2], "结果必须按原始下标排序");

        assert!(outcomes.first().is_some_and(|o| o.outcome.is_ok()));
        assert!(outcomes.get(2).is_some_and(|o| o.outcome.is_ok()));
        match outcomes.get(1) {
            Some(CallOutcome {
                outcome: Err(ToolError::Failed(message)),
                ..
            }) => {
                assert!(message.contains("panic"), "{message}");
            }
            other => panic!("预期下标 1 的调用因 panic 变成 ToolError::Failed，实际是 {other:?}"),
        }
    }
}
