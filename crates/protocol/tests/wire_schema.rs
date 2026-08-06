//! 协议形状快照测试：防止 wire 变体的意外破坏性变更。
//!
//! # 为什么要写"形状快照"而不是逐字段单测
//!
//! `Request` / `Reply` / `Event` 的每个变体都直接决定 wire 字节形状，客户端与运行时
//! 分别编译、分别发布，谁也看不到对方改了什么。jcode 用同类快照挡住这类漂移
//! （`crates/jcode-harness-api/src/lib.rs:26-30`）：把每个 API 方法的一份固定 JSON 样本
//! 提交进仓库，任何一次改动都要逐字节对比。本测试照搬这个思路：把三个枚举的**全部变体**
//! 各造一个确定性样本（无随机、无墙钟时间），序列化后与 `tests/wire-schema.json` 逐字节
//! 比对——字段改名、删字段、加必填字段都会在这里先炸，而不是等两端分别发布后才在生产
//! 环境发现协议不兼容。
//!
//! # 双重穷尽性哨兵
//!
//! 只数"样本数量"防不住两种同时发生的疏漏叠加成的假绿：
//!
//! 1. `_exhaustive_*` 系列函数对枚举做**逐行、禁止 `_` / `|` 合并**的 `match`——新增变体时
//!    编译器立刻报 non-exhaustive match，逼你回到这个文件补一行。
//! 2. `REQUEST_VARIANTS` / `REPLY_VARIANTS` / `EVENT_VARIANTS` 与 `sample_*` 的长度做运行时
//!    断言——补完 match 分支之后，还得记得往样本表里加真样本，这层断言兜住"match 补了、
//!    样本忘加"的情况。
//!
//! 两层缺一不可：只有 (1) 挡不住"match 分支加了、样本表忘加"；只有 (2) 挡不住"样本数凑巧
//! 对上、match 却用 `_ => {}` 悄悄吸收了新变体"。

// 本文件的构建/断言辅助函数只在 `.to_value`/文件 I/O 失败时才会 `expect`——那两种失败本身
// 就是测试应当失败的方式，不需要额外传播出 Result 再在 `#[test]` 体内二次 `expect`。
// 参照 `crates/ai/tests/streaming.rs:9` 的同一做法。
#![expect(clippy::expect_used, reason = "集成测试的辅助函数，失败即测试失败")]

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{Map, Value};
use zcode_protocol::wire::{
    ApprovalId, ApprovalMode, ApprovalReply, AssistantContent, CallId, ClientFrame, ClientId,
    Entry, EntryId, EntryKind, Event, Message, Pending, PendingApproval, PendingStdin, Reply,
    Request, ServerFrame, SessionId, SessionSummary, StdinId, StopReason, ToolProgress, Usage,
    UserContent,
};

/// `Request` 变体总数，须与 [`sample_requests`] 的长度一致。
const REQUEST_VARIANTS: usize = 16;
/// `Reply` 变体总数（含 `Unknown` 兜底），须与 [`sample_replies`] 的长度一致。
const REPLY_VARIANTS: usize = 10;
/// `Event` 变体总数（含 `Unknown` 兜底），须与 [`sample_events`] 的长度一致。
const EVENT_VARIANTS: usize = 19;

/// 穷尽性哨兵：新增 [`Request`] 变体时这里先编译失败。见模块文档"双重穷尽性哨兵"。
#[allow(
    dead_code,
    clippy::match_same_arms,
    reason = "只编译不调用的哨兵；逐行列出每个变体，禁止用 `|` 合并到同一条 arm"
)]
fn _exhaustive_request(value: &Request) {
    match value {
        Request::Ping => {}
        Request::SessionList { .. } => {}
        Request::SessionCreate { .. } => {}
        Request::Subscribe { .. } => {}
        Request::Unsubscribe { .. } => {}
        Request::HistoryFetch { .. } => {}
        Request::Prompt { .. } => {}
        Request::Cancel { .. } => {}
        Request::Compact { .. } => {}
        Request::SetHead { .. } => {}
        Request::SetModel { .. } => {}
        Request::SetTitle { .. } => {}
        Request::SetApprovalMode { .. } => {}
        Request::ApprovalRespond { .. } => {}
        Request::StdinRespond { .. } => {}
        Request::PendingList { .. } => {}
    }
}

