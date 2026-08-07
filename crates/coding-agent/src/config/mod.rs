//! ZCode 配置：两层 TOML + 环境变量的发现、合并与默认值。
//!
//! # 优先级链（低到高）
//!
//! ```text
//! 内置默认 <- 全局 ~/.zcode/config.toml <- 项目 <root>/.zcode/config.toml <- ZCODE_* 环境变量
//! ```
//!
//! `~` 是 [`state_dir`]（`ZCODE_HOME` 可覆盖），`<root>` 由 [`paths::find_project_root`] 从
//! `cwd` 向上找 `.git`/`.zcode/` 定位。**CLI flag 不在本模块处理**——调用方拿到 [`Config`]
//! 后自己 apply，flag 的优先级最高但那是 `cli` 层的职责。
//!
//! # 合并语义（两种，别混）
//!
//! 每一层先解析成全 `Option` 的 [`RawConfig`]，逐层 [`RawConfig::merge`]，最后
//! [`RawConfig::finish`] 一次性填内置默认值变成 [`Config`]。默认是**逐字段覆盖**：
//! 高优先级层某字段是 `None`（TOML 里没写）就完全不影响低优先级层已经填的值；
//! 只有两个字段偏离这条默认规则：
//!
//! - [`ApprovalConfig::policies`]（`HashMap<String, Policy>`）：**entry 级合并**——高优先级层
//!   只覆盖它写到的那些工具名，没提到的工具名沿用低优先级层的策略。见
//!   `policies_merge_by_entry` 测试。
//! - [`ToolsConfig::disabled`]（`Vec<String>`）：**整体替换**——只要高优先级层写了
//!   `tools.disabled`（哪怕是空数组），就完全取代低优先级层的列表，不做并集。
//!   见 `tools_disabled_whole_replace` 测试。
//!
//! # 容错
//!
//! 配置文件不存在 = 正常（当作这一层没有覆盖）；文件存在但解析失败 = 硬错误，
//! 带上文件路径和 `toml` crate 自带的行列号（`toml::de::Error` 的 `Display` 已经是
//! `"TOML parse error at line L, column C\n..."` 格式，见
//! `toml-1.1.4/src/de/error.rs:104-115`，本模块不重新解析、只透传）。**绝不静默回退到
//! 默认值**——用户配置写错了却看到一切正常，是最坏的用户体验。未知字段同理：
//! 所有 `Raw*` 结构都 `#[serde(deny_unknown_fields)]`，拼错一个键当场报错，而不是
//! 静默丢弃后该设置的行为就是默认值。

mod paths;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use zcode_agent::{ApprovalMode, Policy};

/// bash 工具超时的默认值（秒）。
///
/// 出处：oh-my-pi `packages/coding-agent/src/tools/tool-timeouts.ts:11`，
/// `TOOL_TIMEOUTS.bash = { default: 300, min: 1, max: 3600 }`。
const DEFAULT_BASH_TIMEOUT_SECS: u64 = 300;

/// read 工具默认返回的行数上限。
///
/// 出处：oh-my-pi `packages/coding-agent/src/config/settings-schema.ts:3258-3260`，
/// `"read.defaultLimit"` 当前线上默认值 `300`。这个数字不是拍脑袋定的：它是用
/// `scripts/session-stats/read_optimizer.py` 对真实会话的 read 调用重放数据做网格搜索
/// 选出来的——目标函数是 `tokens + 250 * calls + 100_000 * max(0, 新增截断次数)`
/// （`read_optimizer.py:548-552`：每多一次后续调用记 250 token 等价代价，每多一次超过
/// 当前基线的截断记 `100_000`，用来强惩罚"改小了默认值导致模型读不全被迫二次读"），
/// 在候选网格 `--defaults 100,150,200,250,300,400,500,700,1000`
/// （`read_optimizer.py:701`）上取最小值。脚本里另有 `CURRENT_DEFAULT = 500`
/// （`read_optimizer.py:46`）——那是网格搜索开跑前记录的历史基线，早于当前已生效的
/// 300，不要跟这里的默认值混淆。本仓字段语义是"未显式给 limit 时 read 工具返回的行数
/// 上限"，与上游 `read.defaultLimit` 的场景一致，直接照抄这个已验证过的数字。
const DEFAULT_READ_MAX_LINES: usize = 300;

