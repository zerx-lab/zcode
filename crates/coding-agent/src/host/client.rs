//! 三帧握手 + 读写分离的连接处理循环。
//!
//! # 读写为什么分成两个任务
//!
//! 一条连接同时要做三件事：读客户端请求并处理、把处理结果写回去、把订阅会话产生的事件
//! 转发出去。三者共用同一个出站字节流，但**读路径绝不能被写路径卡住**——
//! `Request::Cancel` 必须在写出任何回应字节之前就把取消信号打出去（见
//! [`zcode_protocol::wire::Request::Cancel`] 的文档），若读写挤在同一个 `.await` 链上，
//! 写端忙着回放大帧积压时取消就会排在积压后面，症状是"按了 Esc 没反应"。
//!
//! 解法：一个 writer 任务独占 `WriteHalf`，从一个 `mpsc::UnboundedSender` 拿预编码好的
//! 信封往外写；读循环与每个会话的事件转发任务都只管往这个 channel 里塞，从不直接碰
//! socket 的写半边。取消处理本身（`CancelRegistry::cancel_session`）在读循环里同步执行，
//! 甚至排在"构造 Ack 回应"之前——它不经过 channel，因此不可能被排队。
//!
//! # 读路径两段式
//!
//! 每一帧先解成 [`RawEnvelope`] 拿到 `id`，payload 解析失败时才用 [`FrameProbe`] 分类，
//! 样例见 `crates/protocol/src/wire/mod.rs` 的模块文档。

use std::collections::HashMap;
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use zcode_agent::event::EventStream;
use zcode_agent::{
    ApprovalGate, ApprovalReply as DomainApprovalReply, EntryId as DomainEntryId,
    SessionId as DomainSessionId,
};
use zcode_protocol::envelope::{Envelope, IdGen};
use zcode_protocol::error::{ErrorCode, ProtocolError};
use zcode_protocol::frame::{FrameDecoder, FrameError, encode};
use zcode_protocol::version::{
    ClientAuth, Hello, Nonce as WireNonce, PROTOCOL_VERSION, Proof, ServerHello,
};
use zcode_protocol::wire::{
    self, ClientFrame, Event, FrameProbe, Pending, RawEnvelope, Reply, Request, ServerFrame,
};
use zcode_utils::daemon::{Domain, Nonce as DaemonNonce, proof, verify_proof};
use zcode_utils::transport::{ReadHalf, Stream, WriteHalf};

use crate::host::sessions::{Claim, SessionHandle};
use crate::host::{Host, HostError, adapter, sessions};

/// 本端在握手里报的实现标识，仅用于日志。
const AGENT_NAME: &str = "zcode-host";

/// 单次 socket 读取的缓冲块大小。
const READ_CHUNK: usize = 64 * 1024;

/// 服务一条客户端连接直到握手失败或对端关闭。
///
/// 真 socket 与 `stream_pair()` 自托管走**同一个**函数——这是 `Host` 模块文档已经写明的
/// 不变量，测试直接喂 `stream_pair()` 就等价于测了跨进程路径。
pub(crate) async fn handle_client(host: Arc<Host>, stream: Stream) -> Result<(), HostError> {
    let (mut read_half, write_half) = stream.into_split();
    let mut decoder = FrameDecoder::new();
    let mut buf = vec![0_u8; READ_CHUNK];
    let out_ids = Arc::new(IdGen::default());
    let (outbox, outbox_rx) = mpsc::unbounded_channel::<Envelope<ServerFrame>>();
    let writer = tokio::spawn(run_writer(write_half, outbox_rx));

    let handshake = perform_handshake(
        &host,
        &mut read_half,
        &mut decoder,
        &mut buf,
        &out_ids,
        &outbox,
    )
    .await;
    let proceed = match handshake {
        Ok(proceed) => proceed,
        Err(error) => {
            drop(outbox);
            let _ = writer.await;
            return Err(error);
        }
    };
    if !proceed {
        drop(outbox);
        let _ = writer.await;
        return Ok(());
    }

    let mut conn = ConnState::default();
    let result = run_request_loop(
        &host,
        &mut read_half,
        &mut decoder,
        &mut buf,
        &out_ids,
        &outbox,
        &mut conn,
    )
    .await;

    // 连接收尾：只清理本连接自己起的事件转发任务，**不碰任何会话或 turn**——
    // 它们属于会话，不属于这条连接，见 `sessions` 模块文档。
    for (_, forwarder) in conn.forwarders.drain() {
        forwarder.abort();
    }
    drop(outbox);
    let _ = writer.await;
    result
}

/// 每条连接自己的状态：目前只有它起的事件转发任务。
#[derive(Default)]
struct ConnState {
    forwarders: HashMap<wire::SessionId, JoinHandle<()>>,
}

