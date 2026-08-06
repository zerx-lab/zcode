//! 模型 id 的解析、族判定与归一化。
//!
//! 移植自 oh-my-pi `packages/catalog/src/identity/`，但**不移植它的无界 memo**：
//! 上游 `classify.ts` 连解析失败的 `null` 也缓存，`family.ts` 给约 25 个判定谓词
//! 各配一个 `Map` 当缓存，理由是"调用方只会喂有界集合"里的 id。
//!
//! 这个前提在 ZCode 里不成立：代理发现路径（见 [`crate::manager`]）会把远端
//! `/v1/models` 返回的**任意**字符串喂给这里的函数，集合大小取决于对端会返回
//! 什么，不受我们控制。上游是 GC 语言，缓存不清也只是多占点堆；这里没有 GC，
//! 一个不清空的全局 `Map`/`HashMap` 就是确定性内存泄漏。
//!
//! 因此本模块的每个函数都是**无缓存纯函数**：同样的输入总是同样的输出，
//! 能借用输入就借用（`&str` 而非 `String`），不维护任何跨调用状态。
//! 调用频率不高（模型选择、UI 展示），重新计算的成本远低于泄漏的成本。

pub mod classify;
pub mod family;

pub use classify::*;
pub use family::*;
