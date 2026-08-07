//! 应用状态：把 wire 协议事件、按键动作、展示节奏、重绘节流揉进一份
//! 单一状态，供 [`crate::app`] 的事件循环驱动。除了 [`AppState::build_components`]
//! 返回的渲染句柄以外，本模块不触碰 [`zcode_tui`]/终端——它只产生"状态"和
//! "该发哪些请求"（[`Effect`]），真正的 IO 由调用方（`mod.rs`）执行。

use std::collections::HashMap;
use std::time::Instant;

use zcode_protocol::wire::types::{
    ApprovalReply, AssistantContent, CallId, ClientId, Entry, EntryId, Message, SessionId,
    ToolProgress as WireToolProgress, UserContent,
};
use zcode_protocol::wire::{Event, Pending, Request};

use crate::app::ids;
use crate::app::input::{InputComponent, InputState};
use crate::app::pending::{Front, PendingComponent, PendingState};
use crate::app::reveal::RevealPacer;
use crate::app::status::{self, StatusComponent, StatusKind};
use crate::app::transcript::{
    Block, BlockComponent, ToolStatus, TranscriptEntry, entry_to_block, message_to_block,
};
use zcode_tui::Component;
use zcode_tui::theme::Theme;

/// 一次 `apply_event`/按键处理之后，调用方需要额外发出的请求。
///
/// 之所以不在 `apply_event` 内部直接 `await session.request(...)`：`AppState`
/// 保持同步、无 IO，纯状态机方便单元测试；真正的网络往返留给 `mod.rs` 的事件
/// 循环，那里已经在 `tokio::select!` 里，能正确处理并发与取消。
#[derive(Debug, Clone)]
pub(crate) enum Effect {
    /// 需要发送的一条请求。
    Send(Request),
}

/// 当前键盘输入应该路由到哪个界面元素。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Focus {
    /// 常规输入框。
    Composer,
    /// 审批弹窗（[`PendingState::front`] 返回 [`Front::Approval`]）。
    Approval,
    /// stdin 询问弹窗（[`PendingState::front`] 返回 [`Front::Stdin`]）。
    Stdin,
}

/// 单个内容块 index 允许增长到的上限。上游（provider）按顺序递增 index，正常
/// 情况下远达不到这个量级；这里只是防御性上限，防止解析异常/恶意帧让
/// `Vec::resize_with` 在客户端分配出一个巨大的稀疏数组（上游未给出该上限本身
/// 的依据，属于本仓自设的防御边界，不是抄某个参考实现的数值）。
const MAX_CONTENT_INDEX: usize = 4096;

/// 流式增量的语义类别，供 [`AppState::apply_content_delta`] 区分。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeltaKind {
    Text,
    Thinking,
    ToolCall,
}

/// TUI 应用的全部可变状态。
//
// 四个 bool 是四件互不相关的事（turn 是否在跑、退出确认是否武装、是否已请求退出、
// 是否展示思考），不是一组标志位。把它们塞进一个 `Flags` 结构体只会多一层
// `state.flags.turn_active`，读起来更差且没有任何不变量被这层封装保护。
#[expect(clippy::struct_excessive_bools, reason = "四个独立状态位，见上方注释")]
#[derive(Debug)]
pub(crate) struct AppState {
    session: SessionId,
    #[allow(dead_code)] // 握手要求携带，重连时会再次用到；当前尚未实现重连路径。
    client: ClientId,
    transcript: Vec<TranscriptEntry>,
    entry_index: HashMap<EntryId, usize>,
    tool_index: HashMap<CallId, usize>,
    streaming: HashMap<EntryId, RevealPacer>,
    pending: PendingState,
    input: InputState,
    status: StatusKind,
    status_revision: u64,
    animation_tick: u64,
    turn_active: bool,
    quit_armed: bool,
    should_quit: bool,
    last_entry: Option<EntryId>,
    /// 是否展示模型的思考内容（`config.ui.show_thinking`，默认 `false`）。
    ///
    /// headless 侧一直遵守这个开关，TUI 侧曾经完全无视它、无条件把 `思考: …` 画进
    /// transcript——同一个配置项在两个客户端行为不一致，是真机跑出来的缺陷。
    show_thinking: bool,
    /// 本会话的配色与符号。启动时构造一次、全程只读。
    theme: Theme,
}