/// 完成三帧握手。`Ok(true)` 表示成功，调用方应继续进入请求循环；`Ok(false)` 表示握手
/// 按协议失败并已经回过错误帧，调用方应当直接关闭连接；`Err` 是硬 I/O 失败。
async fn perform_handshake(
    host: &Host,
    read_half: &mut ReadHalf,
    decoder: &mut FrameDecoder,
    buf: &mut [u8],
    out_ids: &IdGen,
    outbox: &mpsc::UnboundedSender<Envelope<ServerFrame>>,
) -> Result<bool, HostError> {
    // 帧 1：ClientHello。
    let Some(raw) = next_envelope(read_half, decoder, buf).await? else {
        return Ok(false);
    };
    let client_hello = match raw.parse_payload::<ClientFrame>() {
        Ok(ClientFrame::Hello(hello)) => hello,
        Ok(_) => {
            send_error(
                outbox,
                out_ids,
                Some(raw.id),
                ProtocolError::new(ErrorCode::HandshakeRequired, "首帧必须是 ClientHello"),
            );
            return Ok(false);
        }
        Err(_) => {
            send_error(outbox, out_ids, Some(raw.id), classify_probe(&raw.probe()));
            return Ok(false);
        }
    };

    if let Err(mismatch) = PROTOCOL_VERSION.negotiate(client_hello.hello.version) {
        send_error(outbox, out_ids, Some(raw.id), mismatch.into());
        return Ok(false);
    }

    // 帧 2：ServerHello——服务端先证明持有密钥，客户端才会在帧 3 出示自己的应答。
    let client_nonce = DaemonNonce::from(client_hello.nonce.0);
    let server_nonce = DaemonNonce::generate()?;
    let server_proof = proof(&host.deps.secret, Domain::Server, &client_nonce);
    let server_hello = Envelope::reply_to(
        out_ids.next_id(),
        raw.id,
        ServerFrame::Hello(ServerHello {
            hello: Hello::local(AGENT_NAME),
            nonce: WireNonce(server_nonce.as_str().to_owned()),
            proof: Proof(server_proof),
        }),
    );
    if outbox.send(server_hello).is_err() {
        return Ok(false);
    }

    // 帧 3：ClientAuth。
    let Some(raw) = next_envelope(read_half, decoder, buf).await? else {
        return Ok(false);
    };
    let client_auth: ClientAuth = match raw.parse_payload::<ClientFrame>() {
        Ok(ClientFrame::Auth(auth)) => auth,
        Ok(_) => {
            send_error(
                outbox,
                out_ids,
                Some(raw.id),
                ProtocolError::new(ErrorCode::HandshakeRequired, "第二帧必须是 ClientAuth"),
            );
            return Ok(false);
        }
        Err(_) => {
            send_error(outbox, out_ids, Some(raw.id), classify_probe(&raw.probe()));
            return Ok(false);
        }
    };

    if !verify_proof(
        &host.deps.secret,
        Domain::Client,
        &server_nonce,
        &client_auth.proof.0,
    ) {
        send_error(
            outbox,
            out_ids,
            Some(raw.id),
            ProtocolError::new(ErrorCode::Unauthorized, "握手校验失败"),
        );
        return Ok(false);
    }

    Ok(true)
}

/// 握手完成后的主循环：逐帧读、逐帧处理、逐帧回。
async fn run_request_loop(
    host: &Arc<Host>,
    read_half: &mut ReadHalf,
    decoder: &mut FrameDecoder,
    buf: &mut [u8],
    out_ids: &Arc<IdGen>,
    outbox: &mpsc::UnboundedSender<Envelope<ServerFrame>>,
    conn: &mut ConnState,
) -> Result<(), HostError> {
    loop {
        let Some(raw) = next_envelope(read_half, decoder, buf).await? else {
            return Ok(());
        };
        match raw.parse_payload::<ClientFrame>() {
            Ok(ClientFrame::Request(request)) => {
                // 取消必须先于任何回应字节：在构造 Ack 之前、同步触发。
                if let Request::Cancel { session } = &request {
                    host.cancels.cancel_session(&domain_session_id(session));
                }
                let outcome = dispatch_request(host, out_ids, outbox, conn, request).await;
                let payload = to_server_frame(outcome);
                let envelope = Envelope::reply_to(out_ids.next_id(), raw.id, payload);
                if outbox.send(envelope).is_err() {
                    return Ok(());
                }
            }
            Ok(ClientFrame::Hello(_) | ClientFrame::Auth(_)) => {
                send_error(
                    outbox,
                    out_ids,
                    Some(raw.id),
                    ProtocolError::new(
                        ErrorCode::HandshakeRequired,
                        "握手已完成，不应再收到握手帧",
                    ),
                );
            }
            Ok(ClientFrame::Error(error)) => {
                tracing::warn!(?error, "客户端上报了一条协议错误");
            }
            Err(_) => {
                send_error(outbox, out_ids, Some(raw.id), classify_probe(&raw.probe()));
            }
        }
    }
}

/// 把一次请求处理结果译成回应帧。
///
/// `ErrorCode` 只覆盖协议层失败（见其模块文档），本仓目前没有为"领域失败"单独定义
/// 错误码——`Request` 引用了不存在的会话这类情况，退而求其次挂在
/// [`ErrorCode::MalformedFrame`] 上：这不是完美的语义匹配，但好过让调用方在等一个
/// 永远不会来的 `Reply`。
fn to_server_frame(outcome: Result<Reply, HostError>) -> ServerFrame {
    match outcome {
        Ok(reply) => ServerFrame::Reply(reply),
        Err(error) => {
            tracing::warn!(%error, "请求处理失败");
            ServerFrame::Error(ProtocolError::new(
                ErrorCode::MalformedFrame,
                error.to_string(),
            ))
        }
    }
}

