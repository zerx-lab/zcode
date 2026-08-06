//! `zcode-utils`：跨 crate 共享的基础设施。
//!
//! 职责边界见 `rule://zcode-architecture` 的 crate 职责表。写新 helper 前先在这里搜一遍：
//! 重复实现同一功能是缺陷，即使两份都能跑。

pub mod daemon;
pub mod env;
pub mod transport;
