//! 事件 → 输出的核心翻译层：从 wire [`Event`] 流产出终端可见的输出，返回退出码。
//!
//! 抽成独立函数（[`render_events`]）是为了可测：输出 sink 是 `impl AsyncWrite`，
//! 事件源是普通 `mpsc::UnboundedReceiver`，审批/stdin/取消回环经 [`Responder`]
//! trait 注入——测试不需要真的起 host，喂 `Vec<u8>` 与假 `Responder` 就够
//! （`rule://rust-testing` 的 mock 策略：trait 抽象 + 依赖注入，不做全局替换）。
//! [`crate::render::run_headless`] 只负责把 [`ClientSession`] 接上这套接口。

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::io;

use async_trait::async_trait;
use serde::Serialize;
use tokio::io::{AsyncWrite, AsyncWriteExt as _};
use tokio::sync::mpsc;
use zcode_protocol::wire::request::Pending;
use zcode_protocol::wire::types::{
    ApprovalId, ApprovalReply, AssistantContent, CallId, Entry, EntryId, EntryKind, Message,
    PendingApproval, PendingStdin, SessionId, SessionSummary, StdinId, ToolProgress,
};
use zcode_protocol::{Event, Reply, Request};

use crate::host::connect::ClientSession;
use crate::render::cleanup::clean_line;
use crate::render::interactive::{is_interactive, read_terminal_line};
use crate::render::{OutputFormat, RenderError};

/// 审批 / stdin 回环与本地取消，全部经这个 trait 间接调用——核心渲染循环因此
/// 不必认识 [`ClientSession`]，测试可以喂一个记录调用的假实现。
#[async_trait]
pub(super) trait Responder: Send + Sync {
    /// 回答一条审批询问。
    async fn respond_approval(&self, request_id: ApprovalId, reply: ApprovalReply);
    /// 回答一条 stdin 询问。
    async fn respond_stdin(&self, request_id: StdinId, text: String);
    /// 取消当前 turn（用户主动取消，或非交互环境遇到必须有人应答的询问）。
    async fn cancel_turn(&self);
    /// 按游标补拉条目，用于 [`Event::Resync`] 之后的追赶。失败时返回空列表——
    /// 调用方把它当"这次没追上，继续消费实时事件"处理，不是致命错误。
    async fn fetch_history(&self, since: Option<EntryId>) -> Vec<Entry>;
}

/// [`Responder`] 的生产实现：直接经 [`ClientSession::request`] 打三种请求。
pub(super) struct SessionResponder<'a> {
    /// 已完成握手的连接。
    pub(super) session: &'a ClientSession,
    /// 本次 headless 运行绑定的会话。
    pub(super) target: SessionId,
}

#[async_trait]
impl Responder for SessionResponder<'_> {
    async fn respond_approval(&self, request_id: ApprovalId, reply: ApprovalReply) {
        if let Err(error) = self
            .session
            .request(Request::ApprovalRespond { request_id, reply })
            .await
        {
            // 审批回执发不出去不该拖垮整轮渲染：运行时那边超时后有自己的兜底
            // 策略，这里只需要留下诊断痕迹。
            tracing::warn!(%error, "发送审批回执失败");
        }
    }

    async fn respond_stdin(&self, request_id: StdinId, text: String) {
        if let Err(error) = self
            .session
            .request(Request::StdinRespond { request_id, text })
            .await
        {
            tracing::warn!(%error, "发送 stdin 回执失败");
        }
    }

    async fn cancel_turn(&self) {
        if let Err(error) = self
            .session
            .request(Request::Cancel {
                session: self.target.clone(),
            })
            .await
        {
            tracing::warn!(%error, "发送取消请求失败");
        }
    }

    async fn fetch_history(&self, since: Option<EntryId>) -> Vec<Entry> {
        match self
            .session
            .request(Request::HistoryFetch {
                session: self.target.clone(),
                since,
            })
            .await
        {
            Ok(Reply::History { entries }) => entries,
            Ok(_) => Vec::new(),
            Err(error) => {
                tracing::warn!(%error, "补拉历史失败，继续消费实时事件");
                Vec::new()
            }
        }
    }
}