/// 分发一条已解析的 [`Request`]。
// 对 16 个 `Request` 变体的**穷尽** match，没有 `_` 兜底：协议加了新请求而本端没实现时，
// 必须是编译错误而不是运行期静默丢弃——丢弃会让对端永远等不到 `reply_to`
// （`crates/protocol/src/wire/request.rs:5-8`）。拆分就要补 fallthrough，护栏当场失效。
#[expect(clippy::too_many_lines, reason = "穷尽 match 不可拆，见上方注释")]
async fn dispatch_request(
    host: &Arc<Host>,
    out_ids: &Arc<IdGen>,
    outbox: &mpsc::UnboundedSender<Envelope<ServerFrame>>,
    conn: &mut ConnState,
    request: Request,
) -> Result<Reply, HostError> {
    match request {
        Request::Ping => Ok(Reply::Pong),

        Request::SessionList { cwd } => {
            let sessions =
                sessions::list_summaries(&host.deps.sessions_dir, cwd.as_deref()).await?;
            Ok(Reply::Sessions { sessions })
        }

        Request::SessionCreate { cwd: _, model } => {
            // 不信任客户端自报的 cwd：Host 只服务一个 workspace，会话的 cwd 必须来自
            // `deps.workspace.root()`，否则客户端能把会话建到任意目录，工具执行时的 cwd
            // 就跟着任意目录走了。`Request::SessionCreate.cwd` 字段因此被忽略，不是缺陷。
            let cwd = host.deps.workspace.root().to_string_lossy().into_owned();
            let (_, summary, root) = host
                .sessions
                .create(&host.deps, &host.cancels, cwd, model)
                .await?;
            Ok(Reply::SessionCreated { summary, root })
        }

        Request::Subscribe {
            session,
            client,
            has_local_history: _,
            takeover,
            since,
        } => {
            handle_subscribe(
                host,
                out_ids,
                outbox,
                conn,
                SubscribeArgs {
                    session,
                    client,
                    takeover,
                    since,
                },
            )
            .await
        }

        Request::Unsubscribe { session } => {
            if let Some(forwarder) = conn.forwarders.remove(&session) {
                forwarder.abort();
            }
            Ok(Reply::Ok)
        }

        Request::HistoryFetch { session, since } => {
            let handle = load_session(host, &session).await?;
            let snapshot = handle.snapshot(since.map(domain_entry_id)).await?;
            Ok(Reply::History {
                entries: snapshot.entries,
            })
        }

        Request::Prompt { session, content } => {
            let handle = load_session(host, &session).await?;
            let text = adapter::prompt_text_from_wire(&content);
            let user_entry = handle.prompt(text).await?;
            Ok(Reply::TurnStarted {
                user_entry: adapter::entry_id(&user_entry),
            })
        }

        Request::Cancel { session: _ } => {
            // 信号已经在 `run_request_loop` 里、构造这个回应之前同步触发过了。
            Ok(Reply::Ok)
        }

        Request::Compact { session } => {
            let handle = load_session(host, &session).await?;
            handle.compact().await?;
            Ok(Reply::Ok)
        }

        Request::SetHead { session, entry } => {
            let handle = load_session(host, &session).await?;
            let moved = handle.set_head(domain_entry_id(entry)).await?;
            if moved {
                publish_session_updated(&handle, &session).await?;
            } else {
                tracing::warn!(%session, "SetHead 引用了不存在的条目，已忽略");
            }
            Ok(Reply::Ok)
        }

        Request::SetModel { session, model } => {
            let handle = load_session(host, &session).await?;
            handle.set_model(model).await?;
            publish_session_updated(&handle, &session).await?;
            Ok(Reply::Ok)
        }

        Request::SetTitle { session, title } => {
            let handle = load_session(host, &session).await?;
            handle.set_title(title).await?;
            publish_session_updated(&handle, &session).await?;
            Ok(Reply::Ok)
        }

        Request::SetApprovalMode { session, mode } => {
            let handle = load_session(host, &session).await?;
            handle
                .set_approval_mode(adapter::approval_mode_from_wire(mode))
                .await?;
            Ok(Reply::Ok)
        }

        Request::ApprovalRespond { request_id, reply } => {
            // 重复应答同一个 request_id 静默成功：多客户端几乎同时点“允许”是正常操作。
            let domain_reply: DomainApprovalReply = adapter::approval_reply_from_wire(reply);
            let _ = host
                .sessions
                .reply_approval(request_id.as_str(), domain_reply)
                .await;
            Ok(Reply::Ok)
        }

        Request::StdinRespond { request_id, text } => {
            let _ = host.sessions.reply_stdin(request_id.as_str(), text).await;
            Ok(Reply::Ok)
        }

        Request::PendingList { session } => {
            let handle = load_session(host, &session).await?;
            Ok(Reply::Pending {
                pending: build_pending(&handle),
            })
        }
    }
}

/// [`handle_subscribe`] 的请求侧参数。
///
/// 单独成一个结构体只为把参数个数压回 clippy 的阈值内；四个字段一一对应
/// `Request::Subscribe` 的同名字段，语义见 `crates/protocol/src/wire/request.rs:75-104`。
/// `has_local_history` 不在其中：它是仲裁判据而非订阅载荷，当前实现未用到。
struct SubscribeArgs {
    session: wire::SessionId,
    client: wire::ClientId,
    takeover: bool,
    since: Option<wire::EntryId>,
}

