//! 取消注册表：把"取消这个会话"翻译成"打到所有相关中断信号上"。
//!
//! # 为什么需要一张表，而不是一个 `CancellationToken`
//!
//! 取消请求到达时**只知道会话 id**：它从 wire 层过来，携带的是 `session`，不是某个 turn
//! 对象或某个连接。一个会话同时可能有多个在飞的 turn（主 turn + 子 agent），以及若干后台
//! 作业，它们各自持有**不同的** [`InterruptSignal`] 实例。所以需要 session → 信号集合的
//! 映射。抄源 jcode `crates/jcode-app-core/src/turn_cancel_registry.rs`。
//!
//! `tokio_util::sync::CancellationToken` 顶不上来，理由有两条，都在上游踩过：
//!
//! 1. 同一次取消要打到**可能是多个实例**的信号上，token 的父子树要求提前建好层级关系，
//!    而 turn 是随时注册进来的；
//! 2. 取消之后需要**延时复位且不能抹掉期间的新 fire**，靠的是
//!    [`InterruptSignal::reset_if_epoch`]，token 没有 epoch 概念（jcode issue #428：
//!    连按两下 Esc，第一次的定时复位把第二次取消擦掉）。
//!
//! # 与 jcode 的两点不同
//!
//! - **不是进程级 `static`。** jcode 用 `static ACTIVE_TURNS: LazyLock<Mutex<HashMap<..>>>`，
//!   代价它自己记了：测试必须靠唯一 session id 字符串隔离，同名 session 无法并行跑。
//!   本仓把表做成可持有对象，daemon 持一份、测试各持一份。
//! - **级联到后台作业。** jcode 只登记 turn；本表还登记 job，且 job 可以声明自己拥有一个
//!   **子会话**，取消沿子会话递归下去（opencode
//!   `packages/opencode/src/session/run-state.ts:108-140`）。
//!
//! # 顺序：先 job、再 runner，且循环到无新增
//!
//! [`CancelRegistry::cancel_session`] 不是"一次快照全部 fire"。job 从收到取消到真正退出
//! 之间**仍然可能登记新的子 job / 子会话**；一次快照会漏掉它们，并且先把 runner 打断，
//! 留下脱管的后台进程。所以是"取一批 → 释放锁 → fire → 重新取锁看有没有新增"，直到一轮
//! 没有新 token，最后才 fire turn 信号。
//!
//! fire 时**不持锁**：`fire()` 会 `notify_waiters()` 唤醒任务，持锁唤醒等于把整张表的
//! 争用面扩大到被唤醒任务的调度延迟上。

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use crate::id::SessionId;
use crate::interrupt::InterruptSignal;

/// 一轮 `cancel_session` 最多重扫多少次。
///
/// 这是**防病态**的安全阀，不是调优值：正常情况下第二轮就没有新增了。一个不断派生新作业
/// 的失控子系统会让"循环到无新增"变成活锁，届时宁可漏掉后来者也不能让取消路径卡死——
/// 触顶会打 `warn!`，那说明有个作业在取消期间还在生产新作业，是缺陷不是负载。
const MAX_CASCADE_PASSES: usize = 64;

/// 注册表内部为每次注册分配的序号。
///
/// 按序号删除而不是按信号删除：同一个 [`InterruptSignal`] 完全可能被注册两次（一个 turn
/// 同时挂在 turn 表与某个 job 上），[`InterruptSignal::same_instance`] 只能判指针相等，
/// 按它删会误删兄弟登记。抄源 jcode `turn_cancel_registry.rs:31,54,91`。
type Token = u64;

#[derive(Debug)]
struct TurnSlot {
    token: Token,
    signal: InterruptSignal,
}

#[derive(Debug)]
struct JobSlot {
    token: Token,
    signal: InterruptSignal,
    /// 本作业自己拥有的子会话。取消沿它递归。
    child: Option<SessionId>,
}

#[derive(Debug, Default)]
struct RegistryState {
    turns: HashMap<SessionId, Vec<TurnSlot>>,
    jobs: HashMap<SessionId, Vec<JobSlot>>,
    next_token: Token,
}