/// JSON 模式首行输出的会话 header。
#[derive(Debug, Serialize)]
pub(super) struct SessionHeader<'a> {
    /// 固定 `"session"`，供下游按 `type` 字段区分这一行不是普通事件。
    #[serde(rename = "type")]
    pub(super) kind: &'static str,
    /// 会话摘要。
    #[serde(flatten)]
    pub(super) summary: &'a SessionSummary,
}

/// 事件循环的可变状态。
#[derive(Default)]
struct RenderState {
    /// 已确认追加进条目树、可以安全当作"游标"的最新条目 id；resync 补拉时用它
    /// 当 `since`。
    known_head: Option<EntryId>,
    /// 已经收到 `MessageStart` 但还没收到 `MessageEnd` 的条目——resync 补拉时
    /// 跳过它们，避免和已经流出去的部分文本重复。
    in_flight_entries: HashSet<EntryId>,
    /// 调用 id → 工具名，供 stderr 摘要行用（`ToolProgress`/`ToolEnd` 事件本身
    /// 不带工具名）。
    tool_names: HashMap<CallId, String>,
    /// 是否本地发起过取消（`Ctrl-C`，或非交互环境的 stdin 兜底）——决定
    /// `TurnEnd` 到达时退出码是 `0` 还是 `130`。
    cancelled_locally: bool,
}

/// 事件源到输出的核心翻译；`run_headless` 只是把 [`ClientSession`] 接上它。
///
/// - `target`：只处理属于这个会话的事件，其余静默跳过（防御性检查：本连接目前
///   只订阅了一个会话，理论上不会收到别的，但不能假设运行时永远这样实现）。
/// - `header`：JSON 模式下作为首行输出的会话摘要。
/// - `pending`：`Reply::Subscribed` 带回的既有待回答项，必须当场处理——它们是
///   订阅之前就存在的询问，不会再作为新事件出现在流里。
/// - `cancel`：外部取消信号。生产路径接 `tokio::signal::ctrl_c()`；测试传
///   `std::future::ready(())` 立即触发，或 `std::future::pending()` 永不触发。
#[allow(
    clippy::too_many_arguments,
    reason = "每个参数控制测试里独立的一路输入/输出；拆成结构体只是把同一组参数换个地方摆，不减少信息量"
)]
pub(super) async fn render_events<Out, ErrOut, Cancel>(
    mut events: mpsc::UnboundedReceiver<Event>,
    responder: &dyn Responder,
    target: &SessionId,
    format: OutputFormat,
    show_thinking: bool,
    header: SessionSummary,
    pending: Pending,
    mut cancel: Cancel,
    mut stdout: Out,
    mut stderr: ErrOut,
) -> Result<i32, RenderError>
where
    Out: AsyncWrite + Unpin,
    ErrOut: AsyncWrite + Unpin,
    Cancel: Future<Output = ()> + Unpin,
{
    if matches!(format, OutputFormat::Json) {
        let line = ndjson_line(&SessionHeader {
            kind: "session",
            summary: &header,
        });
        if !write_line(&mut stdout, &line).await? {
            return Ok(0);
        }
    }

    let mut state = RenderState::default();

    for approval in &pending.approvals {
        if !handle_approval(&mut stderr, responder, approval).await? {
            return Ok(0);
        }
    }
    for stdin_pending in &pending.stdin {
        if !handle_stdin(
            &mut stderr,
            responder,
            stdin_pending,
            &mut state.cancelled_locally,
        )
        .await?
        {
            return Ok(0);
        }
    }

    let mut cancel_armed = true;
    loop {
        tokio::select! {
            biased;
            () = &mut cancel, if cancel_armed => {
                cancel_armed = false;
                state.cancelled_locally = true;
                if !write_line(&mut stderr, "已收到取消信号，正在中止当前 turn…").await? {
                    return Ok(0);
                }
                responder.cancel_turn().await;
            }
            received = events.recv() => {
                let Some(event) = received else {
                    // 事件通道关闭却没见到 TurnEnd/Failed：连接异常掉线，不是
                    // 我们主动结束的，按失败处理（除非正是我们自己触发的取消
                    // 导致连接跟着收尾）。
                    return Ok(if state.cancelled_locally { 130 } else { 1 });
                };
                let Some(event_session) = session_of(&event) else { continue };
                if event_session != target {
                    continue;
                }
                if matches!(format, OutputFormat::Json) {
                    let line = ndjson_line(&event);
                    if !write_line(&mut stdout, &line).await? {
                        return Ok(0);
                    }
                }
                if let Some(code) = handle_event(
                    event, responder, format, show_thinking, &mut state, &mut stdout, &mut stderr,
                ).await? {
                    return Ok(code);
                }
            }
        }
    }
}

