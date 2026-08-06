//! `zcode-schema`：JSON Schema 校验：惰性编译、按 schema 缓存的运行时。
//!
//! 职责边界见 `rule://zcode-architecture` 的 crate 职责表：这里只做 draft 2020-12 子集的
//! 结构校验本身——不做提供商侧 schema 清洗（那是 `zcode-ai` 的 provider 适配器）、
//! 不做工具参数 coercion（属于 agent 层）、不做流式 partial JSON 解析。
//!
//! 校验器是自研的单趟递归实现，不引入 `jsonschema` crate——理由与取舍见
//! [`compile`] 模块文档；这是经过 `rule://reference-first` 三仓调研后的既定选型。
//!
//! # 模块
//!
//! | 模块 | 职责 |
//! | --- | --- |
//! | [`error`] | 编译期 [`error::SchemaError`] 与运行期 [`error::ValidationError`]/[`error::ValidationIssue`]。 |
//! | [`compile`] | [`compile::CompiledSchema`] 惰性编译 + [`compile::SchemaCache`] 按内容哈希缓存。 |
//! | [`render`] | 把 [`error::ValidationError`] 渲染成回灌给模型的错误文本。 |
//!
//! # 示例
//!
//! ```no_run
//! use serde_json::json;
//! use zcode_schema::{render_validation_error, CompiledSchema};
//!
//! let schema = json!({
//!     "type": "object",
//!     "properties": { "path": { "type": "string", "minLength": 1 } },
//!     "required": ["path"],
//! });
//! let compiled = CompiledSchema::compile(schema)?;
//!
//! let instance = json!({ "path": "" });
//! if let Err(error) = compiled.validate(&instance) {
//!     let text = render_validation_error("read_file", &error, &instance);
//!     println!("{text}");
//! }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

pub mod compile;
pub mod error;
pub mod render;
mod validate;

pub use crate::compile::*;
pub use crate::error::*;
pub use crate::render::*;