impl AppState {
    /// 新建一个尚未订阅任何会话内容的状态：连接期间的第一帧就用它画
    /// （任务第 8 条，`plans/runtime-boundary/implementation.md:86-87`）。
    ///
    /// `theme` 由调用方在进入 raw mode 之前构造好传进来：色深探测读环境变量，
    /// 属于「启动时判定一次、全程只读」那一类（`plans/tui/README.md` 不变量 5），
    /// 不该埋在状态构造里每次重来。
    #[must_use]
    pub(crate) fn connecting(
        session: SessionId,
        client: ClientId,
        show_thinking: bool,
        theme: Theme,
    ) -> Self {
        Self {
            session,
            client,
            transcript: Vec::new(),
            entry_index: HashMap::new(),
            tool_index: HashMap::new(),
            streaming: HashMap::new(),
            pending: PendingState::default(),
            input: InputState::default(),
            status: StatusKind::Connecting,
            status_revision: 0,
            animation_tick: 0,
            turn_active: false,
            quit_armed: false,
            should_quit: false,
            last_entry: None,
            show_thinking,
            theme,
        }
    }

    /// 展示一条阻塞性提示（目前只用于连接阶段的 `SessionBusy` 询问）。
    /// 复用 [`StatusKind::Error`]：语义上都是"需要用户看到、且盖过常规状态"的
    /// 一行文本，不必为此单独加一个状态变体。
    pub(crate) fn set_notice(&mut self, message: String) {
        self.status = StatusKind::Error { message };
        self.bump_status();
    }

    /// 本状态订阅的会话 id，构造 `Request` 时用。
    #[must_use]
    pub(crate) fn session_id(&self) -> &SessionId {
        &self.session
    }

    /// 用 `Reply::Subscribed` 的内容重建 transcript 与待办队列。
    pub(crate) fn seed_subscribed(
        &mut self,
        entries: Vec<Entry>,
        pending: Pending,
        turn_active: bool,
    ) {
        self.merge_history(entries);
        self.pending.seed(pending.approvals, pending.stdin);
        self.turn_active = turn_active;
        self.status = if turn_active {
            StatusKind::Thinking
        } else {
            StatusKind::Idle
        };
        self.bump_status();
    }

    /// 合并一批历史条目（`Reply::Subscribed.entries` 与 `Reply::History.entries`
    /// 共用这一条路径）：已经在本地出现过的 entry id 会被跳过，保证幂等。
    pub(crate) fn merge_history(&mut self, entries: Vec<Entry>) {
        for entry in entries {
            self.ingest_history_entry(&entry);
        }
    }

    fn ingest_history_entry(&mut self, entry: &Entry) {
        if self.entry_index.contains_key(&entry.id) {
            return;
        }
        self.last_entry = Some(entry.id.clone());
        if let Some(block) = entry_to_block(entry, self.show_thinking) {
            self.push_finalized(entry.id.clone(), block);
        } else {
            // 不产生展示行（目前只有 `TitleChange`），但仍要占住这个 id：
            // 否则后续同一 id 的重复推送会被误判成"还没见过"。
            self.entry_index.insert(entry.id.clone(), usize::MAX);
        }
    }

    fn push_finalized(&mut self, entry_id: EntryId, block: Block) {
        if self.entry_index.contains_key(&entry_id) {
            return;
        }
        let idx = self.transcript.len();
        self.transcript.push(TranscriptEntry {
            id: ids::entry_component_id(entry_id.as_str()),
            revision: 1,
            block,
        });
        self.entry_index.insert(entry_id.clone(), idx);
        self.last_entry = Some(entry_id);
    }

