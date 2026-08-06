//! 工具审批：能力档位（tier）× 策略（policy）的裁决，加上一套询问回环。
//!
//! # 两层，别混
//!
//! - **裁决**（[`resolve_approval`]）决定一次调用是放行、拒绝、还是要问。规则模型移植自
//!   oh-my-pi `packages/coding-agent/src/tools/approval.ts:29-185`，语义逐条对齐。
//! - **回环**（[`ApprovalGate`]）负责"问"之后的事：oneshot-by-request-id 等待、
//!   `always` 连锁放行、`reject` 连坐、每次结算都广播、重连可补拉 pending。
//!   语义来自 `plans/runtime-boundary/implementation.md:44-49`（本仓既定契约），
//!   实现形状抄 opencode `packages/opencode/src/permission/index.ts:98-167`。
//!
//! # 为什么裁决用 tier×policy 而不是有序 ruleset
//!
//! opencode 的有序 allow/deny/ask ruleset 表达力更强，但默认 `ask` 意味着开箱即用每个工具
//! 都弹窗。jcode 则根本没有 turn 内回环：`PermissionResult` 的 `Approved`/`Denied`/`Timeout`
//! 三个变体在实际路径上永不产生（`crates/jcode-base/src/safety.rs:180-193`），是死代码撑起
//! 的接口。本仓取 oh-my-pi 的裁决表 + opencode 的回环语义。
//!
//! # 默认是 [`ApprovalMode::Yolo`]
//!
//! **产品取向的显式选择**，不是遗漏：本仓面向单人 power-user，开箱即用不审批
//! （上游同样默认 yolo，`packages/coding-agent/src/config/settings-schema.ts:3675-3678`）。
//! 面向团队 / CI 的部署应调成 [`ApprovalMode::Write`] 或 [`ApprovalMode::AlwaysAsk`]。
//! yolo **不是无条件放行**：工具自己声明的 `deny` 与用户配置的 `deny` 仍然优先。
//!
//! # 重连必须补拉 pending
//!
//! [`ApprovalGate::pending`] 存在的唯一理由就是这个。opencode 的 TUI 漏了这一步
//! （`packages/tui/src/context/sync.tsx:445-534` 的 bootstrap 里没有 `permission.list`），
//! 后果是 SSE 在询问后断开则服务端工具永久挂着而 UI 什么都不显示，会话看起来永久卡死。

use std::collections::HashMap;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

use crate::event::{AgentEvent, EventSink};

/// 工具的能力档位。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    /// 只读：不改变任何外部状态。
    Read,
    /// 写入：修改工作区文件。
    Write,
    /// 执行：跑任意命令、访问网络、驱动外部程序。
    Exec,
}

/// 一次审批的结论。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Policy {
    /// 直接放行。
    Allow,
    /// 直接拒绝，不询问。
    Deny,
    /// 询问用户。
    Prompt,
}

/// 审批模式：决定"多高的档位可以免询问"。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ApprovalMode {
    /// 只有只读工具免询问。
    AlwaysAsk,
    /// 只读与写入免询问，执行类要问。
    Write,
    /// 全部免询问。
    #[default]
    Yolo,
}

impl ApprovalMode {
    /// 本模式下免询问的最高档位。
    fn max_tier(self) -> Tier {
        match self {
            Self::AlwaysAsk => Tier::Read,
            Self::Write => Tier::Write,
            Self::Yolo => Tier::Exec,
        }
    }

    /// 该档位在本模式下是否免询问。
    fn approves(self, tier: Tier) -> bool {
        tier <= self.max_tier()
    }
}

