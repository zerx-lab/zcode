//! 内置工具的装配根：声明全部工具子模块，把它们组装成一个 [`ToolRegistry`]。
//!
//! 每个工具自己的实现、参数 schema、prompt 描述都住在各自的子模块（`read.rs` / `bash.rs`
//! / ...），本文件只做三件事：
//!
//! 1. 声明子模块，让它们的类型在 `crate::tools::<name>` 下可见；
//! 2. 在 [`registry`] 里把 `Config` 与 `Workspace` 接成每个工具的构造参数，按
//!    `config.tools.disabled` 跳过用户不想要的工具；
//! 3. 通过 [`output`] 子模块统一工具产出的收尾清理——工具实现只管拼正文，收尾交给它。
//!
//! # 为什么装配放在这一个文件里，不是每个工具自己往全局注册表里塞
//!
//! `zcode_agent::tool::registry::ToolRegistry` 编译期就要求每个工具名唯一（`register`
//! 校验 schema 并检测重名），把"到底有哪八个工具、按什么顺序、谁被谁禁用"这个决策集中在
//! 一处，比让八个模块各自在 `ctor`/`inventory` 式的全局注册表里悄悄插入一条更容易审计——
//! 新增或临时禁用一个工具，改这一个文件就够了。

pub(crate) mod bash;
pub(crate) mod edit;
pub(crate) mod glob;
pub(crate) mod grep;
pub(crate) mod ls;
pub(crate) mod output;
pub(crate) mod read;
pub(crate) mod todo;
pub(crate) mod write;

use std::collections::HashSet;
use std::sync::Arc;

use zcode_agent::Tool;
use zcode_agent::tool::registry::ToolRegistry;
use zcode_schema::SchemaError;

use crate::config::Config;
use crate::workspace::Workspace;

/// 全部内置工具名，是 `config.tools.disabled` 合法取值的唯一来源。
///
/// 顺序无所谓——`ToolRegistry::definitions()` 会在下发给模型前按名重新排序
/// （prompt cache 前提，见 `crates/agent/src/tool/registry.rs` 模块文档）,这里的顺序
/// 只影响注册期的报错先后。
const KNOWN_TOOL_NAMES: [&str; 8] = [
    "read", "ls", "write", "edit", "bash", "glob", "grep", "todo",
];

/// 工具装配失败的错误。
#[derive(Debug, thiserror::Error)]
pub(crate) enum ToolInitError {
    /// 某个工具的参数 schema 编译失败——通常是手写 JSON Schema 时字段或 `$ref` 写错了。
    #[error("工具 `{tool}` 的参数 schema 编译失败：{source}")]
    Schema {
        /// 出问题的工具名。
        tool: &'static str,
        /// 底层 schema 编译错误。
        #[source]
        source: SchemaError,
    },
    /// 同一个工具名被注册了两次。八个工具名互不相同是装配逻辑本身的不变量，出现说明
    /// 这份代码有 bug（比如复制粘贴漏改了名字），不是用户配置能触发的情况。
    #[error("工具名 `{name}` 被重复注册")]
    Duplicate {
        /// 冲突的工具名。
        name: &'static str,
    },
}

/// 装配八个内置工具，返回可交给 [`zcode_agent::turn::AgentRuntime`] 的注册表。
///
/// `config.tools.disabled` 列出的工具名会被跳过、不注册；其中出现的未知名字只
/// `tracing::warn!` 一声，不当错误处理——用户手滑打错一个名字不该让整个进程起不来。
///
/// # Errors
/// 任一工具的参数 schema 编译失败，或出现重复工具名时返回 [`ToolInitError`]。
pub(crate) fn registry(
    config: &Config,
    workspace: &Arc<Workspace>,
) -> Result<ToolRegistry, ToolInitError> {
    let disabled: HashSet<&str> = config.tools.disabled.iter().map(String::as_str).collect();
    for name in &config.tools.disabled {
        if !KNOWN_TOOL_NAMES.contains(&name.as_str()) {
            tracing::warn!(tool = %name, "config.tools.disabled 引用了未知工具名，已忽略");
        }
    }

    let mut registry = ToolRegistry::new();
    register_tool(&mut registry, &disabled, "read", || {
        Arc::new(read::ReadTool::new(Arc::clone(workspace), &config.tools))
    })?;
    register_tool(&mut registry, &disabled, "ls", || {
        Arc::new(ls::LsTool::new(Arc::clone(workspace), &config.tools))
    })?;
    register_tool(&mut registry, &disabled, "write", || {
        Arc::new(write::WriteTool::new(Arc::clone(workspace), &config.tools))
    })?;
    register_tool(&mut registry, &disabled, "edit", || {
        Arc::new(edit::EditTool::new(Arc::clone(workspace), &config.tools))
    })?;
    register_tool(&mut registry, &disabled, "bash", || {
        Arc::new(bash::BashTool::new(Arc::clone(workspace), &config.tools))
    })?;
    register_tool(&mut registry, &disabled, "glob", || {
        Arc::new(glob::GlobTool::new(Arc::clone(workspace), &config.tools))
    })?;
    register_tool(&mut registry, &disabled, "grep", || {
        Arc::new(grep::GrepTool::new(Arc::clone(workspace), &config.tools))
    })?;
    register_tool(&mut registry, &disabled, "todo", || {
        Arc::new(todo::TodoTool::new(config.session.dir.clone()))
    })?;

    Ok(registry)
}

/// 单个工具的"是否禁用 → 构造 → 注册"三段式，八个工具共用同一段逻辑，避免
/// `registry()` 里把跳过判断、schema 报错映射各写八遍。
///
/// `build` 只在工具未被禁用时才会调用——构造函数可能不是零开销的（比如要读 `ToolsConfig`
/// 里的路径字段），没必要为一个用户明确关掉的工具白白构造一次。
fn register_tool(
    registry: &mut ToolRegistry,
    disabled: &HashSet<&str>,
    name: &'static str,
    build: impl FnOnce() -> Arc<dyn Tool>,
) -> Result<(), ToolInitError> {
    if disabled.contains(name) {
        tracing::debug!(tool = name, "工具已在配置中禁用，跳过注册");
        return Ok(());
    }
    match registry.register(build()) {
        Ok(None) => Ok(()),
        Ok(Some(_previous)) => Err(ToolInitError::Duplicate { name }),
        Err(source) => Err(ToolInitError::Schema { tool: name, source }),
    }
}
