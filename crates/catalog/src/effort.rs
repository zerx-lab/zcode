//! 推理努力档位（`Effort`）：跨所有提供商的用户可见思考强度，低→高。
//!
//! `Ord` 派生自声明顺序，即是规范序，不另写比较函数——`Off < Minimal < Low <
//! Medium < High < XHigh < Max`。这是全仓 effort 的**规范类型**（见
//! `rule://zcode-architecture` 的 catalog 导入边界）：`crates/ai/src/types.rs`
//! 的 `ReasoningEffort { None, Minimal, Low, Medium, High, XHigh, Max }` 是同一
//! 概念的并行定义，之后会删掉改成对本类型的 re-export，因此这里的取值集合
//! 必须覆盖它的全部七档、且线上字符串完全一致——包括 `Off` 在线上写作
//! `"none"`（对齐 OpenAI Responses `reasoning.effort` 的取值），而不是
//! `"off"`。`Off` 额外在阶梯里占一个可比较、可排序的位置：Anthropic
//! `Budget` 模式的 ladder 是 `[Off, Low, Medium, High]`，`Off` 就是这条阶梯上
//! 最低的一档，而不是阶梯之外的特殊值。

use std::fmt;

use serde::{Deserialize, Serialize};

/// 推理努力档位，低→高。
///
/// `#[repr(u8)]` + 声明顺序即数值序，[`Effort::index`] 直接给出定长数组下标，
/// 不需要额外的映射表。
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    /// 关闭思考（对支持该档位的模型而言，这是阶梯上最低的一档）。线上取值
    /// 是 `"none"`（对齐 OpenAI Responses `reasoning.effort`），不是
    /// `"off"`——`"off"` 仅作为 [`Effort::parse`] 额外接受的别名拼写。
    #[serde(rename = "none")]
    Off,
    /// 最低非零档。
    Minimal,
    /// 低。
    Low,
    /// 中。
    Medium,
    /// 高。
    High,
    /// 高于 `High` 的加强档。
    XHigh,
    /// 最高档。
    Max,
}

impl Effort {
    /// 全部档位，按规范序排列；同时是 [`Effort::parse`] 的候选集合。
    pub const ALL: [Effort; 7] = [
        Self::Off,
        Self::Minimal,
        Self::Low,
        Self::Medium,
        Self::High,
        Self::XHigh,
        Self::Max,
    ];

    /// 线上/展示用小写字符串：`"none"` | `"minimal"` | `"low"` | `"medium"` |
    /// `"high"` | `"xhigh"` | `"max"`。`Off` 写作 `"none"`，与
    /// `crates/ai/src/types.rs::ReasoningEffort::as_str` 的线上取值一致。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "none",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Max => "max",
        }
    }

    /// 从字符串解析，大小写不敏感（`eq_ignore_ascii_case`，不分配）。
    ///
    /// `Off` 接受两种拼写：线上标准值 `"none"`，以及更符合直觉的别名
    /// `"off"`（历史遗留/用户输入常见拼法）；两者都映射到 [`Effort::Off`]。
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        if s.eq_ignore_ascii_case("off") {
            return Some(Self::Off);
        }
        Self::ALL
            .into_iter()
            .find(|effort| s.eq_ignore_ascii_case(effort.as_str()))
    }

    /// 定长数组下标，`0..7`，与声明顺序（即规范序）一致。
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Off => 0,
            Self::Minimal => 1,
            Self::Low => 2,
            Self::Medium => 3,
            Self::High => 4,
            Self::XHigh => 5,
            Self::Max => 6,
        }
    }
}

impl fmt::Display for Effort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ord_matches_declared_ladder() {
        assert!(Effort::Off < Effort::Minimal);
        assert!(Effort::Minimal < Effort::Low);
        assert!(Effort::Low < Effort::Medium);
        assert!(Effort::Medium < Effort::High);
        assert!(Effort::High < Effort::XHigh);
        assert!(Effort::XHigh < Effort::Max);
        let mut sorted = Effort::ALL;
        sorted.sort_unstable();
        assert_eq!(sorted, Effort::ALL, "ALL 本身已按规范序排列");
    }

    #[test]
    fn off_serializes_as_none_on_the_wire() {
        assert_eq!(Effort::Off.as_str(), "none");
    }

    #[test]
    fn parse_accepts_both_off_and_none_spellings() {
        assert_eq!(Effort::parse("off"), Some(Effort::Off));
        assert_eq!(Effort::parse("OFF"), Some(Effort::Off));
        assert_eq!(Effort::parse("none"), Some(Effort::Off));
        assert_eq!(Effort::parse("NONE"), Some(Effort::Off));
        assert_eq!(Effort::parse("off"), Effort::parse("none"));
    }

    #[test]
    fn parse_is_case_insensitive() {
        assert_eq!(Effort::parse("high"), Some(Effort::High));
        assert_eq!(Effort::parse("HIGH"), Some(Effort::High));
        assert_eq!(Effort::parse("High"), Some(Effort::High));
        assert_eq!(Effort::parse("xhigh"), Some(Effort::XHigh));
        assert_eq!(Effort::parse("XHIGH"), Some(Effort::XHigh));
    }

    #[test]
    fn parse_rejects_unknown_strings() {
        assert_eq!(Effort::parse("extreme"), None);
        assert_eq!(Effort::parse(""), None);
    }

    #[test]
    fn as_str_round_trips_through_parse() {
        for effort in Effort::ALL {
            assert_eq!(Effort::parse(effort.as_str()), Some(effort));
        }
    }

    #[test]
    fn index_is_dense_and_unique() {
        let mut indices: Vec<usize> = Effort::ALL.iter().map(|e| e.index()).collect();
        indices.sort_unstable();
        assert_eq!(indices, vec![0, 1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn serializes_to_lowercase_json_string() {
        let json = serde_json::to_string(&Effort::Medium).unwrap();
        assert_eq!(json, "\"medium\"");
        let back: Effort = serde_json::from_str(&json).unwrap();
        assert_eq!(back, Effort::Medium);
    }

    #[test]
    fn off_serializes_to_none_json_string() {
        let json = serde_json::to_string(&Effort::Off).unwrap();
        assert_eq!(json, "\"none\"");
        let back: Effort = serde_json::from_str(&json).unwrap();
        assert_eq!(back, Effort::Off);
    }
}