/// 穷尽性哨兵：新增 [`Reply`] 变体时这里先编译失败。
#[allow(
    dead_code,
    clippy::match_same_arms,
    reason = "只编译不调用的哨兵；逐行列出每个变体，禁止用 `|` 合并到同一条 arm"
)]
fn _exhaustive_reply(value: &Reply) {
    match value {
        Reply::Ok => {}
        Reply::Pong => {}
        Reply::Sessions { .. } => {}
        Reply::SessionCreated { .. } => {}
        Reply::Subscribed { .. } => {}
        Reply::SessionBusy { .. } => {}
        Reply::History { .. } => {}
        Reply::TurnStarted { .. } => {}
        Reply::Pending { .. } => {}
        Reply::Unknown => {}
    }
}

/// 穷尽性哨兵：新增 [`Event`] 变体时这里先编译失败。
#[allow(
    dead_code,
    clippy::match_same_arms,
    reason = "只编译不调用的哨兵；逐行列出每个变体，禁止用 `|` 合并到同一条 arm"
)]
fn _exhaustive_event(value: &Event) {
    match value {
        Event::TurnStart { .. } => {}
        Event::MessageStart { .. } => {}
        Event::TextDelta { .. } => {}
        Event::ThinkingDelta { .. } => {}
        Event::ToolCallDelta { .. } => {}
        Event::MessageEnd { .. } => {}
        Event::ToolStart { .. } => {}
        Event::ToolProgress { .. } => {}
        Event::ToolEnd { .. } => {}
        Event::ApprovalRequested { .. } => {}
        Event::ApprovalResolved { .. } => {}
        Event::StdinRequested { .. } => {}
        Event::StdinResolved { .. } => {}
        Event::Compacted { .. } => {}
        Event::SessionUpdated { .. } => {}
        Event::TurnEnd { .. } => {}
        Event::Failed { .. } => {}
        Event::Resync { .. } => {}
        Event::Unknown => {}
    }
}

/// 复用的会话摘要样本：字段全部非默认值，最大化字段覆盖。
fn session_summary() -> SessionSummary {
    SessionSummary {
        id: SessionId::from("ses_1"),
        title: Some("协议冒烟会话".to_owned()),
        cwd: "/workspace/zcode".to_owned(),
        model: "claude-sonnet".to_owned(),
        created_ms: 1_700_000_000_000,
        updated_ms: 1_700_000_060_000,
    }
}

/// 复用的助手消息样本：同时覆盖 `AssistantContent::Text` 与 `ToolCall`。
fn assistant_message() -> Message {
    Message::Assistant {
        content: vec![
            AssistantContent::Text {
                text: "已定位问题。".to_owned(),
            },
            AssistantContent::ToolCall {
                id: CallId::from("call_1"),
                name: "bash".to_owned(),
                arguments: r#"{"cmd":"tail -n 20 app.log"}"#.to_owned(),
            },
        ],
        model: Some("claude-sonnet-20260101".to_owned()),
        usage: Usage {
            input: 128,
            output: 64,
            cache_read: 32,
            cache_write: 0,
            reasoning: 16,
        },
        stop_reason: StopReason::ToolUse,
    }
}

/// 复用的条目样本：携带非空 `parent_id`，覆盖分支场景。
fn entry() -> Entry {
    Entry {
        id: EntryId::from("ent_2"),
        parent_id: Some(EntryId::from("ent_1")),
        timestamp_ms: 1_700_000_030_000,
        kind: EntryKind::Message {
            message: assistant_message(),
        },
    }
}

/// 复用的待审批样本。
fn pending_approval() -> PendingApproval {
    PendingApproval {
        request_id: ApprovalId::from("apr_1"),
        call_id: CallId::from("call_1"),
        tool_name: "bash".to_owned(),
        scope: "bash".to_owned(),
        prompt: "允许执行 `tail -n 20 app.log`？".to_owned(),
    }
}

/// 复用的待输入样本：`is_password = true`，覆盖密码类分支。
fn pending_stdin() -> PendingStdin {
    PendingStdin {
        request_id: StdinId::from("stdin_1"),
        call_id: CallId::from("call_2"),
        prompt: "password:".to_owned(),
        is_password: true,
    }
}

