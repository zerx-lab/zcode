//! 流式文本的展示节奏：到达 ≠ 展示。
//!
//! provider 的增量到达节奏千差万别（OpenAI 逐 token、Anthropic 按
//! `content_block_delta` 成块到达），原样展示会让 UI 一卡一卡地"跳字"。本模块
//! 把"到达"和"展示"解耦：增量先进 backlog，一个按时间步进、随 backlog 增大而
//! 加速的比例控制器再把它匀速吐出来。
//!
//! 算法与全部常量抄自 jcode
//! `crates/jcode-tui-core/src/stream_buffer.rs:32,37,46,51`（`reveal_now`
//! 函数体 `:257-297`），前提：
//!
//! - `BASE_REVEAL_CPS = 180`：backlog 为空时的稳态展示速率（字符/秒），
//!   决定一段小突发尾部收尾的观感速度；
//! - `REVEAL_BACKLOG_GAIN = 3`：backlog 每多 1 字符，速率再加 3 字符/秒——
//!   backlog 越大追得越快，稳态 backlog 收敛在 `(到达速率 - 180) / 3`；
//! - `MAX_REVEAL_CPS = 960`：硬上限。没有它，provider 一次性甩来一大段文本时
//!   控制器会在一帧里吐出几十行；按经过时间（而不是"每帧固定字符数"）限速，
//!   使得 16ms 与 50ms 两种重绘节奏观感一致，同时几秒内仍能把大 backlog 排空；
//! - `MAX_REVEAL_STEP = 50ms`：单步最多计入的经过时间。没有它，两次 tick
//!   之间的空档（连接延迟、工具执行间隙）会攒出一大笔预算，下一段突发到达时
//!   瞬间全部吐出，重新引入本模块想消除的"一卡一卡"。
//!
//! # 定点整数，不用浮点
//!
//! 上游用 `f32` 累加器；本仓 `clippy::as_conversions` 与 `cast_*` 全部 deny，
//! 而标准库没有 `f32 -> usize` 的 `TryFrom`。改用毫字符（1 字符 = 1000
//! milli-char）定点整数累加器，数学上与 `cps(字符/秒) * dt(毫秒) = 字符/1000`
//! 恒等：`180 字符/秒 * 50 毫秒 = 9000 milli-char = 9 字符`，与浮点版本逐步
//! 对齐，且全程只用 `saturating_*`/`checked_*` 与 `try_from`，不产生新的
//! `unwrap`/`panic`/`as` 风险。

use std::time::{Duration, Instant};

/// 定点精度：1 字符 = 1000 milli-char。
const MILLI: u64 = 1000;

/// 出处见模块文档。
const BASE_REVEAL_CPS: u64 = 180;
/// 出处见模块文档。
const REVEAL_BACKLOG_GAIN: u64 = 3;
/// 出处见模块文档。
const MAX_REVEAL_CPS: u64 = 960;
/// 出处见模块文档。
const MAX_REVEAL_STEP: Duration = Duration::from_millis(50);

/// 一个展示节奏控制器，管理"backlog 里还有多少字符尚未展示"到"这一步应该新
/// 展示几个字符"的换算。每个正在流式输出的内容块各自持有一个实例：
/// 用户消息、已定稿的历史消息不需要它。
#[derive(Debug, Clone)]
pub(crate) struct RevealPacer {
    /// 比例控制器的毫字符预算，可跨步骤累积小数部分（否则慢速率永远舍入到 0）。
    carry_milli: u64,
    /// 硬上限控制器的独立毫字符预算，与 `carry_milli` 各自累积、取较小者生效——
    /// 复刻上游"两个独立 token bucket"的设计（`stream_buffer.rs:275,278-279`）：
    /// 若共用一个预算，短时间内多次很小 `dt` 的调用会各自免费拿到一个字符，
    /// 累计起来超过按墙钟计算的硬上限。
    ceiling_carry_milli: u64,
    last_reveal: Instant,
}

impl RevealPacer {
    /// 新建一个控制器，`now` 是首次调用 [`RevealPacer::step`] 前的参考时刻。
    #[must_use]
    pub(crate) fn new(now: Instant) -> Self {
        Self {
            carry_milli: 0,
            ceiling_carry_milli: 0,
            last_reveal: now,
        }
    }