/// `Subscribe` 的完整实现：仲裁持有权、取快照、把待回答项一并带回、起/换事件转发任务。
async fn handle_subscribe(
    host: &Arc<Host>,
    out_ids: &Arc<IdGen>,
    outbox: &mpsc::UnboundedSender<Envelope<ServerFrame>>,
    conn: &mut ConnState,
    args: SubscribeArgs,
) -> Result<Reply, HostError> {
    let SubscribeArgs {
        session,
        client,
        takeover,
        since,
    } = args;
    let handle = load_session(host, &session).await?;

    match handle.claim(&client, takeover) {
        Claim::Busy { holder } => return Ok(Reply::SessionBusy { holder }),
        Claim::Granted => {}
    }

    let snapshot = handle.snapshot(since.map(domain_entry_id)).await?;
    let pending = build_pending(&handle);
    let turn_active = host.cancels.is_turn_active(&domain_session_id(&session));

    // 重复订阅同一 session 是幂等的：换掉旧的转发任务，不重复转发。
    if let Some(old) = conn.forwarders.remove(&session) {
        old.abort();
    }
    let forwarder = tokio::spawn(forward_session_events(
        session.clone(),
        handle.events.subscribe(),
        handle.subscribe_meta(),
        Arc::clone(&handle.approvals),
        outbox.clone(),
        Arc::clone(out_ids),
    ));
    conn.forwarders.insert(session, forwarder);

    Ok(Reply::Subscribed {
        summary: snapshot.summary,
        head: snapshot.head,
        entries: snapshot.entries,
        pending,
        turn_active,
    })
}

/// 待回答项列表：审批 + stdin，`Subscribe`/`PendingList` 共用。
fn build_pending(handle: &SessionHandle) -> Pending {
    Pending {
        approvals: handle
            .approvals
            .pending()
            .iter()
            .map(adapter::pending_approval_to_wire)
            .collect(),
        stdin: handle
            .stdin
            .pending()
            .iter()
            .map(adapter::pending_stdin_to_wire)
            .collect(),
    }
}

/// 取一个会话句柄；不在表内时懒加载。
async fn load_session(
    host: &Arc<Host>,
    session: &wire::SessionId,
) -> Result<Arc<SessionHandle>, HostError> {
    host.sessions
        .get_or_load(&host.deps, &host.cancels, &domain_session_id(session))
        .await
}

/// `SetHead`/`SetModel`/`SetTitle` 成功后广播一条 `Event::SessionUpdated`，让同一会话上
/// 的其他客户端跟着更新——`AgentEvent` 没有对应变体（这是宿主层直接改的元数据，不是运行时
/// 领域事件），走 [`SessionHandle::publish_meta`] 这条单独的合成事件通道。
async fn publish_session_updated(
    handle: &SessionHandle,
    session: &wire::SessionId,
) -> Result<(), HostError> {
    let snapshot = handle.snapshot(None).await?;
    handle.publish_meta(Event::SessionUpdated {
        session: session.clone(),
        summary: snapshot.summary,
        head: snapshot.head,
    });
    Ok(())
}

/// 一个会话的事件转发任务：把 [`zcode_agent::AgentEvent`] 与宿主层合成事件都译成
/// [`Event`] 推进 `outbox`。
///
/// 两条源各自处理慢消费者：`agent_events`（[`EventStream`]）内部已经把
/// `broadcast::error::RecvError::Lagged` 转成 `AgentEvent::Resync`（见
/// `zcode_agent::event` 的模块文档）；`meta_events` 是宿主层自己开的广播通道，Lagged
/// 在这里手动转成 `Event::Resync`。两条路径都不关连接，流不断。
async fn forward_session_events(
    session: wire::SessionId,
    mut agent_events: EventStream,
    mut meta_events: tokio::sync::broadcast::Receiver<Event>,
    approvals: Arc<ApprovalGate>,
    outbox: mpsc::UnboundedSender<Envelope<ServerFrame>>,
    ids: Arc<IdGen>,
) {
    loop {
        let event = tokio::select! {
            biased;
            agent_event = agent_events.recv() => {
                match agent_event {
                    Some(event) => adapter::agent_event_to_wire(&session, event, &approvals),
                    // `AgentRuntime`（进而它的 `EventSink`）与 `SessionHandle` 同生共死，
                    // 这条分支理论上不可达；真出现也只是静默结束这个转发任务。
                    None => return,
                }
            }
            meta_event = meta_events.recv() => {
                match meta_event {
                    Ok(event) => Some(event),
                    Err(RecvError::Lagged(dropped)) => Some(Event::Resync {
                        session: session.clone(),
                        dropped,
                    }),
                    Err(RecvError::Closed) => return,
                }
            }
        };
        let Some(event) = event else { continue };
        let envelope = Envelope::new(ids.next_id(), ServerFrame::Event(event));
        if outbox.send(envelope).is_err() {
            return;
        }
    }
}

