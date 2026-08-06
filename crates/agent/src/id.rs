//! 会话与条目标识符：字典序即时间序，进程内严格单调。
//!
//! 形状抄自 opencode `packages/schema/src/identifier.ts:6-29`（时间位在前 + 随机位在后，
//! 因此 `a < b` 就是"a 更早"，全仓可以用字符串比较代替时间戳比较），但**修掉了它的两个缺陷**：
//!
//! - 上游的 `lastTimestamp` / `counter` 是模块级可变全局，同一毫秒内靠 12 位 counter 保证
//!   单调，**溢出（>4096/ms）会污染时间位且无保护**（`identifier.ts:3-4,24-30`）。本实现把
//!   `(毫秒 << 12 | 计数)` 整体放进一个 `AtomicU64`，生成时取 `max(当前时刻, 上一个 + 1)`，
//!   溢出自然向"下一毫秒"借位，时间位永远不会倒退，也永远不会撞。
//! - 上游把随机位拼成 base62，字典序依赖字符集顺序。本实现全程 Crockford base32
//!   （`0-9A-HJKMNP-TV-Z`，字典序与数值序一致），时间位与随机位共用一套字母表。
//!
//! ID 形状：`<前缀>_<13 位时间><8 位随机>`。13 位 base32 恰好覆盖 64 位时间戳字段
//! （65 位容量），8 位随机 = 40 bit，用于区分"同一 stamp 被多个进程生成"的情况——
//! 单进程内 stamp 已经互不相同，随机位只保护多进程共写同一个会话目录的场景。

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// 同一毫秒内的序号位宽。溢出向下一毫秒借位，不会污染时间位。
const COUNTER_BITS: u32 = 12;
/// 时间戳字段编码出的 base32 字符数（13 × 5 = 65 ≥ 64 bit）。
const STAMP_CHARS: usize = 13;
/// 随机后缀的 base32 字符数（8 × 5 = 40 bit）。
const RANDOM_CHARS: usize = 8;
/// Crockford base32 字母表：剔除 I / L / O / U，字典序与数值序一致。
const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

static LAST_STAMP: AtomicU64 = AtomicU64::new(0);

/// 取下一个严格单调的 stamp：`毫秒 << COUNTER_BITS | 序号`。
fn next_stamp() -> u64 {
    let now = now_millis() << COUNTER_BITS;
    let mut prev = LAST_STAMP.load(Ordering::Relaxed);
    loop {
        let next = if now > prev {
            now
        } else {
            prev.saturating_add(1)
        };
        match LAST_STAMP.compare_exchange_weak(prev, next, Ordering::AcqRel, Ordering::Relaxed) {
            Ok(_) => return next,
            Err(observed) => prev = observed,
        }
    }
}

/// 当前 Unix 毫秒。时钟早于纪元（只可能是系统时钟被手工设坏）时取 0——
/// 单调性由 [`next_stamp`] 的"上一个 + 1"分支兜底，不依赖系统时钟正确。
#[must_use]
pub fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| u64::try_from(elapsed.as_millis()).ok())
        .unwrap_or(0)
}

/// 把 `value` 的低 `chars * 5` 位按 Crockford base32 高位优先写进 `out`。
fn push_base32(out: &mut String, value: u64, chars: usize) {
    for index in (0..chars).rev() {
        let shift = index * 5;
        let digit = if shift >= 64 {
            0
        } else {
            usize::try_from((value >> shift) & 0x1f).unwrap_or(0)
        };
        out.push(char::from(*ALPHABET.get(digit).unwrap_or(&b'0')));
    }
}

/// 40 bit 随机后缀。熵源不可用时退化为 0——ID 的唯一性由单调 stamp 保证，
/// 随机位只是多进程场景下的额外保险，缺了它不影响单进程正确性。
fn random_suffix() -> u64 {
    let mut entropy = [0_u8; 5];
    if getrandom::fill(&mut entropy).is_err() {
        return 0;
    }
    entropy
        .iter()
        .fold(0_u64, |acc, byte| (acc << 8) | u64::from(*byte))
}

/// 生成一个带前缀的标识符。
fn generate(prefix: &str) -> String {
    let mut id = String::with_capacity(prefix.len() + 1 + STAMP_CHARS + RANDOM_CHARS);
    id.push_str(prefix);
    id.push('_');
    push_base32(&mut id, next_stamp(), STAMP_CHARS);
    push_base32(&mut id, random_suffix(), RANDOM_CHARS);
    id
}

/// 为标识符 newtype 生成公共实现。
macro_rules! id_newtype {
    ($(#[$meta:meta])* $name:ident, $prefix:literal) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// 生成一个新的标识符。
            #[must_use]
            pub fn generate() -> Self {
                Self(generate($prefix))
            }

            /// 借用底层字符串。
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

id_newtype!(
    /// 会话标识符。
    SessionId,
    "ses"
);
id_newtype!(
    /// 会话条目标识符：出现在每条 JSONL 的 `id` 与 `parent_id` 上。
    EntryId,
    "ent"
);

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn ids_carry_their_prefix_and_fixed_width() {
        let id = SessionId::generate();
        assert!(id.as_str().starts_with("ses_"));
        assert_eq!(id.as_str().len(), "ses_".len() + STAMP_CHARS + RANDOM_CHARS);
    }

    #[test]
    fn ids_are_strictly_increasing_and_unique() {
        let ids: Vec<EntryId> = (0..10_000).map(|_| EntryId::generate()).collect();
        let unique: HashSet<&str> = ids.iter().map(EntryId::as_str).collect();
        assert_eq!(unique.len(), ids.len(), "同一进程内绝不允许撞 id");
        for pair in ids.windows(2) {
            let [earlier, later] = pair else { continue };
            assert!(
                earlier < later,
                "字典序必须等于生成序：{earlier} 应当排在 {later} 之前"
            );
        }
    }

    #[test]
    fn same_millisecond_burst_borrows_from_the_next_millisecond() {
        // 一毫秒内超过 4096 个（counter 位宽）也必须保持单调——上游正是在这里溢出污染时间位。
        let ids: Vec<EntryId> = (0..5_000).map(|_| EntryId::generate()).collect();
        for pair in ids.windows(2) {
            let [earlier, later] = pair else { continue };
            assert!(earlier < later);
        }
    }

    #[test]
    fn base32_is_high_bit_first() {
        let mut encoded = String::new();
        push_base32(&mut encoded, 1, 2);
        assert_eq!(encoded, "01");
        encoded.clear();
        push_base32(&mut encoded, 31, 2);
        assert_eq!(encoded, "0Z");
        encoded.clear();
        push_base32(&mut encoded, 32, 2);
        assert_eq!(encoded, "10");
    }

    #[test]
    fn serde_round_trip_is_a_bare_string() {
        let id = SessionId::generate();
        let json = serde_json::to_string(&id).expect("序列化不应失败");
        assert_eq!(json, format!("\"{id}\""));
        let back: SessionId = serde_json::from_str(&json).expect("反序列化不应失败");
        assert_eq!(back, id);
    }
}