    /// 给定当前时刻与 backlog 中尚未展示的字符数，返回这一步应该新展示多少个
    /// 字符（可能为 0）。调用方负责把返回值累加进"已展示字符数"游标。
    ///
    /// `backlog_chars == 0` 时重置累加器并返回 0：空 backlog 期间不能攒预算，
    /// 否则下一段突发到达时会被瞬间吐出（`stream_buffer.rs:258-265`）。
    pub(crate) fn step(&mut self, now: Instant, backlog_chars: usize) -> usize {
        if backlog_chars == 0 {
            self.carry_milli = 0;
            self.ceiling_carry_milli = 0;
            self.last_reveal = now;
            return 0;
        }

        let elapsed = now
            .saturating_duration_since(self.last_reveal)
            .min(MAX_REVEAL_STEP);
        self.last_reveal = now;
        let dt_ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);

        let backlog = u64::try_from(backlog_chars).unwrap_or(u64::MAX);
        let cps = BASE_REVEAL_CPS.saturating_add(backlog.saturating_mul(REVEAL_BACKLOG_GAIN));
        self.carry_milli = self.carry_milli.saturating_add(cps.saturating_mul(dt_ms));
        self.ceiling_carry_milli = self
            .ceiling_carry_milli
            .saturating_add(MAX_REVEAL_CPS.saturating_mul(dt_ms));

        let controller_budget = self.carry_milli / MILLI;
        let ceiling_budget = self.ceiling_carry_milli / MILLI;
        let mut reveal = controller_budget.min(ceiling_budget);
        if reveal == 0 {
            return 0;
        }

        reveal = reveal.min(backlog);
        self.carry_milli = self
            .carry_milli
            .saturating_sub(reveal.saturating_mul(MILLI));
        self.ceiling_carry_milli = self
            .ceiling_carry_milli
            .saturating_sub(reveal.saturating_mul(MILLI));
        usize::try_from(reveal).unwrap_or(backlog_chars)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_backlog_reveals_nothing_and_resets_carry() {
        let start = Instant::now();
        let mut pacer = RevealPacer::new(start);
        assert_eq!(pacer.step(start + Duration::from_secs(5), 0), 0);
        // 空档之后 backlog 到达：不能有攒下来的预算被瞬间吐出。
        let revealed = pacer.step(
            start + Duration::from_secs(5) + Duration::from_millis(1),
            100,
        );
        assert!(revealed <= 1, "空 backlog 之后不应攒出预算，got {revealed}");
    }

    #[test]
    fn steady_backlog_converges_to_base_rate() {
        let start = Instant::now();
        let mut pacer = RevealPacer::new(start);
        let mut now = start;
        let mut total = 0usize;
        // 持续 1 秒、每步 50ms、backlog 恒定较小（远小于能显著抬高速率的量级）。
        for _ in 0..20 {
            now += Duration::from_millis(50);
            total += pacer.step(now, 10);
        }
        // BASE_REVEAL_CPS=180，1 秒稳态附近应显示约 180 + 少量 backlog 增益，
        // 允许较宽容差，只验证量级正确、且不会一次性倾泻。
        assert!((150..=300).contains(&total), "total={total}");
    }

    #[test]
    fn large_burst_is_capped_by_ceiling_not_dumped_at_once() {
        let start = Instant::now();
        let mut pacer = RevealPacer::new(start);
        // 单步 50ms、backlog 一次性 5000 字符：硬上限 960 cps * 0.05s = 48 字符。
        let revealed = pacer.step(start + Duration::from_millis(50), 5000);
        assert!(revealed <= 48, "单步展示字符数超过硬上限: {revealed}");
        assert!(
            revealed > 0,
            "非零 backlog 不应该在有 dt 的情况下展示 0 个字符"
        );
    }

    #[test]
    fn never_reveals_more_than_backlog() {
        let start = Instant::now();
        let mut pacer = RevealPacer::new(start);
        let revealed = pacer.step(start + Duration::from_secs(10), 3);
        assert!(revealed <= 3);
    }
}