/// 复用的待回答项样本：审批与 stdin 各一条，两个 `Vec` 都非空。
fn pending() -> Pending {
    Pending {
        approvals: vec![pending_approval()],
        stdin: vec![pending_stdin()],
    }
}

/// [`Request`] 的确定性样本，顺序与枚举声明一致；长度受 [`REQUEST_VARIANTS`] 断言。
fn sample_requests() -> Vec<Request> {
    vec![
        Request::Ping,
        Request::SessionList {
            cwd: Some("/workspace/zcode".to_owned()),
        },
        Request::SessionCreate {
            cwd: "/workspace/zcode".to_owned(),
            model: "claude-sonnet".to_owned(),
        },
        Request::Subscribe {
            session: SessionId::from("ses_1"),
            client: ClientId::from("cli_1"),
            has_local_history: true,
            takeover: false,
            since: Some(EntryId::from("ent_1")),
        },
        Request::Unsubscribe {
            session: SessionId::from("ses_1"),
        },
        Request::HistoryFetch {
            session: SessionId::from("ses_1"),
            since: Some(EntryId::from("ent_1")),
        },
        Request::Prompt {
            session: SessionId::from("ses_1"),
            content: vec![UserContent::Text {
                text: "帮我看看这段日志".to_owned(),
            }],
        },
        Request::Cancel {
            session: SessionId::from("ses_1"),
        },
        Request::Compact {
            session: SessionId::from("ses_1"),
        },
        Request::SetHead {
            session: SessionId::from("ses_1"),
            entry: EntryId::from("ent_2"),
        },
        Request::SetModel {
            session: SessionId::from("ses_1"),
            model: "claude-opus".to_owned(),
        },
        Request::SetTitle {
            session: SessionId::from("ses_1"),
            title: "日志排查".to_owned(),
        },
        Request::SetApprovalMode {
            session: SessionId::from("ses_1"),
            mode: ApprovalMode::Write,
        },
        Request::ApprovalRespond {
            request_id: ApprovalId::from("apr_1"),
            reply: ApprovalReply::Once,
        },
        Request::StdinRespond {
            request_id: StdinId::from("stdin_1"),
            text: "y".to_owned(),
        },
        Request::PendingList {
            session: SessionId::from("ses_1"),
        },
    ]
}

/// [`Reply`] 的确定性样本，顺序与枚举声明一致；长度受 [`REPLY_VARIANTS`] 断言。
fn sample_replies() -> Vec<Reply> {
    vec![
        Reply::Ok,
        Reply::Pong,
        Reply::Sessions {
            sessions: vec![session_summary()],
        },
        Reply::SessionCreated {
            summary: session_summary(),
            root: Entry {
                id: EntryId::from("ent_1"),
                parent_id: None,
                timestamp_ms: 1_700_000_000_000,
                kind: EntryKind::SessionInit {
                    cwd: "/workspace/zcode".to_owned(),
                    model: "claude-sonnet".to_owned(),
                },
            },
        },
        Reply::Subscribed {
            summary: session_summary(),
            head: EntryId::from("ent_2"),
            entries: vec![entry()],
            pending: pending(),
            turn_active: true,
        },
        Reply::SessionBusy {
            holder: ClientId::from("cli_2"),
        },
        Reply::History {
            entries: vec![entry()],
        },
        Reply::TurnStarted {
            user_entry: EntryId::from("ent_1"),
        },
        Reply::Pending { pending: pending() },
        Reply::Unknown,
    ]
}