    /// 应用一条 wire 事件；不属于本会话的事件（多会话共用一条 `ClientSession`
    /// 时会发生）直接忽略。返回需要额外发出的请求。
    // 对 19 个 `Event` 变体的**穷尽** match，没有 `_` 兜底：协议加了新事件而 UI 没处理时，
    // 必须是编译错误而不是运行期静默丢帧。每个分支都已经只剩一行委托，拆分只会把
    // 穷尽性护栏换成 fallthrough。
    #[expect(clippy::too_many_lines, reason = "穷尽 match 不可拆，见上方注释")]
    pub(crate) fn apply_event(&mut self, event: Event, now: Instant) -> Vec<Effect> {
        match event {
            Event::TurnStart {
                session,
                user_entry,
            } => self.on_turn_start(&session, &user_entry),
            Event::MessageStart { session, entry } => {
                self.on_message_start(&session, entry, now);
                vec![]
            }
            Event::TextDelta {
                session,
                entry,
                index,
                delta,
            } => {
                if session == self.session {
                    self.apply_content_delta(&entry, index, DeltaKind::Text, &delta, None);
                }
                vec![]
            }
            Event::ThinkingDelta {
                session,
                entry,
                index,
                delta,
            } => {
                // 关掉思考展示时连增量都不收：收了再在渲染层滤掉，`RevealPacer` 的
                // backlog 仍会把这些字符算进去，展示节奏会莫名其妙地卡顿。
                if session == self.session && self.show_thinking {
                    self.apply_content_delta(&entry, index, DeltaKind::Thinking, &delta, None);
                }
                vec![]
            }
            Event::ToolCallDelta {
                session,
                entry,
                index,
                call_id,
                delta,
            } => {
                if session == self.session {
                    self.apply_content_delta(
                        &entry,
                        index,
                        DeltaKind::ToolCall,
                        &delta,
                        Some(call_id),
                    );
                }
                vec![]
            }
            Event::MessageEnd {
                session,
                entry,
                message,
                ..
            } => {
                self.on_message_end(&session, entry, &message);
                vec![]
            }
            Event::ToolStart {
                session,
                call_id,
                name,
            } => {
                self.on_tool_start(&session, call_id, name);
                vec![]
            }
            Event::ToolProgress {
                session,
                call_id,
                progress,
            } => {
                if session == self.session {
                    self.apply_tool_progress(&call_id, progress);
                }
                vec![]
            }
            Event::ToolEnd {
                session,
                call_id,
                entry,
                is_error,
            } => {
                self.on_tool_end(&session, &call_id, entry, is_error);
                vec![]
            }
            Event::ApprovalRequested { session, pending } => {
                if session == self.session {
                    self.pending.push_approval(pending);
                }
                vec![]
            }
            Event::ApprovalResolved {
                session,
                request_id,
                ..
            } => {
                if session == self.session {
                    self.pending.remove_approval(&request_id);
                }
                vec![]
            }
            Event::StdinRequested { session, pending } => {
                if session == self.session {
                    self.pending.push_stdin(pending);
                }
                vec![]
            }
            Event::StdinResolved {
                session,
                request_id,
                ..
            } => {
                if session == self.session {
                    self.pending.remove_stdin(&request_id);
                }
                vec![]
            }
            Event::Compacted { session, .. } => self.on_compacted(&session),
            Event::SessionUpdated { session, .. } => {
                // 多客户端共享会话的标题/模型/head 同步不在本次验收范围内
                // （任务范围控制：不做会话切换 UI）。事件本身完全无害，
                // 忽略即可，协议契约允许静默跳过未消费的推送。
                let _ = session;
                vec![]
            }
            Event::TurnEnd { session } => {
                if session == self.session {
                    self.turn_active = false;
                    self.recompute_status();
                }
                vec![]
            }
            Event::Failed { session, message } => {
                if session == self.session {
                    self.turn_active = false;
                    self.status = StatusKind::Error { message };
                    self.bump_status();
                }
                vec![]
            }
            Event::Resync { session, dropped } => self.on_resync(&session, dropped),
            Event::Unknown => vec![],
        }
    }