/// 合并、填完默认值之后的最终配置。
#[derive(Debug, Clone)]
pub(crate) struct Config {
    /// 模型选择。
    pub(crate) model: ModelConfig,
    /// 工具审批策略。
    pub(crate) approval: ApprovalConfig,
    /// 内置工具的开关与参数。
    pub(crate) tools: ToolsConfig,
    /// 会话存储位置。
    pub(crate) session: SessionConfig,
    /// daemon 生命周期与运行时目录。
    pub(crate) daemon: DaemonConfig,
    /// 终端 UI 展示偏好。
    pub(crate) ui: UiConfig,
}

/// 模型选择。三个字段都可能来自目录/环境解析出的默认值，`None` 交给 `model::resolve_model`
/// 决定回退逻辑，本模块不猜测默认模型是谁。
#[derive(Debug, Clone)]
pub(crate) struct ModelConfig {
    /// 模型 ID；`None` 时由目录给出的默认模型决定。
    pub(crate) id: Option<String>,
    /// 思考强度覆盖；`None` 时使用模型自身的默认思考策略。
    pub(crate) thinking: Option<String>,
    /// 强制指定 provider，跳过按模型 ID 反查；`None` 时按目录正常解析。
    pub(crate) provider: Option<String>,
}

/// 工具审批策略：模式 + 逐工具覆盖。
#[derive(Debug, Clone)]
pub(crate) struct ApprovalConfig {
    /// 审批模式；默认 [`ApprovalMode::Yolo`]（`crates/agent/src/approval.rs:19-24`
    /// 记录了这是产品取向的显式选择，不是遗漏）。
    pub(crate) mode: ApprovalMode,
    /// 工具名 -> 策略的用户覆盖，喂给 `zcode_agent::resolve_approval` 的 `user_policies`。
    pub(crate) policies: HashMap<String, Policy>,
}

/// 内置工具的开关与参数。
#[derive(Debug, Clone)]
pub(crate) struct ToolsConfig {
    /// 禁用的工具名列表（`tools::registry` 按名字跳过注册）。
    pub(crate) disabled: Vec<String>,
    /// bash 工具超时（秒）。
    pub(crate) bash_timeout_secs: u64,
    /// read 工具默认返回的行数上限。
    pub(crate) read_max_lines: usize,
}

/// 会话存储位置。
#[derive(Debug, Clone)]
pub(crate) struct SessionConfig {
    /// 会话 JSONL 树的落盘目录。
    pub(crate) dir: PathBuf,
}

/// daemon 生命周期与运行时目录。
#[derive(Debug, Clone)]
pub(crate) struct DaemonConfig {
    /// 是否允许连接/启动常驻 daemon；`false` 时一律走 `stream_pair()` 自托管。
    pub(crate) enabled: bool,
    /// daemon 的 socket / 注册文件 / 锁文件所在目录。
    pub(crate) runtime_dir: PathBuf,
}

/// 终端 UI 展示偏好。
#[derive(Debug, Clone)]
pub(crate) struct UiConfig {
    /// 是否默认展示模型的思考过程。
    pub(crate) show_thinking: bool,
}

/// 两层 TOML 合并：全局 `~/.zcode/config.toml` <- 项目 `<root>/.zcode/config.toml`
/// <- `ZCODE_*` 环境变量 <- CLI flag（flag 由调用方在返回后 apply）。
///
/// `ZCODE_CONFIG` 设置时跳过两层发现，只读它指向的那一份文件。
pub(crate) fn load(cwd: &Path) -> Result<Config, ConfigError> {
    load_with_env(cwd, &ProcessEnv)
}

/// 配置发现路径（供 `zcode config path` 与错误消息用）。
#[must_use]
pub(crate) fn discover(cwd: &Path) -> ConfigPaths {
    let home = zcode_text::home_dir();
    let zcode_home = std::env::var("ZCODE_HOME").ok();
    // 展示用途：找不到主目录时退化为 cwd 下的 `.zcode`，不影响 load() 的硬错误路径
    // （load() 走 state_dir_from 会在同样场景下如实返回 ConfigError::NoHomeDir）。
    let state_dir = state_dir_from(home.clone(), zcode_home).unwrap_or_else(|_| cwd.join(".zcode"));
    let project_root = paths::find_project_root(cwd, home.as_deref());
    let project = project_root.as_deref().map(project_config_path);
    ConfigPaths {
        global: global_config_path(&state_dir),
        project,
        project_root,
    }
}