/// 工具自己对一次调用的审批声明。
///
/// 默认档位是 [`Tier::Exec`]——**没声明就按最危险处理**。fail-safe 的方向：新工具作者忘了
/// 声明时，后果是多问一次，而不是静默放过一个 `rm -rf`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalDecision {
    /// 能力档位。
    pub tier: Tier,
    /// 工具自己指定的策略；`None` 表示交给用户配置与模式裁决。
    pub policy: Option<Policy>,
    /// 是否为"强制确认"：即使用户配置更宽松也要问。yolo 模式下忽略。
    pub override_mode: bool,
    /// 给用户看的理由。
    pub reason: Option<String>,
    /// [`ApprovalReply::Always`] 记住的**作用域**；`None` 表示按工具名。
    ///
    /// 存在的理由是粒度：默认作用域是整个工具名，用户点一次"总是允许 bash"就等于放行
    /// 此后**所有** bash 命令。会话内敏感的工具应当自己收窄，例如 `bash` 按命令首词返回
    /// `bash:git`，让"总是允许"只覆盖 git 子集。
    ///
    /// 这是本仓在 tier × policy 模型下对粒度问题的答案。opencode 用的是
    /// `permission + patterns` 的有序 ruleset 重新求值
    /// （`packages/opencode/src/permission/index.ts:145-165`），表达力更强，但那要求整套
    /// ruleset 维度——本仓没有它，硬塞进来会造出与 tier × policy 并行的第二套裁决约定。
    pub always_scope: Option<String>,
}

impl Default for ApprovalDecision {
    fn default() -> Self {
        Self {
            tier: Tier::Exec,
            policy: None,
            override_mode: false,
            reason: None,
            always_scope: None,
        }
    }
}

impl ApprovalDecision {
    /// 只声明档位，其余交给用户配置与模式。
    #[must_use]
    pub fn tier(tier: Tier) -> Self {
        Self {
            tier,
            ..Self::default()
        }
    }

    /// 工具强制要求确认（可被 yolo 模式忽略）。
    #[must_use]
    pub fn require_confirmation(tier: Tier, reason: impl Into<String>) -> Self {
        Self {
            tier,
            policy: Some(Policy::Prompt),
            override_mode: true,
            reason: Some(reason.into()),
            always_scope: None,
        }
    }

    /// 工具自己拒绝这次调用（任何模式下都拒）。
    #[must_use]
    pub fn deny(reason: impl Into<String>) -> Self {
        Self {
            tier: Tier::Exec,
            policy: Some(Policy::Deny),
            override_mode: false,
            reason: Some(reason.into()),
            always_scope: None,
        }
    }

    /// 收窄"总是允许"的作用域。
    #[must_use]
    pub fn with_always_scope(mut self, scope: impl Into<String>) -> Self {
        self.always_scope = Some(scope.into());
        self
    }

    /// 本次调用实际生效的 always 作用域。
    #[must_use]
    pub fn scope_for(&self, tool_name: &str) -> String {
        self.always_scope
            .clone()
            .unwrap_or_else(|| tool_name.to_owned())
    }
}

/// 结论的来源，决定拒绝时给模型的措辞。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalSource {
    /// 工具自身的声明。
    Tool,
    /// 用户的逐工具配置。
    User,
    /// 当前审批模式。
    Mode,
}

/// 裁决结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedApproval {
    /// 最终策略。
    pub policy: Policy,
    /// 档位。
    pub tier: Tier,
    /// 是否来自强制确认。
    pub override_mode: bool,
    /// 来源。
    pub source: ApprovalSource,
    /// 理由。
    pub reason: Option<String>,
}

/// 用户的逐工具策略覆盖：工具名 -> 策略。
pub type UserPolicies = HashMap<String, Policy>;

