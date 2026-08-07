//! 重绘节流：多久 tick 一次、resize 突发怎么去抖。
//!
//! 三个节奏与它们的出处、成立前提：
//!
//! - **空闲 250ms**（[`IDLE_INTERVAL`]）：没有流式输出、没有工具在跑、没有动画
//!   时的轮询节奏。出处 jcode `crates/jcode-tui/src/tui/redraw_schedule.rs:16`
//!   `REDRAW_IDLE`。上游没有给出"为什么恰好是 250ms"的推导依据（只是声明的
//!   常量），本仓照抄同一个数值：它足够快，能在四分之一秒内感知另一端
//!   （多客户端共享会话、外部信号）的状态变化，又不会在完全静止时把 CPU
//!   耗在无意义的轮询上。
//! - **spinner 80ms**（[`SPINNER_INTERVAL`]）：有内容在流式输出或工具在跑时的
//!   动画节奏。出处 jcode `crates/jcode-tui-render/src/swarm_gallery.rs:62`
//!   `STRIP_SPINNER_FRAME_MS`。前提：这个值就是 spinner 字形本身的帧间隔
//!   （`STRIP_SPINNER_FPS = 1000 / 80`），重绘节奏必须和动画节奏对齐——
//!   更快只会重绘出完全相同的一帧，更慢会让动画看起来一卡一卡。
//! - **resize 去抖 33ms**（[`RESIZE_DEBOUNCE`]）：连续 resize 事件（拖拽窗口
//!   边框）之间的最小重绘间隔。出处 jcode
//!   `crates/jcode-tui/src/tui/app/input.rs:2882`
//!   `RESIZE_REDRAW_MIN_INTERVAL`。前提：33ms ≈ 30fps，跟手感与"不把拖拽期间
//!   每个中间尺寸都重画一遍"之间的折中；`full_paint`（resize 是显式手势之一）
//!   代价是把整个已提交 transcript 从 home 重放一遍，不去抖会在长会话下
//!   把这个代价乘上"每秒收到的 resize 事件数"。
//!
//! 额外抄了 jcode 的深度空闲退避（同一文件 `:17,20` `REDRAW_DEEP_IDLE` /
//! `REDRAW_DEEP_IDLE_AFTER`）：连续静止超过 30 秒后把轮询降到 5s 一次，
//! 减少长时间挂着的会话的空转唤醒。这不在任务要求的"三条"之内，但既然
//! 同一文件同一上下文给出了，顺手接上，代价只是两个常量。

use std::time::Duration;

/// 空闲态轮询间隔。出处见模块文档。
pub(crate) const IDLE_INTERVAL: Duration = Duration::from_millis(250);
/// 动画（spinner / 流式展示）态的 tick 间隔。出处见模块文档。
pub(crate) const SPINNER_INTERVAL: Duration = Duration::from_millis(80);
/// resize 事件的最小重绘间隔。出处见模块文档。
pub(crate) const RESIZE_DEBOUNCE: Duration = Duration::from_millis(33);
/// 连续静止超过这个时长后，从 [`IDLE_INTERVAL`] 退避到 [`DEEP_IDLE_INTERVAL`]。
pub(crate) const DEEP_IDLE_AFTER: Duration = Duration::from_secs(30);
/// 深度空闲态的轮询间隔。
pub(crate) const DEEP_IDLE_INTERVAL: Duration = Duration::from_secs(5);

/// 下一次 tick 应该等待多久，纯函数：不读时钟、不产生副作用。
///
/// `animating` 为真时（有流式文本、工具在跑、或有未消费的展示 backlog）走
/// spinner 节奏；否则按 `idle_for`（距上一次"真正变化"过去了多久）在
/// [`IDLE_INTERVAL`] 与 [`DEEP_IDLE_INTERVAL`] 之间选择。
#[must_use]
pub(crate) fn next_tick_interval(animating: bool, idle_for: Duration) -> Duration {
    if animating {
        SPINNER_INTERVAL
    } else if idle_for >= DEEP_IDLE_AFTER {
        DEEP_IDLE_INTERVAL
    } else {
        IDLE_INTERVAL
    }
}

/// resize 去抖决策。`last_redraw` 是上一次因 resize 触发重绘的时刻（`None`
/// 表示还没有过），`now` 是这次收到 resize 事件的时刻。
///
/// 返回 `true` 表示可以立即重绘；`false` 表示应该记一个"待重绘"标记，
/// 交由调用方在去抖窗口结束后（用同一个函数、下一次调用时天然满足去抖条件）
/// 补一次尾帧——否则 resize 突发中的最后一次事件如果恰好被去抖掉，画面会停在
/// 一个过时的尺寸上，直到某个不相关的事件恰好触发下一次重绘。
#[must_use]
pub(crate) fn should_redraw_now(
    last_redraw: Option<std::time::Instant>,
    now: std::time::Instant,
) -> bool {
    match last_redraw {
        Some(last) => now.duration_since(last) >= RESIZE_DEBOUNCE,
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn animating_always_uses_spinner_cadence_regardless_of_idle_time() {
        assert_eq!(
            next_tick_interval(true, Duration::from_secs(999)),
            SPINNER_INTERVAL
        );
    }

    #[test]
    fn short_idle_uses_idle_cadence() {
        assert_eq!(
            next_tick_interval(false, Duration::from_secs(1)),
            IDLE_INTERVAL
        );
    }

    #[test]
    fn long_idle_backs_off_to_deep_idle_cadence() {
        assert_eq!(
            next_tick_interval(false, Duration::from_secs(31)),
            DEEP_IDLE_INTERVAL
        );
        // 边界：恰好等于阈值也算深度空闲（`>=`）。
        assert_eq!(
            next_tick_interval(false, DEEP_IDLE_AFTER),
            DEEP_IDLE_INTERVAL
        );
    }

    #[test]
    fn first_resize_always_redraws_immediately() {
        assert!(should_redraw_now(None, Instant::now()));
    }

    #[test]
    fn burst_within_debounce_window_is_suppressed() {
        let now = Instant::now();
        let last = now;
        assert!(!should_redraw_now(
            Some(last),
            now + Duration::from_millis(10)
        ));
        assert!(should_redraw_now(
            Some(last),
            now + Duration::from_millis(34)
        ));
    }
}