/// 事件所属会话；`Unknown`（对端比本端新）没有归属，调用方跳过它。
fn session_of(event: &Event) -> Option<&SessionId> {
    match event {
        Event::TurnStart { session, .. }
        | Event::MessageStart { session, .. }
        | Event::TextDelta { session, .. }
        | Event::ThinkingDelta { session, .. }
        | Event::ToolCallDelta { session, .. }
        | Event::MessageEnd { session, .. }
        | Event::ToolStart { session, .. }
        | Event::ToolProgress { session, .. }
        | Event::ToolEnd { session, .. }
        | Event::ApprovalRequested { session, .. }
        | Event::ApprovalResolved { session, .. }
        | Event::StdinRequested { session, .. }
        | Event::StdinResolved { session, .. }
        | Event::Compacted { session, .. }
        | Event::SessionUpdated { session, .. }
        | Event::TurnEnd { session }
        | Event::Failed { session, .. }
        | Event::Resync { session, .. } => Some(session),
        Event::Unknown => None,
    }
}

/// 单个事件的副作用：写输出、更新状态、必要时决定退出码。
///
/// 返回 `Ok(Some(code))` 表示循环应当结束并以 `code` 退出；`Ok(None)` 表示继续
/// 消费下一个事件。
async fn handle_event<Out, ErrOut>(
    event: Event,
    responder: &dyn Responder,
    format: OutputFormat,
    show_thinking: bool,
    state: &mut RenderState,
    stdout: &mut Out,
    stderr: &mut ErrOut,
) -> Result<Option<i32>, RenderError>
where
    Out: AsyncWrite + Unpin,
    ErrOut: AsyncWrite + Unpin,
{
    match event {
        Event::TurnStart { .. }
        | Event::ToolCallDelta { .. }
        | Event::SessionUpdated { .. }
        | Event::Unknown => Ok(None),
        Event::MessageStart { entry, .. } => {
            state.in_flight_entries.insert(entry);
            Ok(None)
        }
        Event::TextDelta { delta, .. } => {
            if matches!(format, OutputFormat::Text) {
                Ok(stop_unless(write_raw(stdout, &delta).await?))
            } else {
                Ok(None)
            }
        }
        Event::ThinkingDelta { delta, .. } => {
            if show_thinking && matches!(format, OutputFormat::Text) {
                Ok(stop_unless(write_raw(stdout, &delta).await?))
            } else {
                Ok(None)
            }
        }
        Event::MessageEnd { entry, .. } => {
            state.in_flight_entries.remove(&entry);
            advance_head(state, entry);
            Ok(None)
        }
        Event::ToolStart { call_id, name, .. } => {
            state.tool_names.insert(call_id, name.clone());
            note(stderr, &format!("→ {name}")).await
        }
        Event::ToolProgress {
            call_id, progress, ..
        } => {
            let name = tool_label(state, &call_id);
            let text = match progress {
                ToolProgress::Chunk { text } | ToolProgress::Status { text } => text,
            };
            note(stderr, &format!("  {name}: {text}")).await
        }
        Event::ToolEnd {
            call_id, is_error, ..
        } => {
            let name = tool_label(state, &call_id);
            let mark = if is_error { "✗" } else { "✓" };
            note(stderr, &format!("{mark} {name}")).await
        }
        Event::ApprovalRequested { pending, .. } => Ok(stop_unless(
            handle_approval(stderr, responder, &pending).await?,
        )),
        Event::ApprovalResolved {
            request_id,
            approved,
            ..
        } => {
            let verdict = if approved { "已放行" } else { "已拒绝" };
            note(stderr, &format!("[审批] {request_id} {verdict}")).await
        }
        Event::StdinRequested { pending, .. } => Ok(stop_unless(
            handle_stdin(stderr, responder, &pending, &mut state.cancelled_locally).await?,
        )),
        Event::StdinResolved {
            request_id,
            submitted,
            ..
        } => {
            let verdict = if submitted { "已提交" } else { "已取消" };
            note(stderr, &format!("[stdin] {request_id} {verdict}")).await
        }
        Event::Compacted { entry, .. } => {
            advance_head(state, entry);
            note(stderr, "[压缩] 上下文已压缩").await
        }
        Event::TurnEnd { .. } => Ok(Some(if state.cancelled_locally { 130 } else { 0 })),
        Event::Failed { message, .. } => match note(stderr, &message).await? {
            // broken pipe：note() 已经决定立即以退出码 0 收尾，覆盖掉本来该是
            // 1 的失败退出码——用户既然连输出都不读了，报不报错没有意义。
            Some(code) => Ok(Some(code)),
            None => Ok(Some(1)),
        },
        Event::Resync { dropped, .. } => {
            let text = format!("[resync] 客户端落后 {dropped} 条事件，正在按游标补拉历史");
            if note(stderr, &text).await?.is_some() {
                return Ok(Some(0));
            }
            let entries = responder.fetch_history(state.known_head.clone()).await;
            Ok(stop_unless(
                backfill(&entries, state, format, show_thinking, stdout).await?,
            ))
        }
    }
}