/// [`Event`] 的确定性样本，顺序与枚举声明一致；长度受 [`EVENT_VARIANTS`] 断言。
fn sample_events() -> Vec<Event> {
    vec![
        Event::TurnStart {
            session: SessionId::from("ses_1"),
            user_entry: EntryId::from("ent_1"),
        },
        Event::MessageStart {
            session: SessionId::from("ses_1"),
            entry: EntryId::from("ent_2"),
        },
        Event::TextDelta {
            session: SessionId::from("ses_1"),
            entry: EntryId::from("ent_2"),
            index: 0,
            delta: "已定位".to_owned(),
        },
        Event::ThinkingDelta {
            session: SessionId::from("ses_1"),
            entry: EntryId::from("ent_2"),
            index: 0,
            delta: "分析日志中".to_owned(),
        },
        Event::ToolCallDelta {
            session: SessionId::from("ses_1"),
            entry: EntryId::from("ent_2"),
            index: 1,
            call_id: CallId::from("call_1"),
            delta: r#"{"cmd":"#.to_owned(),
        },
        Event::MessageEnd {
            session: SessionId::from("ses_1"),
            entry: EntryId::from("ent_2"),
            message: Box::new(assistant_message()),
            usage: Usage {
                input: 128,
                output: 64,
                cache_read: 32,
                cache_write: 0,
                reasoning: 16,
            },
        },
        Event::ToolStart {
            session: SessionId::from("ses_1"),
            call_id: CallId::from("call_1"),
            name: "bash".to_owned(),
        },
        Event::ToolProgress {
            session: SessionId::from("ses_1"),
            call_id: CallId::from("call_1"),
            progress: ToolProgress::Chunk {
                text: "tail 输出片段".to_owned(),
            },
        },
        Event::ToolEnd {
            session: SessionId::from("ses_1"),
            call_id: CallId::from("call_1"),
            entry: EntryId::from("ent_3"),
            is_error: false,
        },
        Event::ApprovalRequested {
            session: SessionId::from("ses_1"),
            pending: pending_approval(),
        },
        Event::ApprovalResolved {
            session: SessionId::from("ses_1"),
            request_id: ApprovalId::from("apr_1"),
            approved: true,
        },
        Event::StdinRequested {
            session: SessionId::from("ses_1"),
            pending: pending_stdin(),
        },
        Event::StdinResolved {
            session: SessionId::from("ses_1"),
            request_id: StdinId::from("stdin_1"),
            submitted: true,
        },
        Event::Compacted {
            session: SessionId::from("ses_1"),
            entry: EntryId::from("ent_4"),
        },
        Event::SessionUpdated {
            session: SessionId::from("ses_1"),
            summary: session_summary(),
            head: EntryId::from("ent_2"),
        },
        Event::TurnEnd {
            session: SessionId::from("ses_1"),
        },
        Event::Failed {
            session: SessionId::from("ses_1"),
            message: "提供商请求超时".to_owned(),
        },
        Event::Resync {
            session: SessionId::from("ses_1"),
            dropped: 3,
        },
        Event::Unknown,
    ]
}

/// 序列化 `frame`，取其 `type` 标签作为 key 插入 `map`。
///
/// 用标签而非枚举声明顺序做 key：`serde_json::Map` 在本 crate 的 feature 组合下
/// （开 `raw_value`、未开 `preserve_order`）就是 `BTreeMap`，插入顺序不影响输出顺序，
/// key 本身的字典序才是快照文件的实际顺序——这里不需要额外排序。
fn insert_tagged<T: Serialize>(map: &mut Map<String, Value>, frame: T) {
    let value = serde_json::to_value(frame).expect("wire 帧样本必须可序列化");
    let tag = value
        .get("type")
        .and_then(Value::as_str)
        .expect("wire 帧样本必须带 type 标签")
        .to_owned();
    map.insert(tag, value);
}

/// 组装完整快照：`{"request": {...}, "reply": {...}, "event": {...}}`，三个子对象各自
/// 以变体的 `type` 标签为 key。
fn build_schema() -> Value {
    let mut request = Map::new();
    for sample in sample_requests() {
        insert_tagged(&mut request, ClientFrame::Request(sample));
    }

    let mut reply = Map::new();
    for sample in sample_replies() {
        insert_tagged(&mut reply, ServerFrame::Reply(sample));
    }

    let mut event = Map::new();
    for sample in sample_events() {
        insert_tagged(&mut event, ServerFrame::Event(sample));
    }

    let mut root = Map::new();
    root.insert("request".to_owned(), Value::Object(request));
    root.insert("reply".to_owned(), Value::Object(reply));
    root.insert("event".to_owned(), Value::Object(event));
    Value::Object(root)
}

/// 快照文件相对本 crate 根的路径；同时用作断言失败提示里的文件名。
const SNAPSHOT_RELATIVE_PATH: &str = "tests/wire-schema.json";