    fn on_turn_start(&mut self, session: &SessionId, user_entry: &EntryId) -> Vec<Effect> {
        if *session != self.session {
            return vec![];
        }
        self.turn_active = true;
        self.status = StatusKind::Thinking;
        self.bump_status();
        if self.entry_index.contains_key(user_entry) {
            return vec![];
        }
        // 别的客户端替这个会话发的消息：本地没有内容可展示，与其编造
        // 用户没说过的文本，不如按游标补一次历史（`Request::HistoryFetch`
        // 与 `Event::Resync` 共用同一条恢复路径）。
        vec![Effect::Send(Request::HistoryFetch {
            session: self.session.clone(),
            since: self.last_entry.clone(),
        })]
    }

    fn on_message_start(&mut self, session: &SessionId, entry: EntryId, now: Instant) {
        if *session != self.session {
            return;
        }
        self.streaming.insert(entry.clone(), RevealPacer::new(now));
        self.push_finalized(
            entry,
            Block::Assistant {
                content: Vec::new(),
                streaming: true,
                revealed: Some(0),
            },
        );
    }

    fn on_message_end(&mut self, session: &SessionId, entry: EntryId, message: &Message) {
        if *session != self.session {
            return;
        }
        self.streaming.remove(&entry);
        let block = message_to_block(message, self.show_thinking);
        if let Some(&idx) = self.entry_index.get(&entry) {
            if let Some(slot) = self.transcript.get_mut(idx) {
                slot.block = block;
                slot.revision = slot.revision.saturating_add(1);
            }
        } else {
            self.push_finalized(entry, block);
        }
    }

    fn on_tool_start(&mut self, session: &SessionId, call_id: CallId, name: String) {
        if *session != self.session {
            return;
        }
        let idx = self.transcript.len();
        self.transcript.push(TranscriptEntry {
            id: ids::tool_component_id(call_id.as_str()),
            revision: 1,
            block: Block::Tool {
                call_id: call_id.clone(),
                name: name.clone(),
                output: String::new(),
                status: ToolStatus::Running,
            },
        });
        self.tool_index.insert(call_id, idx);
        self.status = StatusKind::RunningTool { name };
        self.bump_status();
    }

    fn on_tool_end(
        &mut self,
        session: &SessionId,
        call_id: &CallId,
        entry: EntryId,
        is_error: bool,
    ) {
        if *session != self.session {
            return;
        }
        if let Some(&idx) = self.tool_index.get(call_id) {
            if let Some(slot) = self.transcript.get_mut(idx) {
                if let Block::Tool { status, .. } = &mut slot.block {
                    *status = if is_error {
                        ToolStatus::Failed
                    } else {
                        ToolStatus::Done
                    };
                }
                slot.revision = slot.revision.saturating_add(1);
            }
            // 把这次工具结果最终落定的 entry id 也记进 entry_index：
            // 之后同一条历史（例如 Resync 触发的 HistoryFetch）再次
            // 出现这个 id 时会被去重，不会画出第二份重复的工具块。
            self.entry_index.insert(entry.clone(), idx);
        }
        self.last_entry = Some(entry);
        self.recompute_status();
    }

    fn on_compacted(&mut self, session: &SessionId) -> Vec<Effect> {
        if *session != self.session {
            return vec![];
        }
        // 压缩摘要文本不在事件里，走统一的历史补齐路径去拿权威内容，
        // 而不是先画一条占位再指望后续覆盖——那样会被 `entry_index` 的
        // 去重逻辑挡住，占位文本反而成了永久内容。
        vec![Effect::Send(Request::HistoryFetch {
            session: self.session.clone(),
            since: self.last_entry.clone(),
        })]
    }

    fn on_resync(&mut self, session: &SessionId, dropped: u64) -> Vec<Effect> {
        if *session != self.session {
            return vec![];
        }
        tracing::warn!(dropped, "事件流落后，按游标补拉历史与待办队列");
        vec![
            Effect::Send(Request::HistoryFetch {
                session: self.session.clone(),
                since: self.last_entry.clone(),
            }),
            // 第 6 条要求的解药：opencode 漏掉了重连/落后后的
            // `permission.list` 重拉，本仓在这里显式补上。
            Effect::Send(Request::PendingList {
                session: self.session.clone(),
            }),
        ]
    }

