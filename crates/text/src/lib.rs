//! `zcode-text`：性能关键路径：文本处理、图像编解码、grep。
//!
//! 四个互不依赖的模块，按调用方需要各取所需：
//!
//! | 模块 | 职责 |
//! | --- | --- |
//! | [`width`] | ANSI 感知的显示宽度、截断、换行、制表符与控制字符清洗 |
//! | [`truncate`] | 工具输出的统一截断（行 / 字节 / 列三个维度）与内联字节封顶 |
//! | [`path`] | 面向展示的路径缩短（主目录 → `~`） |
//! | [mod@grep] | 进程内 ripgrep 引擎：遍历、匹配、分页、取消 |
//! | [`image`] | 出站图像的解码 → 缩放 → 重编码流水线 |
//!
//! 宽度一律经 `unicode-width` 计算，绝不用 `str::len()`——这是 `rule://zcode-architecture`
//! 「TUI 输出清理」一节的硬要求，本 crate 是它唯一的实现落点。

pub mod grep;
pub mod image;
pub mod path;
pub mod truncate;
pub mod width;

pub use crate::grep::*;
pub use crate::image::*;
pub use crate::path::*;
pub use crate::truncate::*;
pub use crate::width::*;