/// 裁决一次工具调用的审批策略。
///
/// 顺序（与上游 `approval.ts:110-185` 一一对应，任何一步的位置都不可交换）：
///
/// 1. 工具自己 `deny` → 拒（来源 `Tool`）。工具最了解自己的爆炸半径。
/// 2. 用户配置 `deny` → 拒（来源 `User`）。用户的禁令高于模式的宽容。
/// 3. yolo 模式：工具显式策略优先，否则取用户策略、再否则放行。
///    **override 型的强制确认在这里被忽略**——yolo 的语义就是"我知道我在干什么"。
/// 4. override 型：`allow` 就放行，其余一律问。
/// 5. 工具显式 `allow` / `prompt`。
/// 6. 用户配置。
/// 7. 模式档位比较：够格就放行，不够就问。
#[must_use]
pub fn resolve_approval(
    tool_name: &str,
    decision: &ApprovalDecision,
    mode: ApprovalMode,
    user_policies: &UserPolicies,
) -> ResolvedApproval {
    let user_policy = user_policies.get(tool_name).copied();
    let from_tool = |policy: Policy, override_mode: bool| ResolvedApproval {
        policy,
        tier: decision.tier,
        override_mode,
        source: ApprovalSource::Tool,
        reason: decision.reason.clone(),
    };
    // 非工具来源的结论不带工具自己的 reason：那句话解释的是工具的顾虑，
    // 拿去解释"用户把它关了"会误导模型。
    let from = |policy: Policy, source: ApprovalSource| ResolvedApproval {
        policy,
        tier: decision.tier,
        override_mode: false,
        source,
        reason: None,
    };

    if decision.policy == Some(Policy::Deny) {
        return from_tool(Policy::Deny, decision.override_mode);
    }
    if user_policy == Some(Policy::Deny) {
        return from(Policy::Deny, ApprovalSource::User);
    }

    if mode == ApprovalMode::Yolo {
        return match (decision.policy, user_policy) {
            (Some(policy), _) => from_tool(policy, false),
            (None, Some(policy)) => from(policy, ApprovalSource::User),
            (None, None) => from(Policy::Allow, ApprovalSource::Mode),
        };
    }

    if decision.override_mode {
        let policy = if decision.policy == Some(Policy::Allow) {
            Policy::Allow
        } else {
            Policy::Prompt
        };
        return from_tool(policy, true);
    }

    if let Some(policy @ (Policy::Allow | Policy::Prompt)) = decision.policy {
        return from_tool(policy, false);
    }

    if let Some(policy) = user_policy {
        return from(policy, ApprovalSource::User);
    }

    if mode.approves(decision.tier) {
        return from(Policy::Allow, ApprovalSource::Mode);
    }

    from(Policy::Prompt, ApprovalSource::Mode)
}

/// 被拒绝时喂回模型的文本。
///
/// 拒绝**不是**执行错误，它是一条 `is_error` 的工具结果：模型要能读懂"为什么被拦"
/// 并改走别的路径，而不是看到一个空洞的失败。
#[must_use]
pub fn denial_message(tool_name: &str, resolved: &ResolvedApproval) -> String {
    let scope = match resolved.source {
        ApprovalSource::Tool => "工具策略",
        ApprovalSource::User => "用户配置",
        ApprovalSource::Mode => "当前审批模式",
    };
    match &resolved.reason {
        Some(reason) => format!("工具 `{tool_name}` 被{scope}拒绝。原因：{reason}"),
        None => format!("工具 `{tool_name}` 被{scope}拒绝。"),
    }
}

/// 用户对一次询问的答复。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalReply {
    /// 只放行这一次。
    Once,
    /// 放行，并且本会话内**同作用域**的后续调用不再问
    /// （作用域见 [`ApprovalDecision::always_scope`]）。
    Always,
    /// 拒绝。
    Reject,
}

/// 一条待审批请求的公开信息。重连时靠它重建 UI。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingApproval {
    /// 审批请求 id。
    pub request_id: String,
    /// 触发审批的工具调用 id。
    pub call_id: String,
    /// 工具名。
    pub tool_name: String,
    /// `Always` 的作用域：连锁放行只覆盖同作用域的其余待审批。
    pub scope: String,
    /// 展示给用户的提示体。
    pub prompt: String,
}

#[derive(Debug)]
struct PendingSlot {
    info: PendingApproval,
    responder: oneshot::Sender<bool>,
}

/// `always` 的授权键。
///
/// **必须带工具名**：作用域字符串由工具自己声明，两个互不相干的工具都写 `"git"` 是完全
/// 可能的，只按作用域比对会让对其中一个点"总是允许"静默放行另一个。
#[derive(Debug, Clone, PartialEq, Eq)]
struct ScopeKey {
    tool_name: String,
    scope: String,
}

#[derive(Debug, Default)]
struct GateState {
    pending: Vec<PendingSlot>,
    /// 本会话内已经 `always` 放行的授权键。**只在内存**：进程重启即失效，
    /// 与 opencode 一致（`permission/index.ts:48`，其 TUI 文案也明说"until restart"）。
    /// 落盘会让一次误点变成永久授权，那是安全边界的变化，不该由一次点击造成。
    always_allowed: Vec<ScopeKey>,
}