/// 一次 [`CancelRegistry::cancel_session`] 的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CancelReport {
    /// 被 fire 的后台作业数。
    pub jobs: usize,
    /// 被 fire 的 turn 数。
    pub turns: usize,
    /// 级联覆盖到的会话数（含根会话）。
    pub sessions: usize,
    /// 级联触顶：仍有作业在取消期间不断派生新作业，本次没能扫到稳定。
    ///
    /// **runner 依然被取消了**——那是唯一能掐断新作业来源的动作，留着它只会让脱管作业
    /// 继续增长。这个标志的用途是让调用方知道"可能残留后台进程"，据此升级处理
    /// （打日志、提示用户、或走强杀路径），而不是当成一次干净的取消。
    pub cascade_exhausted: bool,
}

/// 会话 → 在飞 turn / 后台作业的中断信号表。
#[derive(Debug, Default)]
pub struct CancelRegistry {
    state: Mutex<RegistryState>,
    /// 仅测试：每轮 fire 之后、重新取锁之前调用一次。
    ///
    /// "取消期间新增作业"是本模块最关键也最不可观测的契约：没有这个钩子，任何测试都只能
    /// 断言一次 BFS 快照同样能满足的性质，等于没测。钩子在**不持锁**时调用，否则测试里
    /// 的登记动作会自死锁。
    #[cfg(test)]
    pass_hook: PassHook,
}

/// 仅测试用的每轮回调槽。
#[cfg(test)]
type PassCallback = Box<dyn Fn(usize) + Send>;

/// 仅测试用的每轮回调槽。
#[cfg(test)]
#[derive(Default)]
struct PassHook(Mutex<Option<PassCallback>>);

#[cfg(test)]
impl std::fmt::Debug for PassHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PassHook")
    }
}

#[cfg(test)]
impl PassHook {
    fn run(&self, pass: usize) {
        let guard = match self.0.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some(hook) = guard.as_ref() {
            hook(pass);
        }
    }
}

impl CancelRegistry {
    /// 造一张空表。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 登记一个在飞 turn，返回的守卫 drop 时注销。
    ///
    /// 守卫的 `Drop` 会 `reset()` 这个信号：取消可能经由一个**外部** handle 打到本 turn 的
    /// 信号上，而没有任何其他人会清它；不清就是永久置位，该 agent 上的下一个 turn 一启动
    /// 就被中断。这是 jcode `turn_cancel_registry.rs:103-107` 明确写下的坑。
    pub fn register_turn(
        self: &Arc<Self>,
        session: &SessionId,
        signal: InterruptSignal,
    ) -> TurnRegistration {
        let token = self.with_state(|state| {
            let token = state.take_token();
            state
                .turns
                .entry(session.clone())
                .or_default()
                .push(TurnSlot {
                    token,
                    signal: signal.clone(),
                });
            token
        });
        TurnRegistration {
            registry: Arc::clone(self),
            session: session.clone(),
            token,
            signal,
        }
    }

    /// 登记一个后台作业。
    ///
    /// `child` 是本作业自己拥有的子会话（例如一个子 agent 作业）。取消根会话时会沿它递归
    /// 下去，否则子 agent 会在父会话被取消后继续烧 token。
    pub fn register_job(
        self: &Arc<Self>,
        session: &SessionId,
        signal: InterruptSignal,
        child: Option<SessionId>,
    ) -> JobRegistration {
        let token = self.with_state(|state| {
            let token = state.take_token();
            state
                .jobs
                .entry(session.clone())
                .or_default()
                .push(JobSlot {
                    token,
                    signal: signal.clone(),
                    child,
                });
            token
        });
        JobRegistration {
            registry: Arc::clone(self),
            session: session.clone(),
            token,
            signal,
        }
    }

    /// 该会话当前在飞的 turn 信号。
    #[must_use]
    pub fn active_turn_signals(&self, session: &SessionId) -> Vec<InterruptSignal> {
        self.with_state(|state| {
            state
                .turns
                .get(session)
                .map(|slots| slots.iter().map(|slot| slot.signal.clone()).collect())
                .unwrap_or_default()
        })
    }