    fn apply_content_delta(
        &mut self,
        entry: &EntryId,
        index: u32,
        kind: DeltaKind,
        delta: &str,
        call_id: Option<CallId>,
    ) {
        let Some(&idx) = self.entry_index.get(entry) else {
            return;
        };
        let Some(slot) = self.transcript.get_mut(idx) else {
            return;
        };
        let Block::Assistant { content, .. } = &mut slot.block else {
            return;
        };
        let index = usize::try_from(index).unwrap_or(MAX_CONTENT_INDEX);
        if index >= MAX_CONTENT_INDEX {
            tracing::warn!(index, "助手内容块 index 超过防御上限，丢弃这条增量");
            return;
        }
        if index >= content.len() {
            content.resize_with(index.saturating_add(1), || AssistantContent::Text {
                text: String::new(),
            });
        }
        let Some(item) = content.get_mut(index) else {
            return;
        };
        match kind {
            DeltaKind::Text => match item {
                AssistantContent::Text { text } => text.push_str(delta),
                _ => {
                    *item = AssistantContent::Text {
                        text: delta.to_owned(),
                    }
                }
            },
            DeltaKind::Thinking => match item {
                AssistantContent::Thinking { text, .. } => text.push_str(delta),
                _ => {
                    *item = AssistantContent::Thinking {
                        text: delta.to_owned(),
                        signature: None,
                    };
                }
            },
            DeltaKind::ToolCall => match item {
                AssistantContent::ToolCall { id, arguments, .. } => {
                    if let Some(call_id) = call_id
                        && !call_id.as_str().is_empty()
                    {
                        *id = call_id;
                    }
                    arguments.push_str(delta);
                }
                _ => {
                    *item = AssistantContent::ToolCall {
                        id: call_id.unwrap_or_else(|| CallId::from("")),
                        name: String::new(),
                        arguments: delta.to_owned(),
                    };
                }
            },
        }
        slot.revision = slot.revision.saturating_add(1);
    }

    fn apply_tool_progress(&mut self, call_id: &CallId, progress: WireToolProgress) {
        let Some(&idx) = self.tool_index.get(call_id) else {
            return;
        };
        let Some(slot) = self.transcript.get_mut(idx) else {
            return;
        };
        let Block::Tool { output, .. } = &mut slot.block else {
            return;
        };
        match progress {
            WireToolProgress::Chunk { text } => output.push_str(&text),
            WireToolProgress::Status { text } => {
                if !output.is_empty() && !output.ends_with('\n') {
                    output.push('\n');
                }
                output.push_str("» ");
                output.push_str(&text);
            }
        }
        slot.revision = slot.revision.saturating_add(1);
    }

    fn bump_status(&mut self) {
        self.status_revision = self.status_revision.saturating_add(1);
    }

    /// turn 结束或工具收尾后重算状态行：仍有工具在跑就展示它，否则回到
    /// 思考中/空闲。
    fn recompute_status(&mut self) {
        if self.quit_armed {
            return;
        }
        let running_tool = self.transcript.iter().find_map(|entry| match &entry.block {
            Block::Tool {
                name,
                status: ToolStatus::Running,
                ..
            } => Some(name.clone()),
            _ => None,
        });
        self.status = match (self.turn_active, running_tool) {
            (_, Some(name)) => StatusKind::RunningTool { name },
            (true, None) => StatusKind::Thinking,
            (false, None) => StatusKind::Idle,
        };
        self.bump_status();
    }

    /// 每次重绘 tick 调用一次：推进 spinner 帧、推进所有流式内容的展示节奏。
    pub(crate) fn tick(&mut self, now: Instant) {
        self.animation_tick = self.animation_tick.saturating_add(1);
        self.bump_status();
        let streaming_entries: Vec<EntryId> = self.streaming.keys().cloned().collect();
        for entry in streaming_entries {
            let Some(&idx) = self.entry_index.get(&entry) else {
                continue;
            };
            let full_len = self
                .transcript
                .get(idx)
                .and_then(|slot| match &slot.block {
                    Block::Assistant { content, .. } => {
                        Some(crate::app::transcript::assistant_char_len(content))
                    }
                    _ => None,
                })
                .unwrap_or(0);
            let Some(slot) = self.transcript.get_mut(idx) else {
                continue;
            };
            let Block::Assistant { revealed, .. } = &mut slot.block else {
                continue;
            };
            let current = revealed.unwrap_or(0);
            let backlog = full_len.saturating_sub(current);
            let Some(pacer) = self.streaming.get_mut(&entry) else {
                continue;
            };
            let step = pacer.step(now, backlog);
            if step > 0 {
                *revealed = Some(current.saturating_add(step));
                slot.revision = slot.revision.saturating_add(1);
            }
        }
    }

