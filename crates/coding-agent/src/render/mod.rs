//! headless 客户端：从 [`ClientSession`] 消费 wire 事件，输出到终端，返回退出码。
//!
//! # 本模块允许直接写 stdout/stderr
//!
//! `rule://zcode-architecture`「日志与 CLI 输出边界」的例外条款在这里生效：
//! headless 是一次性命令，跑完就退，不进 TUI、不与协议共享 stdout。因此本模块
//! （以及它的子模块 [`events`] / [`interactive`] / [`cleanup`]）**直接**用
//! `tokio::io::stdout()` / `tokio::io::stderr()` 写终端输出，不走 `tracing`。
//! 例外只覆盖"写给用户看的终端输出"这一类内容；协议往返失败、补拉历史失败这类
//! 内部诊断信息仍然走 `tracing::warn!`（见 [`events::SessionResponder`]）。
//! 不用 `println!`/`eprintln!`：一律 `tokio::io::AsyncWriteExt::write_all` 加手动
//! `flush`，这样 [`events::render_events`] 才能把 sink 换成 `Vec<u8>` 做单元测试。
//!
//! # 两种格式，一个共同原则：stdout 只装"客户端认领的那一份数据"
//!
//! - [`OutputFormat::Text`]：stdout **只有模型的最终文本**，逐个 [`Event::TextDelta`]
//!   边到边写，不缓冲整条消息再一次性吐出。`Event::ThinkingDelta` 只有
//!   `show_thinking` 为真时才追加到 stdout（与 oh-my-pi 的 `printThoughts` 分支
//!   同一处理方式：`packages/coding-agent/src/modes/print-mode.ts:247-249`）。
//!   工具调用、进度、审批/stdin 提示、压缩提示——一律走 stderr。
//!
//!   **这是对 jcode 的修正**：`turn_loops.rs:180-184` 把 `📦 Context compacted`
//!   打到 stdout，`tools.rs:104-140` 把工具摘要也打到 stdout，plain 模式的 stdout
//!   因此不是纯模型输出——`zcode run ... > out.txt` 拿到的文件混进了进度噪音，
//!   下游脚本再解析就得自己过滤。本模块不重复这个错误：stdout 的契约是
//!   "脚本可以放心 `>` 重定向，得到的就是模型说的话"。
//! - [`OutputFormat::Json`]：NDJSON，每个 [`Event`] 一行，首行是一条会话 header
//!   （[`events::SessionHeader`]）。每写完一行立即 flush，保证下游按行消费的
//!   工具（`jq`、`grep`）不会因为缓冲卡住。
//!
//! # broken pipe 不得把整轮弄挂
//!
//! jcode 的 `emit_ndjson_event(...)?` 把 stdout 的 `BrokenPipe` 冒成整轮失败
//! （`src/cli/commands.rs:2878,3007-3018`）：`jcode run --ndjson | head -1` 会让
//! 下游进程一读完就关管道，上游写下一行时收到 `EPIPE`，`?` 直接把它变成
//! `Err`，最终打印 `Error: ...` 并以非零码退出——用户看到的是"命令失败"，
//! 但它其实正常完成了，只是没人要更多输出了。本模块把 `ErrorKind::BrokenPipe`
//! 当成"消费端已经拿到它想要的、正常挂断"处理：遇到就立即停止渲染，返回退出码
//! `0`，不报错（见 [`events::write_line`] / [`events::write_raw`]）。
//!
//! # 退出码
//!
//! - `0`：turn 正常结束。
//! - `1`：turn 以 [`Event::Failed`] 收场——错误文本只写 stderr，不重复写
//!   stdout（jcode 在 `commands.rs:2995-3007` 把同一条错误既编进 `"type":"error"`
//!   JSON 行、又靠 `Err` 冒泡到外层再打一遍，这里不重复它）。
//! - `130`：被取消——本地收到 `Ctrl-C`（`tokio::signal::ctrl_c()`），或者非交互
//!   环境下遇到必须有人应答的 stdin 询问、只能取消整个 turn 兜底
//!   （见 [`events::handle_stdin`] 的文档）。两者都不是 `Err`：它们是正常完成
//!   的运行，只是结果不同（`lib.rs` 模块文档同一约定）。
//!
//! # 审批 / stdin 回环与 `is_interactive`
//!
//! 收到 `Event::ApprovalRequested` / `Event::StdinRequested` 时：
//! - 交互式终端：把提示打到 stderr，读一行 stdin 作答；
//! - 非交互（管道 / CI）：自动拒绝（stdin 则取消整个 turn，协议里没有"拒绝
//!   stdin"这个回复），并在 stderr 说明原因，**绝不无限挂起**。
//!
//! 交互式判据全模块只有 [`interactive::is_interactive`] 一处，别处一律调用它，
//! 不重新发明——对照 jcode 的三处不一致判据（TUI 看 stdin+stdout、外部 auth
//! 看 stdin+stderr、上色看 stdout）。
//!
//! # 输出清理
//!
//! stderr 上的人读文本（工具摘要、进度、审批提示）过 [`cleanup::clean_line`]：
//! 制表符展开、控制字符清洗、按终端宽度截断；宽度探测不到时退回 80 列
//! （见该函数文档的取值理由）。stdout 上的模型原文**不**过这道清理——那是
//! 刻意的：清理会改写字节，而 stdout 的契约恰恰是"原样"。

