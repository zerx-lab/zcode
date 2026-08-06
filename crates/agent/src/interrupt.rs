//! 中断信号：同步可读 + 异步可等的取消原语。
//!
//! 移植自 jcode `crates/jcode-agent-runtime/src/lib.rs:32-118`。**刻意不用**
//! `tokio_util::sync::CancellationToken`，三条语义是它给不了的：
//!
//! 1. **同步可读**。工具内部的紧循环要能在不 `.await` 的前提下查取消位
//!    （[`InterruptSignal::is_set`]），而 `CancellationToken::is_cancelled` 虽然也同步，
//!    但下面两条它没有。
//! 2. **`notified()` 必须先注册 waiter 再查 flag**。`Notify::notify_waiters()` 只唤醒
//!    *已注册* 的 waiter；先查 flag 再注册会丢掉这中间发生的一次 fire，症状是取消被挂到
//!    下一个无关事件到来时才生效（jcode issue #428）。turn 的流循环对每个 stream 事件都重建
//!    一次这个 future，快速 token 流下这个竞态是高频命中而非理论风险。
//! 3. **epoch 保护延迟 reset**。取消常常由"投递方"发起、由"延时清理方"复位。若清理方不带
//!    epoch 判据，两次连续取消会互相抵消：第二次 fire 被第一次的延时 reset 抹掉，而目标
//!    还没观测到。[`InterruptSignal::reset_if_epoch`] 先比 epoch、清完再复核一次，发现竞态
//!    就把 fire 恢复。
//!
//! # 谁负责 reset
//!
//! 经注册表 / 外部 handle fire 的信号，**没有任何其他人会清它**。持有该信号的一次 turn
//! 结束时必须自己 [`InterruptSignal::reset`]（见 [`crate::turn`] 的 RAII guard），否则残留
//! 的置位会让下一个 turn 秒退。这是移植时最容易漏的一条。

use std::pin::pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use tokio::sync::Notify;

/// 一个可克隆的中断信号：所有克隆共享同一份状态。
#[derive(Debug, Clone, Default)]
pub struct InterruptSignal {
    flag: Arc<AtomicBool>,
    /// 单调递增的 fire 计数。持有延时 reset 的一方靠它发现"期间又被 fire 过"，
    /// 从而跳过 reset，而不是抹掉目标尚未观测到的取消。
    epoch: Arc<AtomicU64>,
    notify: Arc<Notify>,
}

impl InterruptSignal {
    /// 构造一个未置位的信号。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 置位并唤醒所有等待者。
    ///
    /// 顺序不可交换：先递增 epoch，再置 flag，最后唤醒。等待方的顺序是镜像的
    /// （先注册 waiter 再查 flag），两侧配合才能保证既不丢 wakeup 也不丢 flag。
    pub fn fire(&self) {
        self.epoch.fetch_add(1, Ordering::AcqRel);
        self.flag.store(true, Ordering::Release);
        self.notify.notify_waiters();
    }

    /// 当前是否已置位。
    #[must_use]
    pub fn is_set(&self) -> bool {
        self.flag.load(Ordering::Acquire)
    }

    /// 无条件复位。
    ///
    /// 只有"确定自己是该信号唯一使用者"的一方才该调它——通常是 turn 结束时的 RAII guard。
    /// 延时清理一律走 [`InterruptSignal::reset_if_epoch`]。
    pub fn reset(&self) {
        self.flag.store(false, Ordering::Release);
    }

    /// 当前 fire 计数。配合 [`InterruptSignal::reset_if_epoch`] 使用。
    #[must_use]
    pub fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::Acquire)
    }

    /// 仅当 epoch 未变时复位；返回是否真的复位了。
    ///
    /// 清完之后**再复核一次** epoch：清理与新的 `fire()` 可能交错，此时新 fire 的置位已经被
    /// 我们抹掉，必须原样恢复（置位 + 重新唤醒），否则那次取消永久丢失。
    #[must_use]
    pub fn reset_if_epoch(&self, epoch: u64) -> bool {
        if self.epoch() != epoch {
            return false;
        }
        self.flag.store(false, Ordering::Release);
        if self.epoch() == epoch {
            return true;
        }
        // 竞态：清理期间又被 fire。恢复被我们抹掉的那次取消。
        self.flag.store(true, Ordering::Release);
        self.notify.notify_waiters();
        false
    }

    /// 等待信号置位；已经置位时立即返回。
    ///
    /// **先注册 waiter，再查 flag**。反过来会丢掉两步之间发生的那次 fire。
    pub async fn notified(&self) {
        let mut notified = pin!(self.notify.notified());
        // 显式注册：把"future 创建即注册"这个 tokio 实现细节变成本模块的显式依赖。
        notified.as_mut().enable();
        if self.is_set() {
            return;
        }
        notified.await;
    }

    /// 两个句柄是否指向同一份状态。
    ///
    /// 取消要广播给"可能是多个实例"的信号时，用它避免对同一实例重复 fire。
    #[must_use]
    pub fn same_instance(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.flag, &other.flag)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::time::timeout;

    use super::*;

    #[tokio::test]
    async fn notified_returns_immediately_when_already_set() {
        let signal = InterruptSignal::new();
        signal.fire();
        timeout(Duration::from_secs(1), signal.notified())
            .await
            .expect("已置位的信号必须立即返回");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn notified_never_misses_a_concurrent_fire() {
        // 锤这个竞态：waiter 注册与 fire 交错 2000 次，任何一次丢 wakeup 都会超时。
        for _ in 0..2000 {
            let signal = InterruptSignal::new();
            let firing = signal.clone();
            let handle = tokio::spawn(async move {
                tokio::task::yield_now().await;
                firing.fire();
            });
            timeout(Duration::from_secs(5), signal.notified())
                .await
                .expect("并发 fire 必须被观测到");
            handle.await.expect("fire 任务不应 panic");
        }
    }

    #[test]
    fn reset_if_epoch_skips_when_fired_again() {
        let signal = InterruptSignal::new();
        signal.fire();
        let epoch = signal.epoch();
        signal.fire();
        assert!(!signal.reset_if_epoch(epoch));
        assert!(signal.is_set(), "较新的 fire 不得被过期的 reset 抹掉");
    }

    #[test]
    fn reset_if_epoch_resets_when_epoch_matches() {
        let signal = InterruptSignal::new();
        signal.fire();
        assert!(signal.reset_if_epoch(signal.epoch()));
        assert!(!signal.is_set());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_fire_survives_a_racing_reset() {
        for _ in 0..2000 {
            let signal = InterruptSignal::new();
            signal.fire();
            let epoch = signal.epoch();
            let firing = signal.clone();
            let handle = tokio::spawn(async move { firing.fire() });
            let did_reset = signal.reset_if_epoch(epoch);
            handle.await.expect("fire 任务不应 panic");
            // 唯一可断言的不变量：并发的那次 fire 绝不能凭空消失。
            // `did_reset == true` 时 fire 可能发生在 reset 完成之后，此刻 flag 为真也是对的，
            // 因此不对该分支的 flag 取值下断言。
            assert!(
                did_reset || signal.is_set(),
                "reset 被竞态挡下时，那次 fire 必须仍然可见"
            );
        }
    }

    #[test]
    fn same_instance_distinguishes_clones_from_new_signals() {
        let signal = InterruptSignal::new();
        assert!(signal.same_instance(&signal.clone()));
        assert!(!signal.same_instance(&InterruptSignal::new()));
    }
}