/// [`discover`] 的结果：三条路径供展示/错误消息使用。
#[derive(Debug, Clone)]
pub(crate) struct ConfigPaths {
    /// 全局配置文件路径（不保证存在）。
    pub(crate) global: PathBuf,
    /// 项目配置文件路径；`project_root` 为 `None` 时同为 `None`。
    pub(crate) project: Option<PathBuf>,
    /// 发现到的项目根；`None` 表示 `cwd` 及其祖先都没有 `.git`/`.zcode/` 标记。
    pub(crate) project_root: Option<PathBuf>,
}

/// 全局状态目录 `~/.zcode`（与 `crates/ai/src/auth/store.rs` 的凭据目录同位：
/// 二者都是 `ZCODE_HOME` 覆盖优先，否则拼主目录；`auth/store.rs:122-135` 用
/// `std::env::home_dir()`，这里用 `zcode_text::home_dir()`，实现不同但语义一致，
/// 都不引入 `dirs` crate）。
pub(crate) fn state_dir() -> Result<PathBuf, ConfigError> {
    state_dir_from(zcode_text::home_dir(), std::env::var("ZCODE_HOME").ok())
}

/// [`state_dir`] 的纯函数核心：主目录与 `ZCODE_HOME` 覆盖都由调用方传入，供测试注入。
fn state_dir_from(
    home: Option<PathBuf>,
    zcode_home_override: Option<String>,
) -> Result<PathBuf, ConfigError> {
    if let Some(over) = zcode_home_override.filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(over));
    }
    home.map(|dir| dir.join(".zcode"))
        .ok_or(ConfigError::NoHomeDir)
}

/// 全局配置文件路径：状态目录下的 `config.toml`。
fn global_config_path(state_dir: &Path) -> PathBuf {
    state_dir.join("config.toml")
}

/// 项目配置文件路径：项目根下的 `.zcode/config.toml`。
fn project_config_path(project_root: &Path) -> PathBuf {
    project_root.join(".zcode").join("config.toml")
}

/// [`load`] 的核心实现：环境变量读取通过 [`EnvSource`] 注入，测试不必碰真实进程环境。
fn load_with_env(cwd: &Path, env: &impl EnvSource) -> Result<Config, ConfigError> {
    let home = zcode_text::home_dir();
    let state_dir = state_dir_from(home.clone(), env.get("ZCODE_HOME"))?;

    let mut merged = RawConfig::default();
    if let Some(direct) = non_empty(env.get("ZCODE_CONFIG")) {
        // 直指一份配置文件：跳过全局/项目两层发现，其余层级（内置默认、环境变量）不变。
        merged = merged.merge(load_layer(Path::new(&direct))?);
    } else {
        merged = merged.merge(load_layer(&global_config_path(&state_dir))?);
        if let Some(root) = paths::find_project_root(cwd, home.as_deref()) {
            merged = merged.merge(load_layer(&project_config_path(&root))?);
        }
    }
    merged = merged.merge(env_overrides(env)?);

    Ok(merged.finish(&state_dir))
}