/// 快照文件的绝对路径。用 `CARGO_MANIFEST_DIR` 而不是相对当前工作目录：
/// nextest 按 crate 并行调度进程，工作目录不保证是 crate 根（见 `rule://rust-testing`
/// "并行安全"：禁止依赖当前工作目录）。
fn snapshot_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(SNAPSHOT_RELATIVE_PATH)
}

/// 变体样本数量必须与穷尽性哨兵旁边声明的常量一致。
///
/// 这是"双重穷尽性哨兵"机制的第二层：新增变体后如果只补了 `_exhaustive_*` 的 match
/// 分支、忘了往 `sample_*` 里加真样本，编译能过，但这里会因为长度不等而失败。
#[test]
fn variant_inventories_match_declared_counts() {
    assert_eq!(
        sample_requests().len(),
        REQUEST_VARIANTS,
        "Request 样本数量与 REQUEST_VARIANTS 不一致：新增/删除变体后，\
         请同步更新 sample_requests() 与这个常量"
    );
    assert_eq!(
        sample_replies().len(),
        REPLY_VARIANTS,
        "Reply 样本数量与 REPLY_VARIANTS 不一致：新增/删除变体后，\
         请同步更新 sample_replies() 与这个常量"
    );
    assert_eq!(
        sample_events().len(),
        EVENT_VARIANTS,
        "Event 样本数量与 EVENT_VARIANTS 不一致：新增/删除变体后，\
         请同步更新 sample_events() 与这个常量"
    );
}

/// 每个样本包进对应帧类型序列化后，必须能原样反序列化回来。
///
/// 这条测试保护的契约与快照测试不同：快照测试盯的是"形状有没有意外改变"，
/// 这条盯的是"当前这份形状本身是不是自洽的"——万一手滑让某个变体的字段用了
/// 不对称的 `serde(skip_serializing)`/`serde(skip)`，快照可能照样稳定，但线上会静默丢字段。
#[test]
fn every_sample_round_trips_through_its_frame() {
    for sample in sample_requests() {
        let frame = ClientFrame::Request(sample.clone());
        let value = serde_json::to_value(&frame).expect("Request 样本必须可序列化");
        let decoded: ClientFrame =
            serde_json::from_value(value).expect("Request 样本序列化结果必须能被回读");
        assert_eq!(
            decoded, frame,
            "Request 样本序列化/反序列化后必须与原值相等"
        );
    }

    for sample in sample_replies() {
        let frame = ServerFrame::Reply(sample.clone());
        let value = serde_json::to_value(&frame).expect("Reply 样本必须可序列化");
        let decoded: ServerFrame =
            serde_json::from_value(value).expect("Reply 样本序列化结果必须能被回读");
        assert_eq!(decoded, frame, "Reply 样本序列化/反序列化后必须与原值相等");
    }

    for sample in sample_events() {
        let frame = ServerFrame::Event(sample.clone());
        let value = serde_json::to_value(&frame).expect("Event 样本必须可序列化");
        let decoded: ServerFrame =
            serde_json::from_value(value).expect("Event 样本序列化结果必须能被回读");
        assert_eq!(decoded, frame, "Event 样本序列化/反序列化后必须与原值相等");
    }
}

/// 核心快照断言：全部样本序列化后必须与提交的 `wire-schema.json` 逐字节一致。
///
/// `ZCODE_UPDATE_WIRE_SCHEMA=1` 时改成刷新模式：写回快照并直接通过，供有意变更时使用。
#[test]
fn wire_schema_matches_committed_snapshot() {
    let schema = build_schema();
    let mut rendered =
        serde_json::to_string_pretty(&schema).expect("schema 必须可序列化为格式化 JSON");
    rendered.push('\n');

    let path = snapshot_path();
    if std::env::var_os("ZCODE_UPDATE_WIRE_SCHEMA").is_some() {
        std::fs::write(&path, &rendered).expect("刷新 wire-schema.json 失败");
        return;
    }

    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    assert_eq!(
        rendered, existing,
        "wire 协议形状与 {SNAPSHOT_RELATIVE_PATH} 快照不一致。\n若为有意变更，重跑 \
         `ZCODE_UPDATE_WIRE_SCHEMA=1 cargo nextest run -p zcode-protocol` 刷新快照，\
         并按 crates/protocol/src/version.rs 的规则判定 bump major 还是 minor。"
    );
}