/// 写一行 stderr 提示，经 [`clean_line`] 清洗；broken pipe 时返回
/// `Ok(Some(0))`（调用方应当立即以退出码 0 结束渲染），否则 `Ok(None)`
/// （继续处理下一个事件）。把"写 stderr 后检查 broken pipe"这三行样板收进
/// 一次调用，是 [`handle_event`] 从 19 个事件分支里拿掉重复代码的主要手段。
async fn note<ErrOut: AsyncWrite + Unpin>(
    stderr: &mut ErrOut,
    text: &str,
) -> Result<Option<i32>, RenderError> {
    if write_line(stderr, &clean_line(text)).await? {
        Ok(None)
    } else {
        Ok(Some(0))
    }
}

/// 把写入辅助函数"是否应该继续"的返回值（`false` = 撞见 broken pipe）转成
/// [`handle_event`] 的返回约定：`None` 继续，`Some(0)` 立即以退出码 0 收尾。
fn stop_unless(ok: bool) -> Option<i32> {
    (!ok).then_some(0)
}

/// 把 `entry` 记成目前已知最新的、已确认落盘的条目（取二者较大值，条目 id
/// 字典序即时间序）。
fn advance_head(state: &mut RenderState, entry: EntryId) {
    state.known_head = Some(match state.known_head.take() {
        Some(previous) if previous >= entry => previous,
        _ => entry,
    });
}

/// 工具展示名；查不到时退回一个通用占位（理论上不会发生——`ToolStart` 总是先于
/// `ToolProgress`/`ToolEnd` 到达，除非客户端是从 resync 中途接上的）。
fn tool_label<'a>(state: &'a RenderState, call_id: &CallId) -> &'a str {
    state.tool_names.get(call_id).map_or("工具", String::as_str)
}