    /// 是否处于需要 spinner 节奏重绘的状态。
    #[must_use]
    pub(crate) fn is_animating(&self) -> bool {
        status::is_animating(&self.status) || !self.streaming.is_empty()
    }

    /// 键盘输入此刻应该路由到哪个界面元素。
    #[must_use]
    pub(crate) fn focus(&self) -> Focus {
        match self.pending.front() {
            Some(Front::Approval(_)) => Focus::Approval,
            Some(Front::Stdin(_)) => Focus::Stdin,
            None => Focus::Composer,
        }
    }

    /// 输入框状态（可写）。
    pub(crate) fn input_mut(&mut self) -> &mut InputState {
        &mut self.input
    }

    /// 待办队列状态（可写，供按键处理里追加/编辑 stdin 缓冲）。
    pub(crate) fn pending_mut(&mut self) -> &mut PendingState {
        &mut self.pending
    }

    /// 取出输入框内容作为一次 `Request::Prompt` 的 payload；为空时返回
    /// `None`（不发送空 turn）。
    pub(crate) fn take_submission(&mut self) -> Option<Vec<UserContent>> {
        if self.input.is_empty() {
            return None;
        }
        Some(vec![UserContent::Text {
            text: self.input.take(),
        }])
    }

    /// `Reply::TurnStarted` 成功返回后调用：把刚发送的用户消息落进 transcript。
    pub(crate) fn on_turn_started(&mut self, user_entry: EntryId, content: Vec<UserContent>) {
        self.push_finalized(user_entry, Block::User { content });
        self.turn_active = true;
        self.status = StatusKind::Thinking;
        self.bump_status();
    }

    /// 针对队首审批构造一条 `Request::ApprovalRespond`；队首不是审批时返回
    /// `None`。
    #[must_use]
    pub(crate) fn respond_front_approval(&self, reply: ApprovalReply) -> Option<Request> {
        let Front::Approval(item) = self.pending.front()? else {
            return None;
        };
        Some(Request::ApprovalRespond {
            request_id: item.request_id.clone(),
            reply,
        })
    }

    /// 针对队首 stdin 询问构造一条 `Request::StdinRespond`，取当前编辑缓冲区的
    /// 内容；队首不是 stdin 询问时返回 `None`。
    #[must_use]
    pub(crate) fn submit_front_stdin(&self) -> Option<Request> {
        let Front::Stdin(item) = self.pending.front()? else {
            return None;
        };
        Some(Request::StdinRespond {
            request_id: item.request_id.clone(),
            text: self.pending.stdin_input().to_owned(),
        })
    }

    /// Esc：输入框非空先清空，返回 `true`；输入框已空则不处理（调用方据此决定
    /// 是否转而发取消请求）。
    pub(crate) fn clear_input_if_any(&mut self) -> bool {
        if self.input.is_empty() {
            return false;
        }
        self.input.clear();
        true
    }

    /// 是否有 turn 在跑，Esc 清空输入后据此决定要不要接着发 `Cancel`。
    #[must_use]
    pub(crate) fn turn_active(&self) -> bool {
        self.turn_active
    }

    /// 构造取消当前 turn 的请求。
    #[must_use]
    pub(crate) fn cancel_request(&self) -> Request {
        Request::Cancel {
            session: self.session.clone(),
        }
    }

    /// 记录一次 Ctrl-C：置状态为"再按一次退出"确认态。
    pub(crate) fn arm_quit_confirmation(&mut self) {
        self.quit_armed = true;
        self.status = StatusKind::ConfirmQuit;
        self.bump_status();
    }