/// 出站 forwarder：独占 `WriteHalf`，从 `outbox` 拿预先构造好的信封逐个编码写出。
///
/// 这是一条连接**唯一**触碰 socket 写半边的地方——取消能抢在回应之前发出的前提正是
/// 读路径与写路径物理上分开，见模块文档。
async fn run_writer(
    mut write_half: WriteHalf,
    mut outbox: mpsc::UnboundedReceiver<Envelope<ServerFrame>>,
) {
    let mut bytes = Vec::new();
    while let Some(envelope) = outbox.recv().await {
        bytes.clear();
        if let Err(error) = encode(&envelope, &mut bytes) {
            tracing::warn!(%error, "帧编码失败，丢弃这一帧");
            continue;
        }
        if let Err(error) = write_half.write_all(&bytes).await {
            tracing::warn!(%error, "写出失败，连接即将关闭");
            return;
        }
    }
}

/// 读一帧信封：先攒够字节再解 [`RawEnvelope`]，单行损坏就跳过继续读下一行。
///
/// [`FrameError::TooLarge`] 之后解帧器状态已无意义，调用方必须断开连接——直接把它转成
/// [`HostError`] 向上传播即可，`handle_client` 顶层会因为这个 `Err` 提前返回。
async fn next_envelope(
    read_half: &mut ReadHalf,
    decoder: &mut FrameDecoder,
    buf: &mut [u8],
) -> Result<Option<RawEnvelope>, HostError> {
    loop {
        match decoder.decode::<RawEnvelope>() {
            Ok(Some(envelope)) => return Ok(Some(envelope)),
            Ok(None) => {}
            Err(FrameError::TooLarge { len, limit }) => {
                return Err(FrameError::TooLarge { len, limit }.into());
            }
            Err(FrameError::Json(error)) => {
                tracing::warn!(%error, "跳过一行无法解析为信封的数据");
                continue;
            }
        }
        let read = read_half.read(buf).await?;
        if read == 0 {
            return Ok(None);
        }
        decoder.push(buf.get(..read).unwrap_or_default());
    }
}

/// payload 解不出 [`ClientFrame`] 时，按探针分类成协议错误。
///
/// `kind == "request"` 必须回 [`ErrorCode::UnsupportedRequest`]，否则等 `reply_to` 的
/// 调用方会永久挂着；其余一律 [`ErrorCode::MalformedFrame`]。样例见
/// `crates/protocol/src/wire/mod.rs` 的模块文档。
fn classify_probe(probe: &FrameProbe) -> ProtocolError {
    if probe.kind.as_deref() == Some("request") {
        ProtocolError::unsupported_request(probe.name.as_deref().unwrap_or("<unnamed>"))
    } else {
        ProtocolError::new(ErrorCode::MalformedFrame, "帧结构不符")
    }
}

/// 发一帧协议错误。`reply_to` 为 `None` 时是主动推送（目前只有握手失败会走这条）。
fn send_error(
    outbox: &mpsc::UnboundedSender<Envelope<ServerFrame>>,
    ids: &IdGen,
    reply_to: Option<u64>,
    error: ProtocolError,
) {
    let payload = ServerFrame::Error(error);
    let envelope = match reply_to {
        Some(request_id) => Envelope::reply_to(ids.next_id(), request_id, payload),
        None => Envelope::new(ids.next_id(), payload),
    };
    let _ = outbox.send(envelope);
}

/// wire 会话 id → 域会话 id。
fn domain_session_id(id: &wire::SessionId) -> DomainSessionId {
    DomainSessionId::from(id.as_str().to_owned())
}