/// resync 之后的尽力回填：只处理我们完全没见过起点的助手消息条目——已经流出去
/// 一部分文本的条目（[`RenderState::in_flight_entries`]）跳过，避免和已经写出
/// 的前缀重复。这是"不要断流"的最低要求，不追求逐字节精确重建。
async fn backfill<Out: AsyncWrite + Unpin>(
    entries: &[Entry],
    state: &mut RenderState,
    format: OutputFormat,
    show_thinking: bool,
    stdout: &mut Out,
) -> Result<bool, RenderError> {
    for entry in entries {
        if state.in_flight_entries.contains(&entry.id) {
            continue;
        }
        advance_head(state, entry.id.clone());

        let EntryKind::Message {
            message: Message::Assistant { content, .. },
        } = &entry.kind
        else {
            continue;
        };

        if matches!(format, OutputFormat::Text) {
            for block in content {
                let text = match block {
                    AssistantContent::Text { text } => Some(text.as_str()),
                    AssistantContent::Thinking { text, .. } if show_thinking => Some(text.as_str()),
                    _ => None,
                };
                if let Some(text) = text
                    && !write_raw(stdout, text).await?
                {
                    return Ok(false);
                }
            }
        } else {
            let line = serde_json::json!({ "type": "resync_backfill", "entry": entry }).to_string();
            if !write_line(stdout, &line).await? {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

/// 处理一条待审批项：交互式终端读用户输入，非交互环境自动拒绝并在 stderr
/// 说明原因。返回 `false` 表示写入时遇到了 broken pipe，调用方应当立即收尾。
pub(super) async fn handle_approval<ErrOut: AsyncWrite + Unpin>(
    stderr: &mut ErrOut,
    responder: &dyn Responder,
    approval: &PendingApproval,
) -> Result<bool, RenderError> {
    let reply = if is_interactive() {
        let prompt = format!(
            "[审批] {} 请求 {}（调用 {}）：{}\n  允许一次(y) / 始终允许(a) / 拒绝(其他): ",
            approval.tool_name, approval.scope, approval.call_id, approval.prompt
        );
        if !write_line(stderr, &clean_line(&prompt)).await? {
            return Ok(false);
        }
        match read_terminal_line(false).await {
            Ok(line) => match line.trim() {
                "y" | "Y" => ApprovalReply::Once,
                "a" | "A" => ApprovalReply::Always,
                _ => ApprovalReply::Reject,
            },
            // 读失败（例如 stdin 意外关闭）按拒绝处理：宁可少放行，不可误放行。
            Err(_) => ApprovalReply::Reject,
        }
    } else {
        let prompt = format!(
            "[审批] {} 请求 {}：{}；非交互环境（stdin/stderr 不是终端），自动拒绝。",
            approval.tool_name, approval.scope, approval.prompt
        );
        if !write_line(stderr, &clean_line(&prompt)).await? {
            return Ok(false);
        }
        ApprovalReply::Reject
    };
    responder
        .respond_approval(approval.request_id.clone(), reply)
        .await;
    Ok(true)
}

/// 处理一条待输入项：交互式终端读一行（`is_password` 时不回显）；非交互环境
/// 无法安全提供答案——协议里 [`Request::StdinRespond`] 没有"拒绝"这个选项，
/// 唯一不挂起又不编造答案的选择是取消整个 turn（与审批 `Reject` 的"绝不放行"
/// 精神一致）。返回 `false` 表示写入时遇到了 broken pipe。
pub(super) async fn handle_stdin<ErrOut: AsyncWrite + Unpin>(
    stderr: &mut ErrOut,
    responder: &dyn Responder,
    pending: &PendingStdin,
    cancelled_locally: &mut bool,
) -> Result<bool, RenderError> {
    if is_interactive() {
        let label = if pending.prompt.is_empty() {
            "(工具未打印提示)"
        } else {
            pending.prompt.as_str()
        };
        if !write_line(stderr, &clean_line(&format!("[stdin] {label}"))).await? {
            return Ok(false);
        }
        let text = read_terminal_line(pending.is_password)
            .await
            .unwrap_or_default();
        responder
            .respond_stdin(pending.request_id.clone(), text)
            .await;
    } else {
        let note =
            "[stdin] 工具请求终端输入，但当前不是交互式终端；无法安全提供答案，已取消本次 turn。";
        if !write_line(stderr, &clean_line(note)).await? {
            return Ok(false);
        }
        *cancelled_locally = true;
        responder.cancel_turn().await;
    }
    Ok(true)
}

/// 序列化一行 NDJSON；序列化失败（理论上不会发生，所有 wire 类型都是
/// `#[derive(Serialize)]` 的纯数据结构）时退回空对象而不是让整轮渲染崩掉。
fn ndjson_line<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "{}".to_owned())
}

/// 写一行到 sink：末尾补换行并立即 flush。遇到 `ErrorKind::BrokenPipe` 时不算
/// 错误——消费端已经关闭（例如 `zcode run ... | head -1`），返回 `false` 告诉
/// 调用方停止后续输出；其余 IO 错误原样上抛。
pub(super) async fn write_line<W: AsyncWrite + Unpin>(
    sink: &mut W,
    text: &str,
) -> Result<bool, RenderError> {
    if !write_bytes(sink, text.as_bytes()).await? {
        return Ok(false);
    }
    if !write_bytes(sink, b"\n").await? {
        return Ok(false);
    }
    flush(sink).await
}

/// 写一段原始文本，不补换行——用于模型文本增量的边到边透传。
pub(super) async fn write_raw<W: AsyncWrite + Unpin>(
    sink: &mut W,
    text: &str,
) -> Result<bool, RenderError> {
    if !write_bytes(sink, text.as_bytes()).await? {
        return Ok(false);
    }
    flush(sink).await
}

async fn write_bytes<W: AsyncWrite + Unpin>(
    sink: &mut W,
    bytes: &[u8],
) -> Result<bool, RenderError> {
    match sink.write_all(bytes).await {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Ok(false),
        Err(error) => Err(RenderError::Io(error)),
    }
}

async fn flush<W: AsyncWrite + Unpin>(sink: &mut W) -> Result<bool, RenderError> {
    match sink.flush().await {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Ok(false),
        Err(error) => Err(RenderError::Io(error)),
    }
}

#[cfg(test)]
mod tests {
    use std::pin::Pin;
    use std::sync::Mutex;
    use std::task::{Context, Poll};

    use super::*;

    #[derive(Default)]
    struct FakeResponder {
        approvals: Mutex<Vec<(ApprovalId, ApprovalReply)>>,
        stdins: Mutex<Vec<(StdinId, String)>>,
        cancels: Mutex<u32>,
        history: Mutex<Vec<Entry>>,
    }

    #[async_trait]
    impl Responder for FakeResponder {
        async fn respond_approval(&self, request_id: ApprovalId, reply: ApprovalReply) {
            self.approvals
                .lock()
                .expect("测试内锁未中毒")
                .push((request_id, reply));
        }

        async fn respond_stdin(&self, request_id: StdinId, text: String) {
            self.stdins
                .lock()
                .expect("测试内锁未中毒")
                .push((request_id, text));
        }

        async fn cancel_turn(&self) {
            *self.cancels.lock().expect("测试内锁未中毒") += 1;
        }

        async fn fetch_history(&self, _since: Option<EntryId>) -> Vec<Entry> {
            self.history.lock().expect("测试内锁未中毒").clone()
        }
    }

    /// 写什么都返回 `BrokenPipe`，用来验证「不得把整轮弄挂」这条约束。
    struct BrokenPipeSink;

    impl AsyncWrite for BrokenPipeSink {
        fn poll_write(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            _buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            Poll::Ready(Err(io::Error::from(io::ErrorKind::BrokenPipe)))
        }

        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }

        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    fn target_session() -> SessionId {
        SessionId::from("ses_test")
    }

    fn header() -> SessionSummary {
        SessionSummary {
            id: target_session(),
            title: None,
            cwd: "/workspace".to_owned(),
            model: "test-model".to_owned(),
            created_ms: 0,
            updated_ms: 0,
        }
    }

    #[tokio::test]
    async fn text_mode_stdout_carries_only_model_text() {
        let (tx, rx) = mpsc::unbounded_channel();
        let session = target_session();
        tx.send(Event::ToolStart {
            session: session.clone(),
            call_id: CallId::from("call_1"),
            name: "bash".to_owned(),
        })
        .expect("发送不应失败");
        tx.send(Event::TextDelta {
            session: session.clone(),
            entry: EntryId::from("ent_1"),
            index: 0,
            delta: "你好".to_owned(),
        })
        .expect("发送不应失败");
        tx.send(Event::TextDelta {
            session: session.clone(),
            entry: EntryId::from("ent_1"),
            index: 0,
            delta: "，世界".to_owned(),
        })
        .expect("发送不应失败");
        tx.send(Event::ToolEnd {
            session: session.clone(),
            call_id: CallId::from("call_1"),
            entry: EntryId::from("ent_2"),
            is_error: false,
        })
        .expect("发送不应失败");
        tx.send(Event::TurnEnd {
            session: session.clone(),
        })
        .expect("发送不应失败");
        drop(tx);

        let responder = FakeResponder::default();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = render_events(
            rx,
            &responder,
            &session,
            OutputFormat::Text,
            false,
            header(),
            Pending::default(),
            std::future::pending::<()>(),
            &mut stdout,
            &mut stderr,
        )
        .await
        .expect("渲染不应失败");

        assert_eq!(code, 0);
        assert_eq!(
            String::from_utf8(stdout).expect("stdout 应为合法 utf8"),
            "你好，世界"
        );
        let stderr_text = String::from_utf8(stderr).expect("stderr 应为合法 utf8");
        assert!(stderr_text.contains("bash"), "工具摘要应当出现在 stderr");
    }

    #[tokio::test]
    async fn json_mode_emits_ndjson_with_header_first() {
        let (tx, rx) = mpsc::unbounded_channel();
        let session = target_session();
        tx.send(Event::TextDelta {
            session: session.clone(),
            entry: EntryId::from("ent_1"),
            index: 0,
            delta: "hi".to_owned(),
        })
        .expect("发送不应失败");
        tx.send(Event::TurnEnd {
            session: session.clone(),
        })
        .expect("发送不应失败");
        drop(tx);

        let responder = FakeResponder::default();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = render_events(
            rx,
            &responder,
            &session,
            OutputFormat::Json,
            false,
            header(),
            Pending::default(),
            std::future::pending::<()>(),
            &mut stdout,
            &mut stderr,
        )
        .await
        .expect("渲染不应失败");

        assert_eq!(code, 0);
        let text = String::from_utf8(stdout).expect("stdout 应为合法 utf8");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3, "header + text_delta + turn_end");

        let header_value: serde_json::Value =
            serde_json::from_str(lines.first().expect("首行必须存在"))
                .expect("首行必须是合法 JSON");
        assert_eq!(header_value["type"], "session");

        for line in &lines {
            let _: serde_json::Value = serde_json::from_str(line).expect("每行都应是合法 JSON");
        }
    }

    #[tokio::test]
    async fn failed_turn_exits_1_with_error_only_on_stderr() {
        let (tx, rx) = mpsc::unbounded_channel();
        let session = target_session();
        tx.send(Event::Failed {
            session: session.clone(),
            message: "provider 超时".to_owned(),
        })
        .expect("发送不应失败");
        drop(tx);

        let responder = FakeResponder::default();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = render_events(
            rx,
            &responder,
            &session,
            OutputFormat::Text,
            false,
            header(),
            Pending::default(),
            std::future::pending::<()>(),
            &mut stdout,
            &mut stderr,
        )
        .await
        .expect("渲染不应失败");

        assert_eq!(code, 1);
        assert!(stdout.is_empty(), "错误文本不能出现在 stdout");
        let stderr_text = String::from_utf8(stderr).expect("stderr 应为合法 utf8");
        assert!(stderr_text.contains("provider 超时"));
    }

    #[tokio::test]
    async fn cancel_signal_yields_exit_code_130() {
        let (tx, rx) = mpsc::unbounded_channel();
        let session = target_session();
        tx.send(Event::TurnEnd {
            session: session.clone(),
        })
        .expect("发送不应失败");
        drop(tx);

        let responder = FakeResponder::default();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = render_events(
            rx,
            &responder,
            &session,
            OutputFormat::Text,
            false,
            header(),
            Pending::default(),
            std::future::ready(()),
            &mut stdout,
            &mut stderr,
        )
        .await
        .expect("渲染不应失败");

        assert_eq!(code, 130);
        assert_eq!(*responder.cancels.lock().expect("测试内锁未中毒"), 1);
    }

    #[tokio::test]
    async fn broken_pipe_stops_silently_with_exit_code_0() {
        let (tx, rx) = mpsc::unbounded_channel();
        let session = target_session();
        tx.send(Event::TextDelta {
            session: session.clone(),
            entry: EntryId::from("ent_1"),
            index: 0,
            delta: "hi".to_owned(),
        })
        .expect("发送不应失败");
        drop(tx);

        let responder = FakeResponder::default();
        let mut stderr = Vec::new();
        let code = render_events(
            rx,
            &responder,
            &session,
            OutputFormat::Text,
            false,
            header(),
            Pending::default(),
            std::future::pending::<()>(),
            BrokenPipeSink,
            &mut stderr,
        )
        .await
        .expect("broken pipe 不应报错");

        assert_eq!(code, 0);
    }

    #[tokio::test]
    async fn non_interactive_approval_auto_rejects_without_hanging() {
        // cargo test/nextest 跑在非 tty 环境下，`is_interactive()` 恒为假，
        // 这正是本测试要验证的路径。
        let (tx, rx) = mpsc::unbounded_channel();
        let session = target_session();
        tx.send(Event::ApprovalRequested {
            session: session.clone(),
            pending: PendingApproval {
                request_id: ApprovalId::from("appr_1"),
                call_id: CallId::from("call_1"),
                tool_name: "bash".to_owned(),
                scope: "bash".to_owned(),
                prompt: "运行 rm -rf /".to_owned(),
            },
        })
        .expect("发送不应失败");
        tx.send(Event::TurnEnd {
            session: session.clone(),
        })
        .expect("发送不应失败");
        drop(tx);

        let responder = FakeResponder::default();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = render_events(
            rx,
            &responder,
            &session,
            OutputFormat::Text,
            false,
            header(),
            Pending::default(),
            std::future::pending::<()>(),
            &mut stdout,
            &mut stderr,
        )
        .await
        .expect("渲染不应失败");

        assert_eq!(code, 0);
        let approvals = responder.approvals.lock().expect("测试内锁未中毒");
        assert_eq!(approvals.len(), 1);
        assert_eq!(approvals[0].0, ApprovalId::from("appr_1"));
        assert_eq!(approvals[0].1, ApprovalReply::Reject);
    }

    #[tokio::test]
    async fn subscribed_pending_approvals_are_handled_before_prompt_events() {
        let (tx, rx) = mpsc::unbounded_channel();
        let session = target_session();
        tx.send(Event::TurnEnd {
            session: session.clone(),
        })
        .expect("发送不应失败");
        drop(tx);

        let responder = FakeResponder::default();
        let pending = Pending {
            approvals: vec![PendingApproval {
                request_id: ApprovalId::from("appr_pre"),
                call_id: CallId::from("call_pre"),
                tool_name: "edit".to_owned(),
                scope: "edit".to_owned(),
                prompt: "改动 src/main.rs".to_owned(),
            }],
            stdin: Vec::new(),
        };
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = render_events(
            rx,
            &responder,
            &session,
            OutputFormat::Text,
            false,
            header(),
            pending,
            std::future::pending::<()>(),
            &mut stdout,
            &mut stderr,
        )
        .await
        .expect("渲染不应失败");

        assert_eq!(code, 0);
        assert_eq!(responder.approvals.lock().expect("测试内锁未中毒").len(), 1);
    }
}