    /// 是否已经处于"等待第二次 Ctrl-C"确认态。
    #[must_use]
    pub(crate) fn quit_armed(&self) -> bool {
        self.quit_armed
    }

    /// 除 Ctrl-C 外的任意按键都应解除确认态（避免"按了一次 Ctrl-C 又继续打字，
    /// 几分钟后误触第二次 Ctrl-C 直接退出"）。
    pub(crate) fn disarm_quit_confirmation(&mut self) {
        if self.quit_armed {
            self.quit_armed = false;
            self.recompute_status();
        }
    }

    /// 标记应用应当退出（第二次 Ctrl-C）。
    pub(crate) fn request_quit(&mut self) {
        self.should_quit = true;
    }

    /// 事件循环是否应该结束。
    #[must_use]
    pub(crate) fn should_quit(&self) -> bool {
        self.should_quit
    }

    /// 活跃区的高度**不在这里算**。
    ///
    /// 它由 [`zcode_tui::Emitter::render`] 从 `compose` 的 boundary 直接得出。本层曾经
    /// 自己数过一遍（状态行 + 弹窗 + 输入框 + 仍在直播的块），那是个错误设计：
    ///
    /// - 要数出行数就得把这些组件各渲染一遍，而它们随后在 `build_components` 里
    ///   还会被渲染一次——助手正文带代码块时，一次 syntect 高亮实测 8-40 ms
    ///   （`cargo run -p zcode-tui --release --example render_cost`），双倍成本直接吃穿
    ///   30 fps 的帧预算；
    /// - 更要命的是它**必然与 `compose` 的实际结果漂移**。漂移一旦发生，viewport 装不下
    ///   活跃内容，顶部几行既没进历史也没画进窗口，表现是消息凭空消失，且不会自愈。
    ///
    /// 这个函数保留成一个常量下限，只为让 viewport 至少有一行可画。
    #[must_use]
    pub(crate) const fn min_viewport_height() -> u16 {
        1
    }

    /// 组装本帧要交给 `Emitter::render` 的组件列表。装箱是为了让四种不同的
    /// 具体组件类型能放进同一个 `Vec`，代价是每帧几次堆分配，相对于整帧渲染
    /// 可忽略不计。
    #[must_use]
    pub(crate) fn build_components(&self) -> Vec<Box<dyn Component + '_>> {
        let mut items: Vec<Box<dyn Component + '_>> =
            Vec::with_capacity(self.transcript.len().saturating_add(3));
        for entry in &self.transcript {
            items.push(Box::new(BlockComponent::new(entry, &self.theme)));
        }
        items.push(Box::new(StatusComponent::new(
            self.status.clone(),
            self.animation_tick,
            self.status_revision,
            &self.theme,
        )));
        if let Some(pending) = PendingComponent::new(&self.pending, &self.theme) {
            items.push(Box::new(pending));
        }
        let composer_focused = matches!(self.focus(), Focus::Composer);
        items.push(Box::new(InputComponent::new(
            &self.input,
            composer_focused,
            &self.theme,
        )));
        items
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> AppState {
        AppState::connecting(
            SessionId::from("s1"),
            ClientId::from("c1"),
            false,
            crate::app::test_theme(),
        )
    }

    #[test]
    fn text_delta_before_message_start_is_ignored_not_panicking() {
        let mut s = state();
        let effects = s.apply_event(
            Event::TextDelta {
                session: SessionId::from("s1"),
                entry: EntryId::from("e1"),
                index: 0,
                delta: "hi".to_owned(),
            },
            Instant::now(),
        );
        assert!(effects.is_empty());
    }