/// 读取单层配置文件。文件不存在按“这一层没有覆盖”处理（[`RawConfig::default`]）；
/// 存在但读不出/解析不出则是硬错误，带上路径与 `toml` 自带的行列号。
fn load_layer(path: &Path) -> Result<RawConfig, ConfigError> {
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(RawConfig::default()),
        Err(source) => {
            return Err(ConfigError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    toml::from_str(&contents).map_err(|source| ConfigError::Parse {
        path: path.to_path_buf(),
        source: Box::new(source),
    })
}

/// 从环境变量提取覆盖，对应优先级链的最高一层。
///
/// 只暴露这六个变量，不做逐字段全覆盖（jcode `crates/jcode-base/src/config/env_overrides.rs`
/// 773 行手写覆盖是它自认的技术债，见 `rule://reference-first` 已探明线索）：
///
/// - `ZCODE_HOME`：`state_dir` 覆盖入口。容器/CI/多身份并存的场景要在不碰真实
///   `$HOME`/`%USERPROFILE%` 的前提下隔离配置与会话数据。
/// - `ZCODE_MODEL`：切模型是最高频的临时改动（换个模型跑一次），不值得为它去改配置文件。
/// - `ZCODE_APPROVAL_MODE`：CI/沙箱脚本要在不改配置文件的前提下临时收紧或放开审批模式。
/// - `ZCODE_CONFIG`：直指一份配置文件，跳过两层发现——集成测试与“一份配置驱动多个项目”
///   的场景需要它，且它本身就是本函数之外单独处理的“跳过发现”开关。
/// - `ZCODE_SESSION_DIR`：会话落盘位置常要跟着测试夹具、只读根文件系统或共享存储换地方。
/// - `ZCODE_NO_DAEMON`：调试与 CI 里经常要强制关掉常驻进程，一次性调试不值得改配置文件。
fn env_overrides(env: &impl EnvSource) -> Result<RawConfig, ConfigError> {
    let mut raw = RawConfig::default();
    if let Some(model) = non_empty(env.get("ZCODE_MODEL")) {
        raw.model.id = Some(model);
    }
    if let Some(mode) = non_empty(env.get("ZCODE_APPROVAL_MODE")) {
        raw.approval.mode = Some(parse_approval_mode(&mode)?);
    }
    if let Some(dir) = non_empty(env.get("ZCODE_SESSION_DIR")) {
        raw.session.dir = Some(PathBuf::from(dir));
    }
    if let Some(no_daemon) = non_empty(env.get("ZCODE_NO_DAEMON")) {
        raw.daemon.enabled = Some(!parse_bool_env("ZCODE_NO_DAEMON", &no_daemon)?);
    }
    Ok(raw)
}

/// 把空字符串当作“未设置”处理——`ZCODE_MODEL=` 这种空值不该被当成有效覆盖。
fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.is_empty())
}

/// 解析 `ZCODE_APPROVAL_MODE` 的取值，对应 [`ApprovalMode`] 的 `kebab-case` serde 命名。
fn parse_approval_mode(value: &str) -> Result<ApprovalMode, ConfigError> {
    match value {
        "always-ask" => Ok(ApprovalMode::AlwaysAsk),
        "write" => Ok(ApprovalMode::Write),
        "yolo" => Ok(ApprovalMode::Yolo),
        other => Err(ConfigError::InvalidEnv {
            var: "ZCODE_APPROVAL_MODE",
            value: other.to_string(),
            reason: "必须是 always-ask / write / yolo 之一".to_string(),
        }),
    }
}

/// 解析形如 `ZCODE_NO_DAEMON` 的布尔环境变量。
fn parse_bool_env(var: &'static str, value: &str) -> Result<bool, ConfigError> {
    match value.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => Err(ConfigError::InvalidEnv {
            var,
            value: value.to_string(),
            reason: "必须是 1/true/yes/on 或 0/false/no/off 之一".to_string(),
        }),
    }
}

/// 环境变量读取的抽象。
///
/// [`load`] 只暴露六个 `ZCODE_*` 变量（见 [`env_overrides`] 的文档），读取全部集中在
/// [`env_overrides`] 与 [`load_with_env`]；测试实现本 trait 注入假环境，不必调用
/// `std::env::set_var`（`rule://rust-testing` 禁止修改真实进程环境——那会在并行测试间
/// 互相污染）。
trait EnvSource {
    /// 读取一个环境变量；未设置或非 UTF-8 时返回 `None`。
    fn get(&self, key: &str) -> Option<String>;
}

/// 真实进程环境，`load()` 的默认 [`EnvSource`]。
struct ProcessEnv;

impl EnvSource for ProcessEnv {
    fn get(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }
}