    /// 该会话当前是否有 turn 在跑。
    #[must_use]
    pub fn is_turn_active(&self, session: &SessionId) -> bool {
        self.with_state(|state| {
            state
                .turns
                .get(session)
                .is_some_and(|slots| !slots.is_empty())
        })
    }

    /// 取消一个会话：先递归打完所有后台作业，再打 turn。
    ///
    /// 顺序不可交换。先打 runner 会让"作业收到取消 → 退出前又登记了一个子作业"这条路径
    /// 留下脱管进程（opencode `packages/opencode/src/session/run-state.ts:108-140`）。
    pub fn cancel_session(&self, session: &SessionId) -> CancelReport {
        let mut visited: HashSet<SessionId> = HashSet::new();
        visited.insert(session.clone());
        let mut fired_tokens: HashSet<Token> = HashSet::new();
        // 去重表跨 job 与 turn 两个阶段共用：同一个信号既可能作为 turn 登记、又作为 job
        // 登记（一个 turn 自己就是某个父会话的作业），两张表各去各的会把它 fire 两次。
        let mut fired = Vec::new();
        let mut fired_jobs = 0_usize;
        let mut cascade_exhausted = false;

        for pass in 0..MAX_CASCADE_PASSES {
            // 取一批：只收还没 fire 过的 token，同时把它们声明的子会话并进 visited。
            let (batch, discovered) = self.with_state(|state| {
                let mut batch = Vec::new();
                let mut discovered = Vec::new();
                for owner in &visited {
                    let Some(slots) = state.jobs.get(owner) else {
                        continue;
                    };
                    for slot in slots {
                        if fired_tokens.contains(&slot.token) {
                            continue;
                        }
                        if let Some(child) = &slot.child
                            && !visited.contains(child)
                        {
                            discovered.push(child.clone());
                        }
                        batch.push((slot.token, slot.signal.clone()));
                    }
                }
                (batch, discovered)
            });
            visited.extend(discovered);
            if batch.is_empty() {
                break;
            }
            for (token, signal) in batch {
                fired_tokens.insert(token);
                if fire_once(&mut fired, &signal) {
                    fired_jobs += 1;
                }
            }
            #[cfg(test)]
            self.pass_hook.run(pass);
            if pass + 1 == MAX_CASCADE_PASSES {
                cascade_exhausted = true;
                tracing::warn!(
                    session = %session,
                    passes = MAX_CASCADE_PASSES,
                    "取消级联触顶：仍有作业在取消期间派生新作业，剩余作业不再追打"
                );
            }
        }

        // runner 最后打。触顶时**依然要打**：runner 是新作业的唯一来源，留着它只会让
        // 脱管作业继续增长；调用方靠 `cascade_exhausted` 知道这次取消不干净。
        let turn_signals = self.with_state(|state| {
            let mut signals = Vec::new();
            for owner in &visited {
                if let Some(slots) = state.turns.get(owner) {
                    signals.extend(slots.iter().map(|slot| slot.signal.clone()));
                }
            }
            signals
        });
        let mut fired_turns = 0_usize;
        for signal in &turn_signals {
            if fire_once(&mut fired, signal) {
                fired_turns += 1;
            }
        }

        CancelReport {
            jobs: fired_jobs,
            turns: fired_turns,
            sessions: visited.len(),
            cascade_exhausted,
        }
    }

    fn with_state<T>(&self, f: impl FnOnce(&mut RegistryState) -> T) -> T {
        // 锁中毒不该让取消路径 panic：一个 panic 过的持锁者留下的状态对本表来说仍然可用
        // （只有 HashMap 与计数器），退化成 `into_inner` 继续跑，好过让取消整条失效。
        let mut guard = match self.state.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        f(&mut guard)
    }

    fn deregister_turn(&self, session: &SessionId, token: Token) {
        self.with_state(|state| {
            if let Some(slots) = state.turns.get_mut(session) {
                slots.retain(|slot| slot.token != token);
                if slots.is_empty() {
                    state.turns.remove(session);
                }
            }
        });
    }

    fn deregister_job(&self, session: &SessionId, token: Token) {
        self.with_state(|state| {
            if let Some(slots) = state.jobs.get_mut(session) {
                slots.retain(|slot| slot.token != token);
                if slots.is_empty() {
                    state.jobs.remove(session);
                }
            }
        });
    }
}