    #[test]
    fn message_lifecycle_produces_one_finalized_block() {
        let mut s = state();
        let now = Instant::now();
        s.apply_event(
            Event::MessageStart {
                session: SessionId::from("s1"),
                entry: EntryId::from("e1"),
            },
            now,
        );
        s.apply_event(
            Event::TextDelta {
                session: SessionId::from("s1"),
                entry: EntryId::from("e1"),
                index: 0,
                delta: "hello".to_owned(),
            },
            now,
        );
        s.apply_event(
            Event::MessageEnd {
                session: SessionId::from("s1"),
                entry: EntryId::from("e1"),
                message: Box::new(Message::Assistant {
                    content: vec![AssistantContent::Text {
                        text: "hello".to_owned(),
                    }],
                    model: None,
                    usage: zcode_protocol::wire::types::Usage::default(),
                    stop_reason: zcode_protocol::wire::types::StopReason::default(),
                }),
                usage: zcode_protocol::wire::types::Usage::default(),
            },
            now,
        );
        assert_eq!(s.transcript.len(), 1);
        assert!(!s.streaming.contains_key(&EntryId::from("e1")));
        assert!(matches!(
            s.transcript.first().map(|e| &e.block),
            Some(Block::Assistant {
                streaming: false,
                ..
            })
        ));
    }

    #[test]
    fn events_for_other_sessions_are_ignored() {
        let mut s = state();
        let effects = s.apply_event(
            Event::TurnStart {
                session: SessionId::from("other"),
                user_entry: EntryId::from("e1"),
            },
            Instant::now(),
        );
        assert!(effects.is_empty());
        assert!(!s.turn_active());
    }

    #[test]
    fn resync_requests_history_fetch_and_pending_list() {
        let mut s = state();
        let effects = s.apply_event(
            Event::Resync {
                session: SessionId::from("s1"),
                dropped: 3,
            },
            Instant::now(),
        );
        assert_eq!(effects.len(), 2);
        assert!(matches!(
            effects.first(),
            Some(Effect::Send(Request::HistoryFetch { .. }))
        ));
        assert!(matches!(
            effects.get(1),
            Some(Effect::Send(Request::PendingList { .. }))
        ));
    }

    #[test]
    fn tool_lifecycle_updates_status_and_transcript() {
        let mut s = state();
        s.apply_event(
            Event::ToolStart {
                session: SessionId::from("s1"),
                call_id: CallId::from("call_1"),
                name: "bash".to_owned(),
            },
            Instant::now(),
        );
        assert!(matches!(s.status, StatusKind::RunningTool { .. }));
        s.apply_event(
            Event::ToolProgress {
                session: SessionId::from("s1"),
                call_id: CallId::from("call_1"),
                progress: WireToolProgress::Chunk {
                    text: "output".to_owned(),
                },
            },
            Instant::now(),
        );
        s.apply_event(
            Event::ToolEnd {
                session: SessionId::from("s1"),
                call_id: CallId::from("call_1"),
                entry: EntryId::from("tool-entry"),
                is_error: false,
            },
            Instant::now(),
        );
        let Some(Block::Tool { output, status, .. }) = s.transcript.first().map(|e| &e.block)
        else {
            panic!("期望恰好一个工具块");
        };
        assert_eq!(output, "output");
        assert_eq!(*status, ToolStatus::Done);
    }

    #[test]
    fn clear_input_if_any_reports_whether_it_cleared() {
        let mut s = state();
        assert!(!s.clear_input_if_any());
        s.input_mut().insert("hi");
        assert!(s.clear_input_if_any());
        assert!(s.input.is_empty());
    }

    #[test]
    fn quit_confirmation_arms_and_disarms() {
        let mut s = state();
        assert!(!s.quit_armed());
        s.arm_quit_confirmation();
        assert!(s.quit_armed());
        s.disarm_quit_confirmation();
        assert!(!s.quit_armed());
    }

    #[test]
    fn duplicate_history_entries_are_not_double_ingested() {
        let mut s = state();
        let entry = Entry {
            id: EntryId::from("e1"),
            parent_id: None,
            timestamp_ms: 0,
            kind: zcode_protocol::wire::types::EntryKind::Message {
                message: Message::User {
                    content: vec![UserContent::Text {
                        text: "hi".to_owned(),
                    }],
                    display_role: None,
                },
            },
        };
        s.merge_history(vec![entry.clone(), entry]);
        assert_eq!(s.transcript.len(), 1);
    }
}
