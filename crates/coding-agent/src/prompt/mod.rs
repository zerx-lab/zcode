//! system prompt 装配 + `AGENTS.md` 发现 + 首条消息的环境上下文。
//!
//! # 分段
//!
//! [`PromptSet::system`] 按顺序拼接进 [`zcode_agent::TurnConfig::system`]：
//!
//! 1. `system.md`——静态主模板，`include_str!` 编译期内联。这是 prompt cache 的
//!    前缀，**必须逐字节稳定**：不含日期、cwd、模型名或任何随运行环境变化的
//!    内容，跨会话、跨天完全相同。
//! 2. `AGENTS.md` 合并结果（[`agents_md::discover`]）——存在才追加这一段，
//!    见该模块的文档了解发现规则、合并顺序与字节上限的推导。
//!
//! [`PromptSet::session_context`] **不**进 `system`，而是首条 user 消息的
//! `<system-reminder>` 包裹块内容（包裹标签由 `HostCore` 加，见该字段的文档）。
//! 生成逻辑在 [`context`] 模块，包含本 crate 唯一的 git 子进程调用点。

mod agents_md;
mod context;

use crate::config::Config;
use crate::model::ResolvedModel;
use crate::workspace::Workspace;

/// 静态主系统模板：prompt cache 前缀，逐字节稳定，见模块文档。
const SYSTEM_TEMPLATE: &str = include_str!("system.md");

/// 一次会话装配好的 prompt 材料。
#[derive(Debug)]
pub(crate) struct PromptSet {
    /// 进 [`zcode_agent::TurnConfig::system`]，按顺序拼接。第 0 段是静态主模板
    /// （缓存前缀）。
    pub(crate) system: Vec<String>,
    /// 首条 user 消息的 `<system-reminder>` 环境上下文；**不进** `system`。
    ///
    /// 理由：日期/git 状态每次都变，放 `system` 会天天打穿 prompt cache
    /// （jcode `crates/jcode-base/src/session.rs:897-921`）。写进会话、决定是否
    /// 只在新会话首条消息生效，是 `HostCore` 的事——本模块只生成文本本身。
    pub(crate) session_context: String,
}

/// `prompt` 模块的错误类型。
///
/// 当前没有可恢复的失败路径：`AGENTS.md` 读取失败、git 调用失败都按设计静默
/// 降级（分别见 [`agents_md`] 与 [`context`] 的模块文档），不会走到这里。保留
/// 这个空枚举是为了不改 [`build`] 的返回类型签名——后续如果引入需要硬失败的
/// 失败模式（例如配置里显式指定了一个必须存在的自定义模板文件），在这里加变体
/// 即可，不要为了凑一个"当前用不上"的变体而编造一个假失败场景。
#[derive(Debug, thiserror::Error)]
pub(crate) enum PromptError {}

/// 装配一次会话用的 prompt 材料。
///
/// `config` 目前未被读取：`system[0]` 必须逐字节稳定（它是 prompt cache 前缀），
/// 因此内容不能随 `config.tools.disabled` 之类的用户配置变化；保留这个参数是
/// 为了让签名稳定，未来若加入"自定义系统 prompt 覆盖文件"这类确需读配置的能力，
/// 不用再改调用点。
pub(crate) async fn build(
    workspace: &Workspace,
    _config: &Config,
    model: &ResolvedModel,
) -> Result<PromptSet, PromptError> {
    let mut system = vec![SYSTEM_TEMPLATE.to_owned()];

    let project_context = agents_md::discover(
        workspace.root(),
        zcode_text::home_dir().as_deref(),
        |path| workspace.display(path),
    )
    .await;
    if let Some(project_context) = project_context {
        system.push(project_context);
    }

    let cwd = workspace.root();
    let cwd_display = workspace.display(cwd);
    let session_context = context::build(cwd, &cwd_display, &model.id).await;

    Ok(PromptSet {
        system,
        session_context,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use zcode_agent::{ApprovalMode, Policy};
    use zcode_ai::{ProviderId, Thinking};

    use super::*;
    use crate::config::{
        ApprovalConfig, DaemonConfig, ModelConfig, SessionConfig, ToolsConfig, UiConfig,
    };

    fn test_config(dir: &std::path::Path) -> Config {
        Config {
            model: ModelConfig {
                id: None,
                thinking: None,
                provider: None,
            },
            approval: ApprovalConfig {
                mode: ApprovalMode::default(),
                policies: HashMap::<String, Policy>::new(),
            },
            tools: ToolsConfig {
                disabled: Vec::new(),
                bash_timeout_secs: 120,
                read_max_lines: 2000,
            },
            session: SessionConfig {
                dir: dir.join("sessions"),
            },
            daemon: DaemonConfig {
                enabled: false,
                runtime_dir: dir.join("daemon"),
            },
            ui: UiConfig {
                show_thinking: false,
            },
        }
    }

    fn test_model() -> ResolvedModel {
        ResolvedModel {
            id: "test-model".to_owned(),
            provider: ProviderId::Anthropic,
            context_window: 200_000,
            thinking: Thinking::default(),
        }
    }

    /// 抄 jcode `crates/jcode-base/tests/prompt_tests.rs:110-113` 的断言思路：
    /// 环境上下文只能出现在首条消息里，绝不能混进 `system` 的任何一段——
    /// 混进去就意味着日期/git 状态会天天打穿 prompt cache 前缀。
    #[tokio::test]
    async fn session_context_never_leaks_into_system_segments() {
        let dir = tempfile::tempdir().expect("创建临时目录");
        std::fs::write(dir.path().join(".git"), "").expect("写入 .git 标记");
        let workspace = Workspace::new(dir.path().to_path_buf());
        let config = test_config(dir.path());
        let model = test_model();

        let prompt = build(&workspace, &config, &model)
            .await
            .expect("build 当前没有可恢复失败路径");

        for segment in &prompt.system {
            assert!(
                !segment.contains(&prompt.session_context),
                "session_context 绝不能出现在 system 的任一段里"
            );
        }
    }

    #[tokio::test]
    async fn system_first_segment_is_the_static_template_verbatim() {
        let dir = tempfile::tempdir().expect("创建临时目录");
        std::fs::write(dir.path().join(".git"), "").expect("写入 .git 标记");
        let workspace = Workspace::new(dir.path().to_path_buf());
        let config = test_config(dir.path());
        let model = test_model();

        let prompt = build(&workspace, &config, &model)
            .await
            .expect("build 当前没有可恢复失败路径");

        assert_eq!(prompt.system.first(), Some(&SYSTEM_TEMPLATE.to_owned()));
    }

    #[tokio::test]
    async fn agents_md_present_appends_a_second_system_segment() {
        let dir = tempfile::tempdir().expect("创建临时目录");
        std::fs::write(dir.path().join(".git"), "").expect("写入 .git 标记");
        std::fs::write(dir.path().join("AGENTS.md"), "Follow the house style.")
            .expect("写入 AGENTS.md");
        let workspace = Workspace::new(dir.path().to_path_buf());
        let config = test_config(dir.path());
        let model = test_model();

        let prompt = build(&workspace, &config, &model)
            .await
            .expect("build 当前没有可恢复失败路径");

        assert_eq!(prompt.system.len(), 2);
        assert!(prompt.system[1].contains("Follow the house style."));
    }
}