impl RegistryState {
    fn take_token(&mut self) -> Token {
        self.next_token = self.next_token.wrapping_add(1);
        self.next_token
    }
}

/// fire 一个信号，跳过与已 fire 过的信号同源的那个。返回是否真的 fire 了。
///
/// 同一个 `AtomicBool` 被 fire 两次不会出错，但会白白多一次 epoch 自增，让持有延时复位的
/// 一方误以为"期间又被取消过"从而跳过复位。抄源 jcode `server/state.rs:611-613` 的
/// `same_instance` 去重。
fn fire_once(fired: &mut Vec<InterruptSignal>, signal: &InterruptSignal) -> bool {
    if fired.iter().any(|seen| seen.same_instance(signal)) {
        return false;
    }
    signal.fire();
    fired.push(signal.clone());
    true
}

/// 一次 turn 登记的守卫。drop 即注销并复位信号。
#[derive(Debug)]
pub struct TurnRegistration {
    registry: Arc<CancelRegistry>,
    session: SessionId,
    token: Token,
    signal: InterruptSignal,
}

impl Drop for TurnRegistration {
    fn drop(&mut self) {
        self.registry.deregister_turn(&self.session, self.token);
        // 取消标志绝不能活得比 turn 长，理由见 `register_turn` 的文档。
        self.signal.reset();
    }
}

/// 一次后台作业登记的守卫。drop 即注销并复位信号。
#[derive(Debug)]
pub struct JobRegistration {
    registry: Arc<CancelRegistry>,
    session: SessionId,
    token: Token,
    signal: InterruptSignal,
}