mod cleanup;
mod events;
mod interactive;

use zcode_protocol::wire::types::{ClientId, SessionId, UserContent};
use zcode_protocol::{Reply, Request};

use crate::host::connect::{ClientSession, ConnectError};

/// headless 输出格式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OutputFormat {
    /// 只输出模型最终文本（流式边到边写），其余一律走 stderr。
    Text,
    /// NDJSON：每个 wire 事件一行，首行是会话 header。
    Json,
}

/// headless 渲染失败。
///
/// 这里只覆盖真正的基础设施故障（连接、协议、终端 IO）；"turn 失败"与"被取消"
/// 是正常完成的运行，用退出码表达，不算这里的错误——见模块文档「退出码」一节。
#[derive(Debug, thiserror::Error)]
pub(crate) enum RenderError {
    /// 与运行时通信失败：握手、请求发送、协议层错误。
    #[error("与运行时通信失败：{0}")]
    Connect(#[from] ConnectError),
    /// [`ClientSession::take_events`] 只能调用一次，本次运行之前已经被取走过。
    #[error("事件通道已被取走，无法再次订阅")]
    EventsTaken,
    /// 运行时对某条请求给出的应答不在预期变体之内，说明协议层出现了不该出现的
    /// 不一致（不是客户端可以自行恢复的状态，不当退出码用）。
    #[error("运行时返回了意料之外的应答：{0:?}")]
    UnexpectedReply(Box<Reply>),
    /// 终端 IO 失败；`ErrorKind::BrokenPipe` 不算——那条路径直接返回退出码 `0`，
    /// 不会走到这里。
    #[error("终端 IO 失败：{0}")]
    Io(#[from] std::io::Error),
}

/// 消费一条会话上的 wire 事件直到 turn 结束，返回退出码。
///
/// `target` / `client` 由调用方（`cli` 层）选好：会话选择是 `--resume` /
/// `--continue` / 新建这些 flag 语义的归属地，本函数不重新实现一遍
/// `SessionList` → 挑一条 → `SessionCreate`（`plans/runtime-boundary` 的既定
/// 裁决，避免 headless 与 TUI 各写一份、第一次改 `--continue` 定义就漂移）。
///
/// 流程：订阅会话 → 处理订阅回应里带回的既有待回答项（`Reply::Subscribed.pending`
/// 必须当场用上，否则重连前就存在的审批/stdin 询问永远没人回应）→ 发起
/// `Request::Prompt` → 把事件循环交给 [`events::render_events`]。
pub(crate) async fn run_headless(
    session: ClientSession,
    target: SessionId,
    client: ClientId,
    prompt: String,
    format: OutputFormat,
    show_thinking: bool,
) -> Result<i32, RenderError> {
    let events_rx = session.take_events().ok_or(RenderError::EventsTaken)?;

    let subscribed = session
        .request(Request::Subscribe {
            session: target.clone(),
            client,
            has_local_history: false,
            takeover: false,
            since: None,
        })
        .await?;

    let (summary, pending) = match subscribed {
        Reply::Subscribed {
            summary, pending, ..
        } => (summary, pending),
        Reply::SessionBusy { holder } => {
            let mut stderr = tokio::io::stderr();
            let message = format!(
                "会话正在被另一个客户端（{holder}）占用；headless 模式默认不接管，\
                 用 --resume 换一条会话，或等它退出后重试。"
            );
            // 这里明知只剩一次写入也走统一的 broken-pipe 容错路径：管道另一端
            // 完全可能已经在我们报错之前就关闭了。
            let _ = events::write_line(&mut stderr, &cleanup::clean_line(&message)).await;
            return Ok(1);
        }
        other => return Err(RenderError::UnexpectedReply(Box::new(other))),
    };

    match session
        .request(Request::Prompt {
            session: target.clone(),
            content: vec![UserContent::Text { text: prompt }],
        })
        .await?
    {
        Reply::TurnStarted { .. } => {}
        other => return Err(RenderError::UnexpectedReply(Box::new(other))),
    }

    let responder = events::SessionResponder {
        session: &session,
        target: target.clone(),
    };
    // 生产路径接真实的 Ctrl-C；测试直接调用 `events::render_events` 时传
    // `std::future::pending()` / `std::future::ready(())`，不需要真信号。
    let cancel: std::pin::Pin<Box<dyn Future<Output = ()>>> = Box::pin(async {
        // `ctrl_c()` 只在信号确实送达时返回 `Err`（罕见的平台故障）；无论如何，
        // 收到这个 future 本身就是"该取消了"的信号，错误细节不影响这个决定。
        let _ = tokio::signal::ctrl_c().await;
    });

    let code = events::render_events(
        events_rx,
        &responder,
        &target,
        format,
        show_thinking,
        summary,
        pending,
        cancel,
        tokio::io::stdout(),
        tokio::io::stderr(),
    )
    .await?;

    if let Err(error) = session.shutdown().await {
        // 已经算出了有意义的退出码，连接收尾失败不应该盖过它——记日志就够。
        tracing::warn!(%error, "关闭与运行时的连接失败");
    }

    Ok(code)
}