/// 审批询问回环。
#[derive(Debug)]
pub struct ApprovalGate {
    state: Mutex<GateState>,
    events: EventSink,
}

impl ApprovalGate {
    /// 建立一个回环，结算事件推给 `events`。
    #[must_use]
    pub fn new(events: EventSink) -> Self {
        Self {
            state: Mutex::new(GateState::default()),
            events,
        }
    }

    /// 本会话内 `(工具名, 作用域)` 是否已被 `always` 放行。
    #[must_use]
    pub fn is_always_allowed(&self, tool_name: &str, scope: &str) -> bool {
        self.with_state(|state| {
            state
                .always_allowed
                .iter()
                .any(|known| known.tool_name == tool_name && known.scope == scope)
        })
    }

    /// 当前所有待审批请求。**重连的客户端必须调它**，否则界面会漏掉询问而看起来卡死。
    #[must_use]
    pub fn pending(&self) -> Vec<PendingApproval> {
        self.with_state(|state| state.pending.iter().map(|slot| slot.info.clone()).collect())
    }

    /// 发起一次询问，等待答复；返回是否放行。
    ///
    /// 调用方的 future 被丢弃（turn 取消）时，`oneshot::Sender` 那一侧会观测到关闭，
    /// [`ApprovalGate::reply`] 因此不会为幽灵请求阻塞；槽位由
    /// [`ApprovalGate::cancel_all`] 在 turn 收尾时清掉。
    ///
    /// # 三件事必须在同一个临界区里
    ///
    /// 查 `always`、入队、发 [`AgentEvent::ApprovalRequested`]——少合并一件就有一个真实的坏交错：
    ///
    /// - 查与入队分离：B 查到未放行 → A 的 `Always` 记位并清空队列 → B 才入队，
    ///   此后再没有人会结算 B，**调用方永久挂起**。
    /// - 入队与发事件分离：A 入队 → B 的 `Always` 把 A 一起结算并发出 `ApprovalResolved`
    ///   → A 才发 `ApprovalRequested`，客户端收到的顺序是"先结算后询问"，
    ///   **UI 上留下一条永远消不掉的幽灵询问**。
    ///
    /// 在 `std::sync::Mutex` 里发事件是安全的：[`EventSink::emit`] 只是一次 broadcast
    /// `send`，不阻塞、不 `.await`。
    pub async fn ask(&self, call_id: &str, tool_name: &str, scope: &str, prompt: String) -> bool {
        let request_id = crate::id::EntryId::generate().as_str().to_owned();
        let (responder, waiter) = oneshot::channel();
        let info = PendingApproval {
            request_id: request_id.clone(),
            call_id: call_id.to_owned(),
            tool_name: tool_name.to_owned(),
            scope: scope.to_owned(),
            prompt: prompt.clone(),
        };
        let registered = self.with_state(|state| {
            if state
                .always_allowed
                .iter()
                .any(|known| known.tool_name == tool_name && known.scope == scope)
            {
                return false;
            }
            state.pending.push(PendingSlot { info, responder });
            self.events.emit(AgentEvent::ApprovalRequested {
                request_id,
                call_id: call_id.to_owned(),
                prompt,
            });
            true
        });
        if !registered {
            return true;
        }
        // 发送端被丢弃（gate 析构 / turn 取消）时按"未获批准"处理：fail closed。
        waiter.await.unwrap_or(false)
    }

