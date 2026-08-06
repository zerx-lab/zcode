//! `zcode-catalog`：模型目录：内置 `models.json`、提供商描述符、模型身份识别与分类。
//!
//! | 模块 | 职责 |
//! | --- | --- |
//! | [`spec`] | `models.json` 的磁盘形态，生成器与运行时共用的契约 |
//! | [`models`] | 内置目录的惰性加载与查询、成本计算 |
//! | [`descriptors`] | 提供商描述符表：base URL、环境变量、默认模型、发现能力 |
//! | [`effort`] | 推理努力档位与规范序 |
//! | [`thinking`] | 思考配置：控制模式、effort ladder、线上字段推导 |
//! | [`identity`] | 模型 id 的解析、族判定与归一化 |
//! | [`cache`] | 运行时模型发现结果的 `SQLite` 落盘缓存 |
//! | [`manager`] | 静态目录与运行时发现结果的仲裁与新鲜度策略 |
//!
//! `models.json` 是生成物，绝不手工编辑；要改内容改
//! `src/bin/gen_models.rs`（见 `rule://zcode-architecture`）。

pub mod cache;
pub mod descriptors;
pub mod effort;
pub mod identity;
pub mod manager;
pub mod models;
pub mod spec;
pub mod thinking;

pub use crate::cache::*;
pub use crate::descriptors::*;
pub use crate::effort::*;
pub use crate::identity::*;
pub use crate::manager::*;
pub use crate::models::*;
pub use crate::spec::*;
pub use crate::thinking::*;