impl Drop for JobRegistration {
    fn drop(&mut self) {
        self.registry.deregister_job(&self.session, self.token);
        self.signal.reset();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::{CancelRegistry, JobRegistration};
    use crate::id::SessionId;
    use crate::interrupt::InterruptSignal;

    fn session(name: &str) -> SessionId {
        SessionId::from(name.to_owned())
    }

    #[test]
    fn cancel_fires_every_turn_of_the_session() {
        let registry = Arc::new(CancelRegistry::new());
        let ses = session("ses_a");
        let first = InterruptSignal::new();
        let second = InterruptSignal::new();
        let _a = registry.register_turn(&ses, first.clone());
        let _b = registry.register_turn(&ses, second.clone());

        let report = registry.cancel_session(&ses);

        assert_eq!(report.turns, 2);
        assert!(first.is_set() && second.is_set());
    }

    #[test]
    fn other_sessions_are_untouched() {
        let registry = Arc::new(CancelRegistry::new());
        let mine = InterruptSignal::new();
        let theirs = InterruptSignal::new();
        let _a = registry.register_turn(&session("ses_a"), mine.clone());
        let _b = registry.register_turn(&session("ses_b"), theirs.clone());

        registry.cancel_session(&session("ses_a"));

        assert!(mine.is_set());
        assert!(!theirs.is_set(), "取消绝不能溢出到别的会话");
    }

    #[test]
    fn dropping_the_guard_deregisters_and_resets() {
        let registry = Arc::new(CancelRegistry::new());
        let ses = session("ses_a");
        let signal = InterruptSignal::new();
        {
            let _guard = registry.register_turn(&ses, signal.clone());
            registry.cancel_session(&ses);
            assert!(signal.is_set());
        }
        assert!(
            !signal.is_set(),
            "标志活得比 turn 长就会秒杀下一个 turn（jcode turn_cancel_registry.rs:103-107）"
        );
        assert!(registry.active_turn_signals(&ses).is_empty());
        assert!(!registry.is_turn_active(&ses));
    }

    #[test]
    fn cancel_cascades_into_child_sessions() {
        let registry = Arc::new(CancelRegistry::new());
        let root = session("ses_root");
        let child = session("ses_child");
        let grandchild = session("ses_grandchild");

        let job_signal = InterruptSignal::new();
        let nested_job = InterruptSignal::new();
        let child_turn = InterruptSignal::new();
        let grandchild_turn = InterruptSignal::new();

        let _root_job = registry.register_job(&root, job_signal.clone(), Some(child.clone()));
        let _child_job =
            registry.register_job(&child, nested_job.clone(), Some(grandchild.clone()));
        let _child_turn = registry.register_turn(&child, child_turn.clone());
        let _grandchild_turn = registry.register_turn(&grandchild, grandchild_turn.clone());

        let report = registry.cancel_session(&root);

        assert_eq!(report.jobs, 2);
        assert_eq!(report.turns, 2);
        assert_eq!(report.sessions, 3);
        assert!(job_signal.is_set() && nested_job.is_set());
        assert!(child_turn.is_set() && grandchild_turn.is_set());
    }

    #[test]
    fn deep_job_chains_need_more_than_one_pass() {
        // 每一层的作业只能从上一层的 `child` 字段发现。若实现只对根会话取一次快照就
        // 统一 fire，这里只会打中第一层，第 4、5 层留成脱管后台进程。
        let registry = Arc::new(CancelRegistry::new());
        let chain: Vec<SessionId> = (0..5).map(|i| session(&format!("ses_{i}"))).collect();
        let signals: Vec<InterruptSignal> = (0..5).map(|_| InterruptSignal::new()).collect();

        let mut guards = Vec::new();
        for (index, owner) in chain.iter().enumerate() {
            let child = chain.get(index + 1).cloned();
            let signal = signals.get(index).cloned().expect("信号与链等长");
            guards.push(registry.register_job(owner, signal, child));
        }

        let root = chain.first().cloned().expect("链非空");
        let report = registry.cancel_session(&root);

        assert_eq!(report.jobs, 5, "每一层的作业都必须被打到");
        assert_eq!(report.sessions, 5);
        assert!(signals.iter().all(InterruptSignal::is_set));
        drop(guards);
    }

    #[test]
    fn a_job_registered_during_cancellation_is_still_fired() {
        // 真正的竞态：作业收到取消、退出前又登记了一个新作业。一次 BFS 快照会漏掉它，
        // 并且已经把 runner 打了——留下脱管后台进程。钩子在第 0 轮 fire 之后登记，
        // 因此只有"重新取锁再扫一轮"的实现才能打中它。
        let registry = Arc::new(CancelRegistry::new());
        let root = session("ses_root");
        let first = InterruptSignal::new();
        let late = InterruptSignal::new();
        let _root_job = registry.register_job(&root, first.clone(), None);

        let late_guard: Arc<Mutex<Option<JobRegistration>>> = Arc::new(Mutex::new(None));
        {
            // 用 Weak：钩子存在注册表自己的字段里，捕获 Arc 会成环、把表泄漏掉。
            let hook_registry = Arc::downgrade(&registry);
            let hook_root = root.clone();
            let hook_signal = late.clone();
            let hook_slot = Arc::clone(&late_guard);
            let mut slot = registry.pass_hook.0.lock().expect("测试内新建的锁不会中毒");
            *slot = Some(Box::new(move |pass| {
                if pass != 0 {
                    return;
                }
                let Some(registry) = hook_registry.upgrade() else {
                    return;
                };
                let guard = registry.register_job(&hook_root, hook_signal.clone(), None);
                let mut held = hook_slot.lock().expect("测试内新建的锁不会中毒");
                *held = Some(guard);
            }));
        }
        let report = registry.cancel_session(&root);

        assert_eq!(report.jobs, 2, "取消期间新登记的作业必须在后续轮次被扫到");
        assert!(late.is_set());
        assert!(!report.cascade_exhausted);
    }

    #[test]
    fn the_same_signal_registered_twice_fires_once() {
        let registry = Arc::new(CancelRegistry::new());
        let ses = session("ses_a");
        let shared = InterruptSignal::new();
        let _turn = registry.register_turn(&ses, shared.clone());
        let _job = registry.register_job(&ses, shared.clone(), None);

        let before = shared.epoch();
        registry.cancel_session(&ses);

        assert_eq!(
            shared.epoch(),
            before + 1,
            "同一个信号被重复 fire 会多推一次 epoch，让延时复位误判为期间又被取消过"
        );
    }
}