    /// 答复一次询问。返回该 `request_id` 是否存在。
    ///
    /// - [`ApprovalReply::Always`] **连锁**：记住该请求的**作用域**，并把同作用域的其余
    ///   待审批一起放行。作用域默认是工具名，工具可以收窄（见
    ///   [`ApprovalDecision::always_scope`]），所以"总是允许"不必然等于放行整类工具。
    /// - [`ApprovalReply::Reject`] **连坐**：拒掉**全部**待审批。用户表达的是"停"，
    ///   此时继续追问其余请求毫无意义。
    /// - 任何一条待审批消失都会广播 [`AgentEvent::ApprovalResolved`]——这是客户端唯一的
    ///   移除信号，漏发就是 UI 上的幽灵条目。结算与广播同在临界区内，
    ///   因此客户端看到的顺序永远是"先 Requested 后 Resolved"。
    pub fn reply(&self, request_id: &str, reply: ApprovalReply) -> bool {
        self.with_state(|state| {
            let Some(index) = state
                .pending
                .iter()
                .position(|slot| slot.info.request_id == request_id)
            else {
                return false;
            };
            let slot = state.pending.remove(index);
            let approved = reply != ApprovalReply::Reject;

            let mut settled: Vec<(PendingSlot, bool)> = Vec::new();
            match reply {
                ApprovalReply::Always => {
                    let key = ScopeKey {
                        tool_name: slot.info.tool_name.clone(),
                        scope: slot.info.scope.clone(),
                    };
                    if !state.always_allowed.contains(&key) {
                        state.always_allowed.push(key.clone());
                    }
                    let (chained, rest): (Vec<PendingSlot>, Vec<PendingSlot>) =
                        std::mem::take(&mut state.pending)
                            .into_iter()
                            .partition(|other| {
                                other.info.tool_name == key.tool_name
                                    && other.info.scope == key.scope
                            });
                    state.pending = rest;
                    settled.extend(chained.into_iter().map(|other| (other, true)));
                }
                ApprovalReply::Reject => {
                    settled.extend(
                        std::mem::take(&mut state.pending)
                            .into_iter()
                            .map(|other| (other, false)),
                    );
                }
                ApprovalReply::Once => {}
            }
            settled.push((slot, approved));
            self.settle(settled);
            true
        })
    }

    /// 拒掉所有待审批。turn 取消或运行时收尾时调用，避免调用方永久挂着。
    pub fn cancel_all(&self) {
        self.with_state(|state| {
            let settled = std::mem::take(&mut state.pending)
                .into_iter()
                .map(|slot| (slot, false))
                .collect();
            self.settle(settled);
        });
    }

    /// 唤醒等待者并广播结算。**只在持锁时调用**，以保证事件顺序。
    fn settle(&self, settled: Vec<(PendingSlot, bool)>) {
        for (slot, approved) in settled {
            let _ = slot.responder.send(approved);
            self.events.emit(AgentEvent::ApprovalResolved {
                request_id: slot.info.request_id,
                approved,
            });
        }
    }