/// wire 条目 id → 域条目 id。
fn domain_entry_id(id: wire::EntryId) -> DomainEntryId {
    DomainEntryId::from(id.into_string())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Duration;

    use async_trait::async_trait;
    use futures_util::stream;
    use zcode_ai::{
        AiError, CompletionRequest, Provider, ProviderId, StopReason as AiStopReason, StreamEvent,
        Usage as AiUsage,
    };
    use zcode_protocol::version::ClientHello;
    use zcode_utils::daemon::Secret;
    use zcode_utils::transport::stream_pair;

    use super::*;
    use crate::host::HostDeps;

    /// 一句话就说完的假 provider，供整条 wire 协议路径的测试复用。
    #[derive(Debug)]
    struct EchoProvider;

    #[async_trait]
    impl Provider for EchoProvider {
        fn id(&self) -> ProviderId {
            ProviderId::Anthropic
        }

        async fn stream(
            &self,
            _request: &CompletionRequest,
        ) -> Result<zcode_ai::EventStream, AiError> {
            let events = vec![
                Ok(StreamEvent::Start {
                    response_id: None,
                    model: None,
                }),
                Ok(StreamEvent::TextStart { index: 0 }),
                Ok(StreamEvent::TextDelta {
                    index: 0,
                    delta: "好的".to_owned(),
                }),
                Ok(StreamEvent::TextEnd {
                    index: 0,
                    text: "好的".to_owned(),
                }),
                Ok(StreamEvent::Done {
                    stop_reason: AiStopReason::Stop,
                    usage: AiUsage::default(),
                }),
            ];
            Ok(Box::pin(stream::iter(events)))
        }
    }

    /// 装一个跑在内存里的 `Host`：假 provider、空工具表、临时会话目录、随机密钥。
    fn test_host() -> (Arc<Host>, Secret, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir 创建失败");
        let secret = Secret::generate().expect("密钥生成失败");
        let deps = HostDeps {
            provider: Arc::new(EchoProvider),
            registry: Arc::new(zcode_agent::tool::registry::ToolRegistry::new()),
            config: Arc::new(crate::config::Config {
                model: crate::config::ModelConfig {
                    id: None,
                    thinking: None,
                    provider: None,
                },
                approval: crate::config::ApprovalConfig {
                    mode: zcode_agent::ApprovalMode::default(),
                    policies: HashMap::new(),
                },
                tools: crate::config::ToolsConfig {
                    disabled: Vec::new(),
                    bash_timeout_secs: 30,
                    read_max_lines: 2000,
                },
                session: crate::config::SessionConfig {
                    dir: dir.path().to_path_buf(),
                },
                daemon: crate::config::DaemonConfig {
                    enabled: false,
                    runtime_dir: dir.path().to_path_buf(),
                },
                ui: crate::config::UiConfig {
                    show_thinking: false,
                },
            }),
            prompts: Arc::new(crate::prompt::PromptSet {
                system: Vec::new(),
                session_context: String::new(),
            }),
            model: crate::model::ResolvedModel {
                id: "test-model".to_owned(),
                provider: ProviderId::Anthropic,
                context_window: 200_000,
                thinking: zcode_ai::Thinking::default(),
            },
            workspace: Arc::new(crate::workspace::Workspace::new(PathBuf::from("."))),
            sessions_dir: dir.path().to_path_buf(),
            secret: secret.clone(),
        };
        (Host::new(deps), secret, dir)
    }

    /// 测试用最小 wire 客户端：手写握手 + 逐帧收发，不依赖 `HostDaemon` 的 `ClientSession`
    /// （那是另一个模块的产物，本测试直接用 `zcode_protocol`/`zcode_utils` 的原语，
    /// 与真实客户端的实现方式完全一致）。
    struct TestClient {
        read_half: ReadHalf,
        write_half: WriteHalf,
        decoder: FrameDecoder,
        ids: IdGen,
        buf: Vec<u8>,
    }

    impl TestClient {
        fn new(stream: Stream) -> Self {
            let (read_half, write_half) = stream.into_split();
            Self {
                read_half,
                write_half,
                decoder: FrameDecoder::new(),
                ids: IdGen::default(),
                buf: vec![0_u8; 64 * 1024],
            }
        }

        async fn send(&mut self, payload: ClientFrame) -> u64 {
            let id = self.ids.next_id();
            let mut bytes = Vec::new();
            encode(&Envelope::new(id, payload), &mut bytes).expect("编码失败");
            self.write_half.write_all(&bytes).await.expect("写入失败");
            id
        }

        /// 发一条不合法的原始字节行（跳过编码，直接测服务端的探针分类路径）。
        async fn send_raw_line(&mut self, line: &str) {
            let mut bytes = line.as_bytes().to_vec();
            bytes.push(b'\n');
            self.write_half.write_all(&bytes).await.expect("写入失败");
        }

        async fn recv(&mut self) -> Envelope<ServerFrame> {
            loop {
                if let Ok(Some(envelope)) = self.decoder.decode::<Envelope<ServerFrame>>() {
                    return envelope;
                }
                let read = self.read_half.read(&mut self.buf).await.expect("读取失败");
                assert!(read > 0, "连接在等待回应时提前关闭");
                self.decoder.push(self.buf.get(..read).unwrap_or_default());
            }
        }

        /// 完成三帧握手。`claimed_secret` 是本端用来算 `ClientAuth.proof` 的密钥——测试可以
        /// 故意传一个错的，服务端应当拒绝。`real_secret` 用来校验服务端自己的应答，
        /// 任何测试都必须用服务端真正持有的那份，否则连"服务端确实证明了持有密钥"这一步
        /// 本身就验证不了。
        async fn handshake(&mut self, real_secret: &Secret, claimed_secret: &Secret) {
            let client_nonce = zcode_utils::daemon::Nonce::generate().expect("nonce 生成失败");
            self.send(ClientFrame::Hello(ClientHello {
                hello: Hello::local("zcode-test-client"),
                nonce: WireNonce(client_nonce.as_str().to_owned()),
            }))
            .await;

            let envelope = self.recv().await;
            let ServerFrame::Hello(server_hello) = envelope.payload else {
                panic!("期望 ServerHello，实际收到 {:?}", envelope.payload);
            };
            let server_nonce = zcode_utils::daemon::Nonce::from(server_hello.nonce.0);
            assert!(
                verify_proof(
                    real_secret,
                    Domain::Server,
                    &client_nonce,
                    &server_hello.proof.0
                ),
                "服务端未能证明持有密钥"
            );

            let client_proof = proof(claimed_secret, Domain::Client, &server_nonce);
            self.send(ClientFrame::Auth(ClientAuth {
                proof: Proof(client_proof),
            }))
            .await;
        }
    }

    fn expect_reply(envelope: Envelope<ServerFrame>, request_id: u64) -> Reply {
        assert_eq!(
            envelope.reply_to,
            Some(request_id),
            "reply_to 必须指向对应请求"
        );
        match envelope.payload {
            ServerFrame::Reply(reply) => reply,
            other => panic!("期望 Reply，实际收到 {other:?}"),
        }
    }

    #[tokio::test]
    async fn handshake_succeeds_and_ping_round_trips() {
        let (host, secret, _dir) = test_host();
        let (server_stream, client_stream) = stream_pair().await.expect("stream_pair 失败");
        let server = tokio::spawn(handle_client(host, server_stream));

        let mut client = TestClient::new(client_stream);
        client.handshake(&secret, &secret).await;

        let id = client.send(ClientFrame::Request(Request::Ping)).await;
        let reply = expect_reply(client.recv().await, id);
        assert_eq!(reply, Reply::Pong);

        drop(client);
        server
            .await
            .expect("任务不应 panic")
            .expect("连接应正常结束");
    }

    #[tokio::test]
    async fn handshake_rejects_wrong_client_secret() {
        let (host, secret, _dir) = test_host();
        let wrong_secret = Secret::generate().expect("密钥生成失败");
        let (server_stream, client_stream) = stream_pair().await.expect("stream_pair 失败");
        let server = tokio::spawn(handle_client(host, server_stream));

        let mut client = TestClient::new(client_stream);
        client.handshake(&secret, &wrong_secret).await;

        let envelope = client.recv().await;
        let ServerFrame::Error(error) = envelope.payload else {
            panic!("期望协议错误帧，实际收到 {:?}", envelope.payload);
        };
        assert_eq!(error.code, ErrorCode::Unauthorized);

        drop(client);
        server
            .await
            .expect("任务不应 panic")
            .expect("连接应正常结束");
    }

    #[tokio::test]
    async fn unknown_request_type_gets_unsupported_request_with_reply_to() {
        let (host, secret, _dir) = test_host();
        let (server_stream, client_stream) = stream_pair().await.expect("stream_pair 失败");
        let server = tokio::spawn(handle_client(host, server_stream));

        let mut client = TestClient::new(client_stream);
        client.handshake(&secret, &secret).await;

        let id = client.ids.next_id();
        client
            .send_raw_line(&format!(
                r#"{{"v":1,"id":{id},"payload":{{"kind":"request","type":"time_travel"}}}}"#
            ))
            .await;

        let envelope = client.recv().await;
        assert_eq!(envelope.reply_to, Some(id));
        let ServerFrame::Error(error) = envelope.payload else {
            panic!("期望协议错误帧，实际收到 {:?}", envelope.payload);
        };
        assert_eq!(error.code, ErrorCode::UnsupportedRequest);

        drop(client);
        server
            .await
            .expect("任务不应 panic")
            .expect("连接应正常结束");
    }

    #[tokio::test]
    async fn malformed_payload_gets_malformed_frame_with_reply_to() {
        let (host, secret, _dir) = test_host();
        let (server_stream, client_stream) = stream_pair().await.expect("stream_pair 失败");
        let server = tokio::spawn(handle_client(host, server_stream));

        let mut client = TestClient::new(client_stream);
        client.handshake(&secret, &secret).await;

        let id = client.ids.next_id();
        // 合法 JSON、合法信封，但 payload 既不是 request 也不是任何已知 kind。
        client
            .send_raw_line(&format!(
                r#"{{"v":1,"id":{id},"payload":{{"kind":"telepathy"}}}}"#
            ))
            .await;

        let envelope = client.recv().await;
        assert_eq!(envelope.reply_to, Some(id));
        let ServerFrame::Error(error) = envelope.payload else {
            panic!("期望协议错误帧，实际收到 {:?}", envelope.payload);
        };
        assert_eq!(error.code, ErrorCode::MalformedFrame);

        drop(client);
        server
            .await
            .expect("任务不应 panic")
            .expect("连接应正常结束");
    }

    fn dummy_summary(session: &wire::SessionId) -> wire::SessionSummary {
        wire::SessionSummary {
            id: session.clone(),
            title: None,
            cwd: "/dummy".to_owned(),
            model: "m".to_owned(),
            created_ms: 0,
            updated_ms: 0,
        }
    }

    #[tokio::test]
    async fn meta_lag_becomes_resync_and_forwarding_continues() {
        let events = zcode_agent::EventSink::new();
        let agent_stream = events.subscribe();
        // 容量故意开得很小，几条 SessionUpdated 就能把它灌满触发 Lagged。
        let (meta_tx, meta_rx) = tokio::sync::broadcast::channel::<Event>(4);
        let approvals = Arc::new(ApprovalGate::new(events));
        let (outbox, mut outbox_rx) = mpsc::unbounded_channel();
        let ids = Arc::new(IdGen::default());
        let session = wire::SessionId::from("ses_lag_test");

        let forwarder = tokio::spawn(forward_session_events(
            session.clone(),
            agent_stream,
            meta_rx,
            approvals,
            outbox,
            ids,
        ));

        for _ in 0..10 {
            let _ = meta_tx.send(Event::SessionUpdated {
                session: session.clone(),
                summary: dummy_summary(&session),
                head: wire::EntryId::from("ent_1"),
            });
        }
        let _ = meta_tx.send(Event::TurnEnd {
            session: session.clone(),
        });

        let mut saw_resync = false;
        let mut saw_turn_end = false;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while tokio::time::Instant::now() < deadline {
            let Ok(Some(envelope)) =
                tokio::time::timeout(Duration::from_secs(1), outbox_rx.recv()).await
            else {
                break;
            };
            match envelope.payload {
                ServerFrame::Event(Event::Resync { dropped, .. }) => {
                    assert!(dropped > 0, "Resync 必须报告非零丢帧数");
                    saw_resync = true;
                }
                ServerFrame::Event(Event::TurnEnd { .. }) => {
                    saw_turn_end = true;
                    break;
                }
                _ => {}
            }
        }
        assert!(
            saw_resync,
            "落后的订阅者应该收到 Resync，而不是让流直接断掉"
        );
        assert!(saw_turn_end, "Resync 之后流必须继续送达后续事件");
        forwarder.abort();
    }

    #[tokio::test]
    async fn pending_list_recovers_pending_approvals() {
        let (host, secret, _dir) = test_host();
        let (server_stream, client_stream) = stream_pair().await.expect("stream_pair 失败");
        let server = tokio::spawn(handle_client(Arc::clone(&host), server_stream));

        let mut client = TestClient::new(client_stream);
        client.handshake(&secret, &secret).await;

        let create_id = client
            .send(ClientFrame::Request(Request::SessionCreate {
                cwd: "/cwd".to_owned(),
                model: "m".to_owned(),
            }))
            .await;
        let Reply::SessionCreated { summary, .. } = expect_reply(client.recv().await, create_id)
        else {
            panic!("期望 SessionCreated");
        };
        let session = summary.id;

        // 不走真实工具执行，直接在内部句柄上模拟一次审批询问，验证 PendingList 能重拉到它。
        let handle = host
            .sessions
            .get_or_load(
                &host.deps,
                &host.cancels,
                &zcode_agent::SessionId::from(session.as_str().to_owned()),
            )
            .await
            .expect("会话应该已存在");
        let asking = Arc::clone(&handle);
        tokio::spawn(async move {
            let _ = asking
                .approvals
                .ask("call_1", "bash", "bash", "运行一个危险命令".to_owned())
                .await;
        });
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while handle.approvals.pending().is_empty() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "审批询问应该很快入队"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }

        let pending_id = client
            .send(ClientFrame::Request(Request::PendingList {
                session: session.clone(),
            }))
            .await;
        let Reply::Pending { pending } = expect_reply(client.recv().await, pending_id) else {
            panic!("期望 Pending");
        };
        assert_eq!(pending.approvals.len(), 1);
        assert_eq!(pending.approvals[0].tool_name, "bash");

        drop(client);
        server.abort();
    }

    #[tokio::test]
    async fn turn_survives_client_disconnect_and_is_visible_after_reconnect() {
        let (host, secret, _dir) = test_host();

        let (server_stream, client_stream) = stream_pair().await.expect("stream_pair 失败");
        let server = tokio::spawn(handle_client(Arc::clone(&host), server_stream));
        let mut client = TestClient::new(client_stream);
        client.handshake(&secret, &secret).await;

        let create_id = client
            .send(ClientFrame::Request(Request::SessionCreate {
                cwd: "/cwd".to_owned(),
                model: "m".to_owned(),
            }))
            .await;
        let Reply::SessionCreated { summary, .. } = expect_reply(client.recv().await, create_id)
        else {
            panic!("期望 SessionCreated");
        };
        let session = summary.id;

        let prompt_id = client
            .send(ClientFrame::Request(Request::Prompt {
                session: session.clone(),
                content: vec![wire::UserContent::Text {
                    text: "你好".to_owned(),
                }],
            }))
            .await;
        let Reply::TurnStarted { .. } = expect_reply(client.recv().await, prompt_id) else {
            panic!("期望 TurnStarted");
        };

        // 立刻整体断开这条连接：读循环应该在下一次读到 EOF 时正常收尾。
        drop(client);
        server
            .await
            .expect("任务不应 panic")
            .expect("连接应正常结束（EOF）");

        // 用一条全新的连接重新订阅，轮询直到 turn 跑完，确认历史里有完整的助手回复。
        let (server_stream2, client_stream2) = stream_pair().await.expect("stream_pair 失败");
        let server2 = tokio::spawn(handle_client(Arc::clone(&host), server_stream2));
        let mut client2 = TestClient::new(client_stream2);
        client2.handshake(&secret, &secret).await;

        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let sub_id = client2
                .send(ClientFrame::Request(Request::Subscribe {
                    session: session.clone(),
                    client: wire::ClientId::from("client-2"),
                    has_local_history: false,
                    takeover: false,
                    since: None,
                }))
                .await;
            let Reply::Subscribed {
                turn_active,
                entries,
                ..
            } = expect_reply(client2.recv().await, sub_id)
            else {
                panic!("期望 Subscribed");
            };
            if !turn_active {
                let has_assistant_reply = entries.iter().any(|entry| {
                    matches!(
                        &entry.kind,
                        wire::EntryKind::Message {
                            message: wire::Message::Assistant { .. },
                        }
                    )
                });
                assert!(has_assistant_reply, "turn 跑完后历史里应该有一条助手回复");
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "turn 应该在合理时间内跑完"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        drop(client2);
        server2.abort();
    }
}