/// 一层配置的原始解析结果：所有字段都是 `Option`，缺省表示“这一层没写”。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct RawConfig {
    model: RawModelConfig,
    approval: RawApprovalConfig,
    tools: RawToolsConfig,
    session: RawSessionConfig,
    daemon: RawDaemonConfig,
    ui: RawUiConfig,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct RawModelConfig {
    id: Option<String>,
    thinking: Option<String>,
    provider: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct RawApprovalConfig {
    mode: Option<ApprovalMode>,
    policies: Option<HashMap<String, Policy>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct RawToolsConfig {
    disabled: Option<Vec<String>>,
    bash_timeout_secs: Option<u64>,
    read_max_lines: Option<usize>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct RawSessionConfig {
    dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct RawDaemonConfig {
    enabled: Option<bool>,
    runtime_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct RawUiConfig {
    show_thinking: Option<bool>,
}

impl RawConfig {
    /// 逐字段覆盖合并：`other`（更高优先级）里 `Some` 的字段覆盖 `self` 的同名字段，
    /// `None` 则保留 `self` 已有的值。`approval.policies` 与 `tools.disabled` 是
    /// 两个例外，语义见模块文档。
    fn merge(self, other: Self) -> Self {
        Self {
            model: self.model.merge(other.model),
            approval: self.approval.merge(other.approval),
            tools: self.tools.merge(other.tools),
            session: self.session.merge(other.session),
            daemon: self.daemon.merge(other.daemon),
            ui: self.ui.merge(other.ui),
        }
    }

    /// 用状态目录派生的默认值填满剩余的 `None`，产出最终 [`Config`]。
    fn finish(self, state_dir: &Path) -> Config {
        Config {
            model: ModelConfig {
                id: self.model.id,
                thinking: self.model.thinking,
                provider: self.model.provider,
            },
            approval: ApprovalConfig {
                mode: self.approval.mode.unwrap_or_default(),
                policies: self.approval.policies.unwrap_or_default(),
            },
            tools: ToolsConfig {
                disabled: self.tools.disabled.unwrap_or_default(),
                bash_timeout_secs: self
                    .tools
                    .bash_timeout_secs
                    .unwrap_or(DEFAULT_BASH_TIMEOUT_SECS),
                read_max_lines: self.tools.read_max_lines.unwrap_or(DEFAULT_READ_MAX_LINES),
            },
            session: SessionConfig {
                dir: self
                    .session
                    .dir
                    .unwrap_or_else(|| state_dir.join("sessions")),
            },
            daemon: DaemonConfig {
                enabled: self.daemon.enabled.unwrap_or(true),
                runtime_dir: self
                    .daemon
                    .runtime_dir
                    .unwrap_or_else(|| state_dir.join("run")),
            },
            ui: UiConfig {
                show_thinking: self.ui.show_thinking.unwrap_or(false),
            },
        }
    }
}

impl RawModelConfig {
    fn merge(self, other: Self) -> Self {
        Self {
            id: other.id.or(self.id),
            thinking: other.thinking.or(self.thinking),
            provider: other.provider.or(self.provider),
        }
    }
}

impl RawApprovalConfig {
    /// `mode` 逐字段覆盖；`policies` 按 entry 合并——高优先级层只覆盖它写到的工具名，
    /// 其余工具名沿用低优先级层（`HashMap::extend` 天然是这个语义：同 key 取后者，
    /// 不同 key 并集）。
    fn merge(self, other: Self) -> Self {
        let policies = match (self.policies, other.policies) {
            (None, None) => None,
            (Some(base), None) => Some(base),
            (None, Some(overrides)) => Some(overrides),
            (Some(mut base), Some(overrides)) => {
                base.extend(overrides);
                Some(base)
            }
        };
        Self {
            mode: other.mode.or(self.mode),
            policies,
        }
    }
}

impl RawToolsConfig {
    /// `disabled` 整体替换：`other` 一旦写了这个字段就完全取代 `self` 的列表，不做并集。
    fn merge(self, other: Self) -> Self {
        Self {
            disabled: other.disabled.or(self.disabled),
            bash_timeout_secs: other.bash_timeout_secs.or(self.bash_timeout_secs),
            read_max_lines: other.read_max_lines.or(self.read_max_lines),
        }
    }
}

impl RawSessionConfig {
    fn merge(self, other: Self) -> Self {
        Self {
            dir: other.dir.or(self.dir),
        }
    }
}

impl RawDaemonConfig {
    fn merge(self, other: Self) -> Self {
        Self {
            enabled: other.enabled.or(self.enabled),
            runtime_dir: other.runtime_dir.or(self.runtime_dir),
        }
    }
}

impl RawUiConfig {
    fn merge(self, other: Self) -> Self {
        Self {
            show_thinking: other.show_thinking.or(self.show_thinking),
        }
    }
}

/// 配置加载失败的原因。
#[derive(Debug, thiserror::Error)]
pub(crate) enum ConfigError {
    /// 配置文件存在但读不出来（权限、非文件等）；文件不存在不算错误，见 [`load_layer`]。
    #[error("读取配置文件 {} 失败：{source}", path.display())]
    Io {
        /// 出问题的文件路径。
        path: PathBuf,
        /// 底层 IO 错误。
        #[source]
        source: std::io::Error,
    },
    /// 配置文件存在但 TOML 语法或字段不合法（含未知字段）。
    #[error("解析配置文件 {} 失败：{source}", path.display())]
    Parse {
        /// 出问题的文件路径。
        path: PathBuf,
        /// `toml` crate 的原始错误，`Display` 自带行列号定位。
        #[source]
        source: Box<toml::de::Error>,
    },
    /// 某个 `ZCODE_*` 环境变量的取值不合法。
    #[error("环境变量 {var} 的值 \"{value}\" 无法解析：{reason}")]
    InvalidEnv {
        /// 变量名。
        var: &'static str,
        /// 用户设置的取值。
        value: String,
        /// 面向用户的原因说明。
        reason: String,
    },
    /// 既没有 `ZCODE_HOME`，也定位不到用户主目录。
    #[error("无法定位用户主目录，请设置 ZCODE_HOME 指定状态目录")]
    NoHomeDir,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试用 [`EnvSource`]：固定的键值表，不碰真实进程环境。
    struct FakeEnv(HashMap<&'static str, String>);

    impl FakeEnv {
        fn new(pairs: &[(&'static str, &str)]) -> Self {
            Self(pairs.iter().map(|(k, v)| (*k, (*v).to_string())).collect())
        }
    }

    impl EnvSource for FakeEnv {
        fn get(&self, key: &str) -> Option<String> {
            self.0.get(key).cloned()
        }
    }

    fn write_toml(dir: &Path, relative: &str, contents: &str) -> PathBuf {
        let path = dir.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("创建配置目录");
        }
        std::fs::write(&path, contents).expect("写配置文件");
        path
    }

    #[test]
    fn field_level_override_across_layers() {
        let state = tempfile::tempdir().expect("创建状态目录");
        write_toml(
            state.path(),
            "config.toml",
            "[model]\nid = \"global-model\"\n\n[tools]\nbash_timeout_secs = 99\n",
        );

        let project = tempfile::tempdir().expect("创建项目目录");
        std::fs::create_dir(project.path().join(".git")).expect("创建 .git 标记");
        write_toml(
            project.path(),
            ".zcode/config.toml",
            "[model]\nprovider = \"anthropic\"\n",
        );

        let env = FakeEnv::new(&[("ZCODE_HOME", state.path().to_str().expect("路径需为 UTF-8"))]);
        let config = load_with_env(project.path(), &env).expect("加载配置");

        // 项目层没写 model.id，最终值必须来自全局层，而不是被“整段覆盖”成 None。
        assert_eq!(config.model.id.as_deref(), Some("global-model"));
        // 项目层写的 provider 生效。
        assert_eq!(config.model.provider.as_deref(), Some("anthropic"));
        // 项目层没碰 tools.bash_timeout_secs，全局层的值原样保留。
        assert_eq!(config.tools.bash_timeout_secs, 99);
    }

    #[test]
    fn policies_merge_by_entry() {
        let state = tempfile::tempdir().expect("创建状态目录");
        write_toml(
            state.path(),
            "config.toml",
            "[approval.policies]\nbash = \"deny\"\ngrep = \"allow\"\n",
        );

        let project = tempfile::tempdir().expect("创建项目目录");
        std::fs::create_dir(project.path().join(".git")).expect("创建 .git 标记");
        write_toml(
            project.path(),
            ".zcode/config.toml",
            "[approval.policies]\nbash = \"allow\"\nwrite = \"prompt\"\n",
        );

        let env = FakeEnv::new(&[("ZCODE_HOME", state.path().to_str().expect("路径需为 UTF-8"))]);
        let config = load_with_env(project.path(), &env).expect("加载配置");

        // 项目层覆盖了 bash 这一个 key。
        assert_eq!(config.approval.policies.get("bash"), Some(&Policy::Allow));
        // 项目层新增了 write。
        assert_eq!(config.approval.policies.get("write"), Some(&Policy::Prompt));
        // 全局层的 grep 没被项目层提到，必须原样保留——不是整段被项目层的 policies 替换掉。
        assert_eq!(config.approval.policies.get("grep"), Some(&Policy::Allow));
    }

    #[test]
    fn tools_disabled_whole_replace() {
        let state = tempfile::tempdir().expect("创建状态目录");
        write_toml(
            state.path(),
            "config.toml",
            "[tools]\ndisabled = [\"bash\", \"grep\"]\n",
        );

        let project = tempfile::tempdir().expect("创建项目目录");
        std::fs::create_dir(project.path().join(".git")).expect("创建 .git 标记");
        write_toml(
            project.path(),
            ".zcode/config.toml",
            "[tools]\ndisabled = [\"todo\"]\n",
        );

        let env = FakeEnv::new(&[("ZCODE_HOME", state.path().to_str().expect("路径需为 UTF-8"))]);
        let config = load_with_env(project.path(), &env).expect("加载配置");

        // 整体替换：结果只有项目层写的 ["todo"]，全局层的 bash/grep 不会被并入。
        assert_eq!(config.tools.disabled, vec!["todo".to_string()]);
    }

    #[test]
    fn zcode_config_env_skips_two_layer_discovery() {
        let state = tempfile::tempdir().expect("创建状态目录");
        write_toml(
            state.path(),
            "config.toml",
            "[model]\nid = \"should-be-ignored\"\n",
        );

        let project = tempfile::tempdir().expect("创建项目目录");
        std::fs::create_dir(project.path().join(".git")).expect("创建 .git 标记");
        write_toml(
            project.path(),
            ".zcode/config.toml",
            "[model]\nid = \"also-ignored\"\n",
        );

        let direct = tempfile::tempdir().expect("创建独立配置目录");
        let direct_path = write_toml(direct.path(), "custom.toml", "[model]\nid = \"direct\"\n");

        let env = FakeEnv::new(&[
            ("ZCODE_HOME", state.path().to_str().expect("路径需为 UTF-8")),
            (
                "ZCODE_CONFIG",
                direct_path.to_str().expect("路径需为 UTF-8"),
            ),
        ]);
        let config = load_with_env(project.path(), &env).expect("加载配置");

        assert_eq!(config.model.id.as_deref(), Some("direct"));
    }

    #[test]
    fn env_overrides_win_over_both_file_layers() {
        let state = tempfile::tempdir().expect("创建状态目录");
        write_toml(
            state.path(),
            "config.toml",
            "[model]\nid = \"from-global-file\"\n",
        );

        let project = tempfile::tempdir().expect("创建项目目录");
        std::fs::create_dir(project.path().join(".git")).expect("创建 .git 标记");
        write_toml(
            project.path(),
            ".zcode/config.toml",
            "[model]\nid = \"from-project-file\"\n",
        );

        let env = FakeEnv::new(&[
            ("ZCODE_HOME", state.path().to_str().expect("路径需为 UTF-8")),
            ("ZCODE_MODEL", "from-env"),
            ("ZCODE_APPROVAL_MODE", "write"),
            ("ZCODE_NO_DAEMON", "true"),
        ]);
        let config = load_with_env(project.path(), &env).expect("加载配置");

        // 项目层已经覆盖了全局层的 model.id，env 变量还要再赢一次。
        assert_eq!(config.model.id.as_deref(), Some("from-env"));
        assert_eq!(config.approval.mode, ApprovalMode::Write);
        assert!(!config.daemon.enabled);
    }

    #[test]
    fn parse_failure_reports_path_and_line() {
        let dir = tempfile::tempdir().expect("创建临时目录");
        // 第二行语法非法：裸露的 `not valid toml` 不是合法的 key/value。
        let path = write_toml(dir.path(), "config.toml", "[model]\nnot valid toml\n");

        let err = load_layer(&path).expect_err("非法 TOML 必须报错");
        match &err {
            ConfigError::Parse { path: err_path, .. } => assert_eq!(err_path, &path),
            other => panic!("期望 ConfigError::Parse，实际是 {other:?}"),
        }
        let message = err.to_string();
        assert!(
            message.contains(path.to_str().expect("路径需为 UTF-8")),
            "{message}"
        );
        assert!(
            message.contains("line 2"),
            "错误消息应带上出错行号：{message}"
        );
    }

    #[test]
    fn unknown_field_rejected() {
        let dir = tempfile::tempdir().expect("创建临时目录");
        let path = write_toml(dir.path(), "config.toml", "totally_unknown_key = 1\n");

        let err = load_layer(&path).expect_err("未知字段必须报错，不能静默忽略");
        assert!(matches!(err, ConfigError::Parse { .. }), "{err:?}");
    }

    #[test]
    fn missing_file_layer_is_not_an_error() {
        let dir = tempfile::tempdir().expect("创建临时目录");
        let raw = load_layer(&dir.path().join("does-not-exist.toml")).expect("文件不存在不是错误");
        assert!(raw.model.id.is_none());
    }
}