    /// 在锁内跑一段不含 `.await` 的临界区。
    ///
    /// 用 `std::sync::Mutex` 而不是 tokio 的：临界区里没有 `.await`，
    /// 换成异步锁只会多一次调度。锁毒化时取回内部值继续——审批状态没有"半改坏"的中间态，
    /// 因一个 panic 就让整个会话再也无法审批是更糟的失败模式。
    fn with_state<T>(&self, apply: impl FnOnce(&mut GateState) -> T) -> T {
        let mut guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        apply(&mut guard)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use super::*;

    fn policies(pairs: &[(&str, Policy)]) -> UserPolicies {
        pairs
            .iter()
            .map(|(name, policy)| ((*name).to_owned(), *policy))
            .collect()
    }

    #[test]
    fn tool_deny_beats_everything_including_yolo() {
        let decision = ApprovalDecision::deny("会删掉整个仓库");
        let resolved =
            resolve_approval("bash", &decision, ApprovalMode::Yolo, &UserPolicies::new());
        assert_eq!(resolved.policy, Policy::Deny);
        assert_eq!(resolved.source, ApprovalSource::Tool);
        assert!(denial_message("bash", &resolved).contains("会删掉整个仓库"));
    }

    #[test]
    fn user_deny_beats_yolo() {
        let decision = ApprovalDecision::tier(Tier::Read);
        let resolved = resolve_approval(
            "read",
            &decision,
            ApprovalMode::Yolo,
            &policies(&[("read", Policy::Deny)]),
        );
        assert_eq!(resolved.policy, Policy::Deny);
        assert_eq!(resolved.source, ApprovalSource::User);
        assert!(resolved.reason.is_none(), "用户的禁令不该借用工具的理由");
    }

    #[test]
    fn yolo_ignores_tool_forced_confirmation() {
        // yolo 与 write 模式的分水岭：**无显式 policy** 的 override 在 yolo 下失效。
        let bare_override = ApprovalDecision {
            override_mode: true,
            ..ApprovalDecision::tier(Tier::Exec)
        };
        let empty = UserPolicies::new();
        assert_eq!(
            resolve_approval("write", &bare_override, ApprovalMode::Yolo, &empty).policy,
            Policy::Allow
        );
        assert_eq!(
            resolve_approval("write", &bare_override, ApprovalMode::Write, &empty).policy,
            Policy::Prompt
        );

        // 但工具**显式**写了 prompt 的，yolo 下仍然生效。
        let explicit = ApprovalDecision::require_confirmation(Tier::Exec, "不可逆");
        assert_eq!(
            resolve_approval("write", &explicit, ApprovalMode::Yolo, &empty).policy,
            Policy::Prompt
        );
    }

    #[test]
    fn missing_declaration_defaults_to_the_most_dangerous_tier() {
        let decision = ApprovalDecision::default();
        assert_eq!(decision.tier, Tier::Exec);
        let resolved = resolve_approval(
            "mystery",
            &decision,
            ApprovalMode::Write,
            &UserPolicies::new(),
        );
        assert_eq!(resolved.policy, Policy::Prompt, "没声明档位就必须问");
    }

    #[test]
    fn mode_tier_comparison_is_the_last_resort() {
        let read = ApprovalDecision::tier(Tier::Read);
        let write = ApprovalDecision::tier(Tier::Write);
        let exec = ApprovalDecision::tier(Tier::Exec);
        let empty = UserPolicies::new();

        for (mode, expected) in [
            (
                ApprovalMode::AlwaysAsk,
                [Policy::Allow, Policy::Prompt, Policy::Prompt],
            ),
            (
                ApprovalMode::Write,
                [Policy::Allow, Policy::Allow, Policy::Prompt],
            ),
            (
                ApprovalMode::Yolo,
                [Policy::Allow, Policy::Allow, Policy::Allow],
            ),
        ] {
            let actual = [&read, &write, &exec]
                .map(|decision| resolve_approval("t", decision, mode, &empty).policy);
            assert_eq!(actual, expected, "模式 {mode:?} 的档位阈值不对");
        }
    }

    #[test]
    fn user_policy_outranks_mode_but_not_the_tool() {
        let bare = ApprovalDecision::tier(Tier::Exec);
        let allowed = resolve_approval(
            "bash",
            &bare,
            ApprovalMode::AlwaysAsk,
            &policies(&[("bash", Policy::Allow)]),
        );
        assert_eq!(allowed.policy, Policy::Allow);
        assert_eq!(allowed.source, ApprovalSource::User);

        let tool_prompt = ApprovalDecision {
            policy: Some(Policy::Prompt),
            ..ApprovalDecision::tier(Tier::Exec)
        };
        let still_prompt = resolve_approval(
            "bash",
            &tool_prompt,
            ApprovalMode::AlwaysAsk,
            &policies(&[("bash", Policy::Allow)]),
        );
        assert_eq!(still_prompt.policy, Policy::Prompt);
        assert_eq!(still_prompt.source, ApprovalSource::Tool);
    }

    #[tokio::test]
    async fn once_settles_only_its_own_request() {
        let gate = ApprovalGate::new(EventSink::new());
        let asking = {
            let gate = &gate;
            async move { gate.ask("call_1", "bash", "bash", "跑吗".to_owned()).await }
        };
        let replying = async {
            let request = wait_for_pending(&gate, 1).await;
            let Some(first) = request.first() else {
                panic!("必须有一条待审批");
            };
            assert!(gate.reply(&first.request_id, ApprovalReply::Once));
            assert!(!gate.is_always_allowed("bash", "bash"), "once 不该记住");
        };
        let (approved, ()) = tokio::join!(asking, replying);
        assert!(approved);
        assert!(gate.pending().is_empty());
    }

    #[tokio::test]
    async fn always_chains_to_other_pending_calls_of_the_same_tool() {
        let gate = ApprovalGate::new(EventSink::new());
        let first = async { gate.ask("call_1", "bash", "bash", "a".to_owned()).await };
        let second = async { gate.ask("call_2", "bash", "bash", "b".to_owned()).await };
        let third = async { gate.ask("call_3", "write", "write", "c".to_owned()).await };
        let replying = async {
            let pending = wait_for_pending(&gate, 3).await;
            let Some(bash) = pending.iter().find(|slot| slot.tool_name == "bash") else {
                panic!("必须有 bash 的待审批");
            };
            assert!(gate.reply(&bash.request_id, ApprovalReply::Always));
            // 连锁只覆盖同名工具，write 仍然挂着。
            assert_eq!(gate.pending().len(), 1);
            let Some(write) = gate.pending().first().cloned() else {
                panic!("write 仍应待审批");
            };
            assert!(gate.reply(&write.request_id, ApprovalReply::Once));
        };
        let (a, b, c, ()) = tokio::join!(first, second, third, replying);
        assert!(a && b && c);
        assert!(gate.is_always_allowed("bash", "bash"));
        // 记住之后再问同一个工具就不会再产生待审批。
        assert!(gate.ask("call_4", "bash", "bash", "d".to_owned()).await);
    }

    #[tokio::test]
    async fn always_respects_a_narrowed_scope() {
        // 粒度契约：批准"总是允许 git"绝不能顺带放行 `rm`。
        let gate = ApprovalGate::new(EventSink::new());
        let git = async {
            gate.ask("call_1", "bash", "bash:git", "git 状态".to_owned())
                .await
        };
        let removing = async {
            gate.ask("call_2", "bash", "bash:rm", "删文件".to_owned())
                .await
        };
        let replying = async {
            let pending = wait_for_pending(&gate, 2).await;
            let Some(slot) = pending.iter().find(|slot| slot.scope == "bash:git") else {
                panic!("git 的待审批必须存在");
            };
            assert!(gate.reply(&slot.request_id, ApprovalReply::Always));
            assert_eq!(gate.pending().len(), 1, "另一个作用域不该被连锁放行");
            let Some(rest) = gate.pending().first().cloned() else {
                panic!("rm 仍应待审批");
            };
            assert_eq!(rest.scope, "bash:rm");
            assert!(gate.reply(&rest.request_id, ApprovalReply::Reject));
        };
        let (allowed, denied, ()) = tokio::join!(git, removing, replying);
        assert!(allowed);
        assert!(!denied);
        assert!(gate.is_always_allowed("bash", "bash:git"));
        assert!(
            !gate.is_always_allowed("bash", "bash"),
            "收窄的作用域不得放行整个工具"
        );
    }

    #[tokio::test]
    async fn always_does_not_leak_across_tools_that_declare_the_same_scope() {
        // 作用域字符串由工具自己取名，撞名是必然的。授权键必须带工具名。
        let gate = ApprovalGate::new(EventSink::new());
        let first = async { gate.ask("call_1", "vcs", "git", "a".to_owned()).await };
        let second = async { gate.ask("call_2", "shell", "git", "b".to_owned()).await };
        let replying = async {
            let pending = wait_for_pending(&gate, 2).await;
            let Some(vcs) = pending.iter().find(|slot| slot.tool_name == "vcs") else {
                panic!("vcs 的待审批必须存在");
            };
            assert!(gate.reply(&vcs.request_id, ApprovalReply::Always));
            assert_eq!(
                gate.pending().len(),
                1,
                "同名作用域的另一个工具不得被连带放行"
            );
            let Some(shell) = gate.pending().first().cloned() else {
                panic!("shell 仍应待审批");
            };
            assert!(gate.reply(&shell.request_id, ApprovalReply::Reject));
        };
        let (allowed, denied, ()) = tokio::join!(first, second, replying);
        assert!(allowed);
        assert!(!denied);
        assert!(gate.is_always_allowed("vcs", "git"));
        assert!(!gate.is_always_allowed("shell", "git"));
    }

    #[tokio::test]
    async fn reject_cascades_to_every_pending_request() {
        let gate = ApprovalGate::new(EventSink::new());
        let first = async { gate.ask("call_1", "bash", "bash", "a".to_owned()).await };
        let second = async { gate.ask("call_2", "write", "write", "b".to_owned()).await };
        let replying = async {
            let pending = wait_for_pending(&gate, 2).await;
            let Some(any) = pending.first() else {
                panic!("必须有待审批");
            };
            assert!(gate.reply(&any.request_id, ApprovalReply::Reject));
        };
        let (a, b, ()) = tokio::join!(first, second, replying);
        assert!(!a, "被拒的请求必须返回 false");
        assert!(!b, "连坐必须拒掉其余请求");
        assert!(gate.pending().is_empty());
    }

    #[tokio::test]
    async fn every_settlement_broadcasts_so_clients_can_clear_their_ui() {
        let events = EventSink::new();
        let mut stream = events.subscribe();
        let gate = ApprovalGate::new(events);
        let asking = async { gate.ask("call_1", "bash", "bash", "a".to_owned()).await };
        let replying = async {
            let pending = wait_for_pending(&gate, 1).await;
            let Some(first) = pending.first() else {
                panic!("必须有待审批");
            };
            gate.reply(&first.request_id, ApprovalReply::Once);
        };
        let (_, ()) = tokio::join!(asking, replying);

        let Some(AgentEvent::ApprovalRequested { request_id, .. }) = stream.recv().await else {
            panic!("先发询问事件");
        };
        assert_eq!(
            stream.recv().await,
            Some(AgentEvent::ApprovalResolved {
                request_id,
                approved: true
            })
        );
    }

    #[tokio::test]
    async fn cancel_all_unblocks_every_waiter() {
        let gate = ApprovalGate::new(EventSink::new());
        let asking = async { gate.ask("call_1", "bash", "bash", "a".to_owned()).await };
        let cancelling = async {
            wait_for_pending(&gate, 1).await;
            gate.cancel_all();
        };
        let (approved, ()) = tokio::join!(asking, cancelling);
        assert!(!approved, "取消必须 fail closed");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_asks_never_outlive_an_always_reply() {
        // 回归：曾经"查 always"与"入队"分处两个临界区，交错
        //   B 查到未放行 → A 的 Always 记位并清队列 → B 才入队
        // 会让 B 永久挂起。锤 200 轮，任何一轮挂住都会在这里超时。
        for _ in 0..200 {
            let gate = Arc::new(ApprovalGate::new(EventSink::new()));
            let mut asks = Vec::new();
            for index in 0..4 {
                let gate = Arc::clone(&gate);
                asks.push(tokio::spawn(async move {
                    gate.ask(&format!("call_{index}"), "bash", "bash", "跑吗".to_owned())
                        .await
                }));
            }
            let replying = {
                let gate = Arc::clone(&gate);
                tokio::spawn(async move {
                    let pending = loop {
                        let pending = gate.pending();
                        if let Some(first) = pending.first() {
                            break first.request_id.clone();
                        }
                        tokio::task::yield_now().await;
                    };
                    gate.reply(&pending, ApprovalReply::Always);
                })
            };

            let settle = async {
                for ask in asks {
                    assert!(ask.await.expect("ask 任务不应 panic"));
                }
                replying.await.expect("reply 任务不应 panic");
            };
            tokio::time::timeout(Duration::from_secs(5), settle)
                .await
                .expect("Always 之后不得有询问永久挂起");
        }
    }

    #[test]
    fn replying_to_an_unknown_request_is_reported_not_ignored() {
        let gate = ApprovalGate::new(EventSink::new());
        assert!(!gate.reply("ent_nope", ApprovalReply::Once));
    }

    /// 轮询到 `count` 条待审批为止。`ask` 是在另一个 future 里注册的，
    /// 直接读会有竞态。
    async fn wait_for_pending(gate: &ApprovalGate, count: usize) -> Vec<PendingApproval> {
        loop {
            let pending = gate.pending();
            if pending.len() >= count {
                return pending;
            }
            tokio::task::yield_now().await;
        }
    }
}
