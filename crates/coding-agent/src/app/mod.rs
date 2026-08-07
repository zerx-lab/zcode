//! TUI 交互客户端：事件循环、终端生命周期、按键路由。
//!
//! # 分层
//!
//! | 模块 | 职责 |
//! | --- | --- |
//! | [`error`] | [`AppError`] |
//! | [`ids`] | 协议 id → [`zcode_tui::ComponentId`] 的稳定映射 |
//! | [`transcript`] | transcript 语义模型 + 纯渲染函数 |
//! | [`input`] | 多行输入框：文本、光标、换行 |
//! | [`pending`] | 审批 / stdin 待回答队列 |
//! | [`status`] | 状态行 + spinner |
//! | [`reveal`] | 流式展示节奏（到达 ≠ 展示） |
//! | [`redraw`] | 重绘节流常量与调度决策 |
//! | [`state`] | 把以上几层揉进一份 [`state::AppState`]，事件循环驱动它 |
//!
//! 本文件只做两件事：终端生命周期（进入/退出 raw mode、Windows VT、bracketed
//! paste）与事件循环（`tokio::select!` 合流终端事件、wire 事件、重绘 tick）。
//! 渲染只经 [`zcode_tui::Emitter`]，本文件不直接向终端写 escape 字节。
//!
//! # 范围控制（与任务书一致，不是遗漏）
//!
//! - 不做主题系统、slash 命令自动补全、模型切换弹窗。
//! - 不接 `zcode_tui::job_control`（Unix Ctrl-Z 挂起）：raw mode 下 `ISIG`
//!   已关闭，Ctrl-Z 到达时只是一个普通按键（当前被忽略），不会真的把进程挂起，
//!   所以"不处理"不等于"处理错了"——只是没有 codex 那种"挂起后光标重锚定"的
//!   体验。留待任务书要求的四步之外的下一期。
//! - 不做多客户端会话同步 UI（`Event::SessionUpdated` 直接忽略）。

mod error;
mod ids;
mod input;
mod pending;
mod redraw;
mod reveal;
mod state;
mod status;
mod transcript;

use std::io::{IsTerminal, Stdout};
use std::pin::pin;
use std::time::{Duration, Instant};

use crossterm::event::{
    Event as TermEvent, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
};
use futures_util::StreamExt;
use ratatui::backend::CrosstermBackend;
#[cfg(test)]
use ratatui::backend::{Backend as RatatuiBackend, ClearType, WindowSize};
#[cfg(test)]
use ratatui::buffer::Cell;
#[cfg(test)]
use ratatui::layout::{Position, Size};
#[cfg(test)]
use std::io::Write as _;
use tokio::time::Instant as TokioInstant;
use zcode_protocol::wire::Event;
#[cfg(test)]
use zcode_protocol::wire::types::{ApprovalId, CallId, EntryId, PendingApproval};
use zcode_protocol::wire::{ApprovalReply, ClientId, Entry, Pending, Reply, Request, SessionId};
use zcode_tui::{Component, Emitter, OutputCaps, Terminal};

use crate::host::connect::ClientSession;

pub(crate) use error::AppError;
use state::{AppState, Effect, Focus};

/// 交互式退出的正常退出码：与 shell `SIGINT` 惯例一致（`lib.rs` 模块文档同一约定
/// 用于区分 headless 的「失败」与「被取消」）。连接阶段被用户提前放弃、以及双击
/// Ctrl-C 主动退出都走这个值。
const EXIT_CANCELLED: i32 = 130;
/// 正常处理完成后的退出码。
const EXIT_OK: i32 = 0;

type Backend = CrosstermBackend<Stdout>;

/// 进入 TUI 交互循环，返回退出码。
///
/// `target`/`client` 由 cli 层预先选定/生成（会话选择是 `--continue`/`--resume`
/// 这类 CLI flag 的语义，归 cli 层，见 `local://contract.md` 的裁决）。`initial`
/// 非空时在订阅完成后立即发一条 `Request::Prompt`。
///
/// `show_thinking` 来自 `config.ui.show_thinking`（默认 `false`）。它**必须**传进来：
/// headless 侧一直遵守这个开关，TUI 侧曾经无条件展示思考内容，同一个配置项在两个
/// 客户端行为不一致——真机跑出来的缺陷。
pub(crate) async fn run_tui(
    session: ClientSession,
    target: SessionId,
    client: ClientId,
    initial: Option<String>,
    show_thinking: bool,
) -> Result<i32, AppError> {
    let caps = OutputCaps::probe();
    if !caps.interactive_output || !std::io::stdin().is_terminal() {
        return Err(AppError::NotInteractive);
    }

    // 主题在进入 raw mode **之前**构造：色深与亮暗判定只读环境变量，属于「启动时
    // 判定一次、全程只读」（`plans/tui/README.md` 不变量 5）。
    let theme = build_theme()?;

    let (mut emitter, terminal_guard) = enter_terminal(caps)?;

    // 会话主体单独一层，好让**每一条**退出路径（含 `?` 冒泡与提前 return）都先收起
    // 活跃区再退 raw mode。活跃区里画的是输入框与状态行，它们从没提交进 scrollback；
    // 不收就直接烙在终端上——真机现象是 shell 提示符下方挂着半个圆角框，且边框的 SGR
    // 还开着，后面每一行都染上边框色。
    let result = run_session(
        &session,
        &target,
        &client,
        initial,
        show_thinking,
        theme,
        &mut emitter,
    )
    .await;

    if let Err(err) = emitter.shutdown() {
        tracing::warn!(error = %err, "收起活跃区失败");
    }
    drop(terminal_guard);

    let code = result?;
    if code == EXIT_OK {
        session.shutdown().await?;
    }
    Ok(code)
}

/// `run_tui` 的会话主体：从这里往下任何一条退出路径都会被调用方统一收尾。
async fn run_session(
    session: &ClientSession,
    target: &SessionId,
    client: &ClientId,
    initial: Option<String>,
    show_thinking: bool,
    theme: zcode_tui::Theme,
    emitter: &mut Emitter<Backend>,
) -> Result<i32, AppError> {
    // 第一帧在连接（这里特指 `Subscribe` 握手）之前就画出
    // （`plans/runtime-boundary/implementation.md:86-87`）。
    let mut state = AppState::connecting(target.clone(), client.clone(), show_thinking, theme);
    render_frame(emitter, &state)?;

    let mut term_events = EventStream::new();
    let Some(mut wire_events) = session.take_events() else {
        return Err(AppError::EventsUnavailable);
    };

    let Some((entries, pending, turn_active)) = establish_subscription(
        session,
        target,
        client,
        &mut state,
        emitter,
        &mut term_events,
    )
    .await?
    else {
        return Ok(EXIT_CANCELLED);
    };
    state.seed_subscribed(entries, pending, turn_active);
    render_frame(emitter, &state)?;

    if let Some(text) = initial.filter(|t| !t.trim().is_empty()) {
        state.input_mut().insert(&text);
        submit_composer(session, &mut state).await;
        render_frame(emitter, &state)?;
    }

    run_event_loop(
        session,
        &mut state,
        emitter,
        &mut term_events,
        &mut wire_events,
    )
    .await?;

    Ok(EXIT_OK)
}

/// 构造本会话的主题：色深 + 亮暗 + 符号档位，三者都只看环境变量。
///
/// # 亮暗怎么定
///
/// 只用 `COLORFGBG`（`fg;bg`，`bg < 8` 判暗），失败一律回落暗色——这是上游三层
/// 探测（`oh-my-pi/packages/coding-agent/src/modes/theme/theme.ts:2144-2167`）里
/// **不需要与终端往返**的那一层。剩下两层本仓暂不做：OSC 11 查询要在 raw mode
/// 下发字节并等回包（上游为此配了 DA1 哨兵防挂死），macOS 原生 API 那条只为
/// 修 Zellij 下 OSC 11 透传坏掉的已知路径。两者都得等 `terminal_probe` 那条
/// 通道接进启动流程，不适合在这里现开一条。
///
/// # 符号档位怎么定
///
/// 看 `ZCODE_SYMBOLS`（`unicode` / `nerd` / `ascii`），默认 `unicode`。
/// **不做自动探测**：上游曾按终端身份猜 Nerd Font，后来整体删除了——猜错的代价
/// 是满屏豆腐块，而进程内拿不到任何关于终端字体的可靠信号。
fn build_theme() -> Result<zcode_tui::Theme, AppError> {
    let dark = match std::env::var("COLORFGBG") {
        Ok(value) => value
            .split(';')
            .nth(1)
            .and_then(|bg| bg.trim().parse::<u8>().ok())
            .is_none_or(|bg| bg < 8),
        Err(_) => true,
    };
    let builtin = if dark {
        zcode_tui::BuiltinTheme::Dark
    } else {
        zcode_tui::BuiltinTheme::Light
    };
    let preset = match std::env::var("ZCODE_SYMBOLS").as_deref() {
        Ok("nerd") => zcode_tui::SymbolPreset::Nerd,
        Ok("ascii") => zcode_tui::SymbolPreset::Ascii,
        // 拼错的值按默认档走并留一条日志：把它当错误挡住启动，对一个纯外观开关
        // 来说代价太大。
        Ok(other) if !other.is_empty() && other != "unicode" => {
            tracing::warn!(value = other, "未知的 ZCODE_SYMBOLS 取值，回落 unicode");
            zcode_tui::SymbolPreset::Unicode
        }
        _ => zcode_tui::SymbolPreset::Unicode,
    };
    builtin
        .load(zcode_tui::ColorMode::probe(), preset)
        .map_err(AppError::Theme)
}

/// 单元测试统一用的主题：固定 dark + truecolor + unicode。
///
/// **不走 [`build_theme`]**：那条路径读环境变量，会让断言随开发机的 `COLORFGBG`
/// 与 `ZCODE_SYMBOLS` 漂移，且测试并行时改环境变量互相打架。固定一份让「渲染出
/// 的样式恰好等于某个颜色」这类断言成立。
#[cfg(test)]
pub(crate) fn test_theme() -> zcode_tui::Theme {
    zcode_tui::BuiltinTheme::Dark
        .load(
            zcode_tui::ColorMode::TrueColor,
            zcode_tui::SymbolPreset::Unicode,
        )
        .expect("内置暗色主题必须能加载")
}

/// 建终端：Windows VT 已经在 [`OutputCaps::probe`] 里独立施加过（不变量 4），
/// 这里只管 raw mode、bracketed paste、[`Terminal`]/[`Emitter`] 的构造与 pin。
fn enter_terminal(caps: OutputCaps) -> Result<(Emitter<Backend>, TerminalGuard), AppError> {
    crossterm::terminal::enable_raw_mode()?;
    {
        let mut out = std::io::stdout();
        crossterm::execute!(out, crossterm::event::EnableBracketedPaste)?;
    }
    let guard = TerminalGuard;

    let backend = CrosstermBackend::new(std::io::stdout());
    let terminal = Terminal::new(backend)?;
    let mut emitter = Emitter::new(terminal, caps);
    // 底部活跃区（状态行/待办弹窗/输入框，以及仍在直播的 transcript 块）必须
    // 是 pinned：不 pin 的话它一旦滚出窗口顶部就会被当"冻结快照"提交进
    // scrollback——输入框本身绝不能发生这种事（见 `state::AppState::live_region_height`
    // 与 `crates/tui/src/ledger.rs` 模块文档"L 恒定"一节）。
    emitter.set_pinned(true);
    Ok((emitter, guard))
}

/// 完成 `Subscribe` 握手，处理 `SessionBusy` 的接管询问。`Ok(None)` 表示用户在
/// 这段等待期间放弃（Ctrl-C 或拒绝接管），调用方据此返回 [`EXIT_CANCELLED`]。
async fn establish_subscription(
    session: &ClientSession,
    target: &SessionId,
    client: &ClientId,
    state: &mut AppState,
    emitter: &mut Emitter<Backend>,
    term_events: &mut EventStream,
) -> Result<Option<(Vec<Entry>, Pending, bool)>, AppError> {
    match subscribe_once(session, target, client, false, state, emitter, term_events).await? {
        Some(Reply::Subscribed {
            entries,
            pending,
            turn_active,
            ..
        }) => Ok(Some((entries, pending, turn_active))),
        Some(Reply::SessionBusy { holder }) => {
            state.set_notice(format!("会话正被 {holder} 占用 · 按 y 接管 / 按其它键放弃"));
            if !confirm_takeover(state, emitter, term_events).await? {
                return Err(AppError::SessionBusy {
                    holder: holder.into_string(),
                });
            }
            match subscribe_once(session, target, client, true, state, emitter, term_events).await?
            {
                Some(Reply::Subscribed {
                    entries,
                    pending,
                    turn_active,
                    ..
                }) => Ok(Some((entries, pending, turn_active))),
                Some(_) => Err(AppError::UnexpectedReply),
                None => Ok(None),
            }
        }
        Some(_) => Err(AppError::UnexpectedReply),
        None => Ok(None),
    }
}

/// 订阅完成后的主循环：合流终端事件、wire 事件、重绘 tick，直到用户退出或
/// 终端事件流关闭。
async fn run_event_loop(
    session: &ClientSession,
    state: &mut AppState,
    emitter: &mut Emitter<Backend>,
    term_events: &mut EventStream,
    wire_events: &mut tokio::sync::mpsc::UnboundedReceiver<Event>,
) -> Result<(), AppError> {
    let mut idle_since = Instant::now();
    let mut resize_pending = false;
    let mut last_resize_redraw: Option<Instant> = None;

    loop {
        let animating = state.is_animating();
        let mut wake_at = Instant::now()
            + redraw::next_tick_interval(
                animating,
                Instant::now().saturating_duration_since(idle_since),
            );
        if resize_pending {
            wake_at = wake_at.min(Instant::now() + redraw::RESIZE_DEBOUNCE);
        }
        let mut dirty = false;

        tokio::select! {
            biased;
            maybe_evt = term_events.next() => {
                match maybe_evt {
                    Some(Ok(evt)) => {
                        match evt {
                            TermEvent::Key(key) => {
                                let width = emitter.terminal().last_known_screen_size().width;
                                handle_key(key, state, session, width).await;
                            }
                            TermEvent::Paste(text) => handle_paste(&text, state),
                            TermEvent::Resize(_, _) => {
                                resize_pending = true;
                            }
                            TermEvent::FocusGained | TermEvent::FocusLost | TermEvent::Mouse(_) => {}
                        }
                        idle_since = Instant::now();
                        dirty = true;
                    }
                    Some(Err(err)) => return Err(AppError::from(err)),
                    None => break,
                }
            }
            Some(event) = wire_events.recv() => {
                let effects = state.apply_event(event, Instant::now());
                run_effects(effects, session, state).await;
                idle_since = Instant::now();
                dirty = true;
            }
            () = tokio::time::sleep_until(TokioInstant::from_std(wake_at)) => {
                if resize_pending && redraw::should_redraw_now(last_resize_redraw, Instant::now()) {
                    resize_pending = false;
                    last_resize_redraw = Some(Instant::now());
                    dirty = true;
                } else if state.is_animating() {
                    state.tick(Instant::now());
                    dirty = true;
                }
            }
        }

        if state.should_quit() {
            break;
        }
        if dirty {
            render_frame(emitter, state)?;
        }
    }
    Ok(())
}

/// 一次 `Request::Subscribe` 往返，期间保持独立 redraw tick 并把终端事件路由进
/// 输入框（这样用户在等待连接时敲的文字不会丢）。`Ok(None)` 表示用户在这段等待
/// 期间按 Ctrl-C 主动放弃。
async fn subscribe_once(
    session: &ClientSession,
    target: &SessionId,
    client: &ClientId,
    takeover: bool,
    state: &mut AppState,
    emitter: &mut Emitter<Backend>,
    term_events: &mut EventStream,
) -> Result<Option<Reply>, AppError> {
    let mut fut = pin!(session.request(Request::Subscribe {
        session: target.clone(),
        client: client.clone(),
        has_local_history: false,
        takeover,
        since: None,
    }));
    loop {
        let wake_at =
            Instant::now() + redraw::next_tick_interval(state.is_animating(), Duration::ZERO);
        tokio::select! {
            biased;
            reply = &mut fut => return Ok(Some(reply?)),
            maybe_evt = term_events.next() => {
                match maybe_evt {
                    Some(Ok(evt)) => {
                        if connecting_event_requests_abort(&evt) {
                            return Ok(None);
                        }
                        apply_connecting_event(evt, state);
                    }
                    Some(Err(err)) => return Err(AppError::from(err)),
                    None => return Ok(None),
                }
            }
            () = tokio::time::sleep_until(TokioInstant::from_std(wake_at)) => {
                state.tick(Instant::now());
            }
        }
        render_frame(emitter, state)?;
    }
}

/// `SessionBusy` 之后的 y/n 确认，同样保持 redraw tick。
async fn confirm_takeover(
    state: &mut AppState,
    emitter: &mut Emitter<Backend>,
    term_events: &mut EventStream,
) -> Result<bool, AppError> {
    render_frame(emitter, state)?;
    loop {
        let wake_at =
            Instant::now() + redraw::next_tick_interval(state.is_animating(), Duration::ZERO);
        tokio::select! {
            biased;
            maybe_evt = term_events.next() => {
                match maybe_evt {
                    Some(Ok(TermEvent::Key(key))) if key.kind != KeyEventKind::Release => {
                        return Ok(matches!(key.code, KeyCode::Char('y' | 'Y')));
                    }
                    Some(Ok(_)) => {}
                    Some(Err(err)) => return Err(AppError::from(err)),
                    None => return Ok(false),
                }
            }
            () = tokio::time::sleep_until(TokioInstant::from_std(wake_at)) => {
                state.tick(Instant::now());
            }
        }
        render_frame(emitter, state)?;
    }
}

fn connecting_event_requests_abort(evt: &TermEvent) -> bool {
    matches!(evt, TermEvent::Key(key) if key.kind != KeyEventKind::Release && is_ctrl_c(key))
}

/// 连接阶段的事件处理：只接受文本编辑，不做请求（还没有会话可发）。
fn apply_connecting_event(evt: TermEvent, state: &mut AppState) {
    match evt {
        TermEvent::Key(key) if key.kind != KeyEventKind::Release => match key.code {
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                state.input_mut().insert_char(c);
            }
            KeyCode::Backspace => state.input_mut().backspace(),
            KeyCode::Delete => state.input_mut().delete_forward(),
            KeyCode::Left => state.input_mut().move_left(),
            KeyCode::Right => state.input_mut().move_right(),
            _ => {}
        },
        TermEvent::Paste(text) => state.input_mut().insert(&text),
        _ => {}
    }
}

/// 执行 [`state::AppState::apply_event`] 产生的副作用请求：目前只有历史/待办的
/// 补拉，都走同一条"发出去、把回应合并回状态"的路径。
async fn run_effects(effects: Vec<Effect>, session: &ClientSession, state: &mut AppState) {
    for Effect::Send(request) in effects {
        match session.request(request).await {
            Ok(Reply::History { entries }) => state.merge_history(entries),
            Ok(Reply::Pending { pending }) => {
                state.pending_mut().seed(pending.approvals, pending.stdin);
            }
            Ok(_) => {}
            Err(err) => tracing::warn!(error = %err, "补拉历史/待办队列失败"),
        }
    }
}

/// Ctrl-C：crossterm 在 raw mode 下把它当普通按键送来（`ISIG` 已关闭），不是
/// 信号。
fn is_ctrl_c(key: &KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c' | 'C'))
}

async fn handle_key(key: KeyEvent, state: &mut AppState, session: &ClientSession, width: u16) {
    if key.kind == KeyEventKind::Release {
        return;
    }
    if is_ctrl_c(&key) {
        if state.quit_armed() {
            state.request_quit();
        } else {
            state.arm_quit_confirmation();
        }
        return;
    }
    state.disarm_quit_confirmation();

    if key.code == KeyCode::Esc {
        // 输入框有内容先清空；已空且有 turn 在跑才发取消——两条各自独立，
        // 不会因为"清空"顺带把取消也发了。
        if !state.clear_input_if_any() && state.turn_active() {
            send_and_log(session, state.cancel_request(), "取消当前 turn").await;
        }
        return;
    }

    match state.focus() {
        Focus::Approval => handle_approval_key(key, state, session).await,
        Focus::Stdin => handle_stdin_key(key, state, session).await,
        Focus::Composer => handle_composer_key(key, state, session, width).await,
    }
}

async fn handle_approval_key(key: KeyEvent, state: &mut AppState, session: &ClientSession) {
    let reply = match key.code {
        KeyCode::Char('y' | 'Y') => Some(ApprovalReply::Once),
        KeyCode::Char('a' | 'A') => Some(ApprovalReply::Always),
        KeyCode::Char('n' | 'N' | 'r' | 'R') => Some(ApprovalReply::Reject),
        _ => None,
    };
    let Some(reply) = reply else {
        return;
    };
    if let Some(request) = state.respond_front_approval(reply) {
        send_and_log(session, request, "回复审批请求").await;
    }
}

async fn handle_stdin_key(key: KeyEvent, state: &mut AppState, session: &ClientSession) {
    match key.code {
        KeyCode::Enter => {
            if let Some(request) = state.submit_front_stdin() {
                send_and_log(session, request, "提交 stdin 输入").await;
            }
        }
        KeyCode::Backspace => state.pending_mut().stdin_backspace(),
        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.pending_mut().stdin_push_char(c);
        }
        _ => {}
    }
}

async fn handle_composer_key(
    key: KeyEvent,
    state: &mut AppState,
    session: &ClientSession,
    width: u16,
) {
    let width = usize::from(width.max(1));
    match key.code {
        // Alt+Enter 插入字面换行；多数终端不区分 Shift+Enter 与 Enter（缺 Kitty
        // 协议时两者送来的是同一个按键事件），Alt+Enter 是更可移植的约定。
        KeyCode::Enter if key.modifiers.contains(KeyModifiers::ALT) => {
            state.input_mut().insert_char('\n');
        }
        KeyCode::Enter => submit_composer(session, state).await,
        KeyCode::Backspace => state.input_mut().backspace(),
        KeyCode::Delete => state.input_mut().delete_forward(),
        KeyCode::Left => state.input_mut().move_left(),
        KeyCode::Right => state.input_mut().move_right(),
        KeyCode::Up => state.input_mut().move_up(width),
        KeyCode::Down => state.input_mut().move_down(width),
        KeyCode::Home => state.input_mut().move_line_start(width),
        KeyCode::End => state.input_mut().move_line_end(width),
        KeyCode::Tab => state.input_mut().insert_char('\t'),
        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.input_mut().insert_char(c);
        }
        _ => {}
    }
}

async fn submit_composer(session: &ClientSession, state: &mut AppState) {
    let Some(content) = state.take_submission() else {
        return;
    };
    let request = Request::Prompt {
        session: state.session_id().clone(),
        content: content.clone(),
    };
    match session.request(request).await {
        Ok(Reply::TurnStarted { user_entry }) => state.on_turn_started(user_entry, content),
        Ok(_) => tracing::warn!("Request::Prompt 收到意料之外的回应"),
        Err(err) => tracing::warn!(error = %err, "发送消息失败"),
    }
}

fn handle_paste(text: &str, state: &mut AppState) {
    match state.focus() {
        Focus::Composer => state.input_mut().insert(text),
        Focus::Stdin => {
            for c in text.chars().filter(|c| *c != '\n' && *c != '\r') {
                state.pending_mut().stdin_push_char(c);
            }
        }
        Focus::Approval => {}
    }
}

async fn send_and_log(session: &ClientSession, request: Request, what: &'static str) {
    if let Err(err) = session.request(request).await {
        tracing::warn!(error = %err, what, "请求失败");
    }
}

/// 组装一帧并交给 [`Emitter::render`]——本 crate 里渲染的唯一入口，除此之外
/// 不直接向终端写 escape 字节。
fn render_frame(emitter: &mut Emitter<Backend>, state: &AppState) -> Result<(), AppError> {
    let components = state.build_components();
    let refs: Vec<&dyn Component> = components.iter().map(AsRef::as_ref).collect();
    emitter.render(&refs, AppState::min_viewport_height())?;
    Ok(())
}

/// 退出前恢复终端状态：关闭 bracketed paste、退出 raw mode。收集不到错误就
/// 各自 `tracing::warn!` 后继续——退出路径要尽力把终端还原，一步失败不能连累
/// 后面几步不执行。
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let mut out = std::io::stdout();
        if let Err(err) = crossterm::execute!(out, crossterm::event::DisableBracketedPaste) {
            tracing::warn!(error = %err, "关闭 bracketed paste 失败");
        }
        if let Err(err) = crossterm::terminal::disable_raw_mode() {
            tracing::warn!(error = %err, "退出 raw mode 失败");
        }
    }
}

/// 端到端烟测：驱动 [`state::AppState`] 走一遍"输入 → 发送 → 助手流式回复 →
/// 工具调用 → 审批弹窗"的真实场景，全程经过真正的 [`zcode_tui::Emitter`] /
/// [`Terminal`]，用 `vt100` 解析实际发出的字节并断言整屏网格——不是文本源码
/// 断言，是"终端真的会显示成什么样"。
///
/// 后端抄的是 `crates/tui/tests/shadow.rs` 的 `Screen` 模式（同一个已验证的
/// `Backend` + `Write` 组合），本文件不重新发明一份。
#[cfg(test)]
mod tests {
    use super::*;

    struct Screen {
        parser: vt100::Parser,
        size: Size,
    }

    impl Screen {
        fn new(width: u16, height: u16) -> Self {
            Self {
                parser: vt100::Parser::new(height, width, 1024),
                size: Size::new(width, height),
            }
        }

        /// 可见屏幕拼成一整块文本，便于 `contains` 断言。
        fn screen_text(&self) -> String {
            self.parser
                .screen()
                .rows(0, self.size.width)
                .collect::<Vec<_>>()
                .join("\n")
        }

        /// scrollback + 可见屏的联合文本。
        ///
        /// `screen_text()` 只看得见当前屏，而 transcript 的设计就是让已定稿内容滚进
        /// 终端**原生 scrollback**——只断言可见屏，等于默认「滚出去的东西不用管」，
        /// 而「滚出去的东西凭空少了几行」恰恰是这一层最容易出的缺陷。
        fn scrollback_text(&mut self) -> String {
            let mut out = String::new();
            // 从最老的一屏往回读。步长取整屏高度，逐屏拼接；vt100 的 scrollback 偏移
            // 以行计，超出实际历史长度时它自己 clamp，多读几屏只是拿到重复内容。
            let step = usize::from(self.size.height.max(1));
            let mut offset = step * 8;
            while offset > 0 {
                self.parser.screen_mut().set_scrollback(offset);
                out.push_str(
                    &self
                        .parser
                        .screen()
                        .rows(0, self.size.width)
                        .collect::<Vec<_>>()
                        .join("\n"),
                );
                out.push('\n');
                offset = offset.saturating_sub(step);
            }
            self.parser.screen_mut().set_scrollback(0);
            out.push_str(&self.screen_text());
            out
        }
    }

    impl std::io::Write for Screen {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.parser.process(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl RatatuiBackend for Screen {
        type Error = std::io::Error;

        fn draw<'a, I>(&mut self, content: I) -> std::io::Result<()>
        where
            I: Iterator<Item = (u16, u16, &'a Cell)>,
        {
            for (x, y, cell) in content {
                write!(self, "\x1b[{};{}H{}", y + 1, x + 1, cell.symbol())?;
            }
            Ok(())
        }

        fn hide_cursor(&mut self) -> std::io::Result<()> {
            write!(self, "\x1b[?25l")
        }

        fn show_cursor(&mut self) -> std::io::Result<()> {
            write!(self, "\x1b[?25h")
        }

        fn get_cursor_position(&mut self) -> std::io::Result<Position> {
            let (row, col) = self.parser.screen().cursor_position();
            Ok(Position::new(col, row))
        }

        fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> std::io::Result<()> {
            let position = position.into();
            write!(self, "\x1b[{};{}H", position.y + 1, position.x + 1)
        }

        fn clear(&mut self) -> std::io::Result<()> {
            write!(self, "\x1b[2J")
        }

        fn clear_region(&mut self, clear_type: ClearType) -> std::io::Result<()> {
            let code = match clear_type {
                ClearType::All => "2J",
                ClearType::AfterCursor => "0J",
                ClearType::BeforeCursor => "1J",
                ClearType::CurrentLine => "2K",
                ClearType::UntilNewLine => "0K",
            };
            write!(self, "\x1b[{code}")
        }

        fn size(&self) -> std::io::Result<Size> {
            Ok(self.size)
        }

        fn window_size(&mut self) -> std::io::Result<WindowSize> {
            Ok(WindowSize {
                columns_rows: self.size,
                pixels: Size::new(0, 0),
            })
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }

        fn scroll_region_up(
            &mut self,
            region: std::ops::Range<u16>,
            line_count: u16,
        ) -> std::io::Result<()> {
            write!(
                self,
                "\x1b[{};{}r\x1b[{line_count}S\x1b[r",
                region.start + 1,
                region.end
            )
        }

        fn scroll_region_down(
            &mut self,
            region: std::ops::Range<u16>,
            line_count: u16,
        ) -> std::io::Result<()> {
            write!(
                self,
                "\x1b[{};{}r\x1b[{line_count}T\x1b[r",
                region.start + 1,
                region.end
            )
        }
    }

    fn new_emitter(width: u16, height: u16) -> Emitter<Screen> {
        let backend = Screen::new(width, height);
        let terminal =
            Terminal::with_screen_size(backend, Size::new(width, height), Position { x: 0, y: 0 });
        Emitter::new(
            terminal,
            OutputCaps {
                interactive_output: true,
                scrollback_purge: true,
            },
        )
    }

    fn render(emitter: &mut Emitter<Screen>, state: &AppState) {
        let components = state.build_components();
        let refs: Vec<&dyn Component> = components.iter().map(AsRef::as_ref).collect();
        emitter
            .render(&refs, AppState::min_viewport_height())
            .expect("渲染到内存 VT100 后端不应失败");
    }

    /// 完整场景：输入 → 提交 → 助手流式回复 → 工具调用 → 审批弹窗，全程经过
    /// 真实渲染管线，逐步用 `vt100` 断言屏幕上出现了预期内容。
    #[test]
    // 端到端场景测试刻意保持一条连贯叙事（输入→提交→流式→工具→审批→结算），
    // 拆成多个小测试会丢失"整条链路衔接处不出错"这个断言本身要保护的东西。
    #[allow(clippy::too_many_lines)]
    fn full_conversation_scenario_renders_expected_text() {
        let session = SessionId::from("s1");
        // 展示思考：本场景要断言思考行确实出现，因此显式打开。
        let mut state =
            AppState::connecting(session.clone(), ClientId::from("c1"), true, test_theme());
        // 40 行而非 24：气泡上下留白、工具卡片与输入框的边框各多占 2 行，整段
        // 会话在 24 行里放不下，头部会先滚进 scrollback，而 `screen_text()` 只
        // 看得见当前屏。终端高度不是被测契约，把窗口开够即可。
        let mut emitter = new_emitter(80, 40);
        emitter.set_pinned(true);

        // 连接阶段：第一帧在 Subscribe 完成前就画出（任务第 8 条）。
        render(&mut emitter, &state);
        assert!(
            emitter.terminal().last_known_screen_size().width > 0,
            "第一帧应该已经建立好 viewport"
        );

        state.seed_subscribed(vec![], Pending::default(), false);
        state.input_mut().insert("fix the bug");
        render(&mut emitter, &state);
        assert!(
            emitter
                .terminal()
                .backend()
                .screen_text()
                .contains("fix the bug"),
            "输入框内容应该出现在屏幕上"
        );

        // 提交：模拟 `Reply::TurnStarted` 成功返回后的本地状态更新。
        let content = state.take_submission().expect("刚输入的文本不应为空");
        state.on_turn_started(EntryId::from("u1"), content);
        render(&mut emitter, &state);
        assert!(
            emitter
                .terminal()
                .backend()
                .screen_text()
                .contains("fix the bug"),
            "已提交的用户消息应该可见"
        );
        assert!(state.turn_active(), "提交后应处于 turn 进行中状态");

        // 助手开始流式回复。
        let now = Instant::now();
        state.apply_event(
            Event::MessageStart {
                session: session.clone(),
                entry: EntryId::from("a1"),
            },
            now,
        );
        state.apply_event(
            Event::TextDelta {
                session: session.clone(),
                entry: EntryId::from("a1"),
                index: 0,
                delta: "Sure, checking now.".to_owned(),
            },
            now,
        );
        // 展示节奏需要真实流逝的时间才会推进；连续 tick 足够的虚拟时间让
        // backlog（约 20 字符）在 180 cps 的稳态速率下完全展示出来。
        let mut t = now;
        for _ in 0..10 {
            t += Duration::from_millis(50);
            state.tick(t);
        }
        render(&mut emitter, &state);
        assert!(
            emitter
                .terminal()
                .backend()
                .screen_text()
                .contains("checking now"),
            "流式展示节奏应该已经吐出全部 backlog；screen=\n{}",
            emitter.terminal().backend().screen_text()
        );

        // 工具调用：开始、进度、结束。
        state.apply_event(
            Event::ToolStart {
                session: session.clone(),
                call_id: CallId::from("call_1"),
                name: "bash".to_owned(),
            },
            t,
        );
        state.apply_event(
            Event::ToolProgress {
                session: session.clone(),
                call_id: CallId::from("call_1"),
                progress: zcode_protocol::wire::types::ToolProgress::Chunk {
                    text: "no bug found".to_owned(),
                },
            },
            t,
        );
        state.apply_event(
            Event::ToolEnd {
                session: session.clone(),
                call_id: CallId::from("call_1"),
                entry: EntryId::from("tool-e1"),
                is_error: false,
            },
            t,
        );
        render(&mut emitter, &state);
        let screen = emitter.terminal().backend().screen_text();
        assert!(screen.contains("bash"), "工具名应可见；screen=\n{screen}");
        assert!(
            screen.contains("no bug found"),
            "工具输出应可见；screen=\n{screen}"
        );

        // 审批弹窗：请求到达后应立刻可见，带工具名与操作提示。
        state.apply_event(
            Event::ApprovalRequested {
                session: session.clone(),
                pending: PendingApproval {
                    request_id: ApprovalId::from("req1"),
                    call_id: CallId::from("call_2"),
                    tool_name: "write".to_owned(),
                    scope: "write".to_owned(),
                    prompt: "写入 src/main.rs".to_owned(),
                },
            },
            t,
        );
        render(&mut emitter, &state);
        let screen = emitter.terminal().backend().screen_text();
        assert!(
            screen.contains("审批请求"),
            "应显示审批标题；screen=\n{screen}"
        );
        assert!(
            screen.contains("write"),
            "应显示待审批的工具名；screen=\n{screen}"
        );
        assert_eq!(
            state.focus(),
            Focus::Approval,
            "有待审批项时应把焦点切到审批弹窗"
        );

        // 审批结算：resolved 事件到达后弹窗应消失，焦点还给输入框。
        state.apply_event(
            Event::ApprovalResolved {
                session: session.clone(),
                request_id: ApprovalId::from("req1"),
                approved: true,
            },
            t,
        );
        assert_eq!(state.focus(), Focus::Composer, "审批结算后焦点应还给输入框");
        render(&mut emitter, &state);
    }

    /// **真机回归**：24 行终端上连发几轮，每一条消息都必须还在（屏幕或 scrollback）。
    ///
    /// 现象：先发一句 `hello`，模型回复，再发第二句——第一句**凭空消失**。
    ///
    /// 根因不是渲染函数，是 viewport 高度：那时由 `AppState` 自己数活跃区行数再传给
    /// `Emitter::render`。加了气泡上下留白与卡片/输入框边框之后，这个估算必然偏小；
    /// viewport 装不下活跃内容时，顶部几行既没被提交进 scrollback、也没画进窗口，
    /// 就此丢失，且不会自愈。修法是让高度只由 `compose` 的 boundary 得出
    /// （见 `zcode_tui::Emitter::render` 的文档）。
    ///
    /// 这个测试盯的是**结果**而非实现：只要任何一条已发出的消息在终端上找不到，
    /// 它就红。所以后续无论怎么改布局，它都还有效。
    #[test]
    fn every_message_survives_multiple_turns_on_a_short_terminal() {
        let session = SessionId::from("s1");
        let mut state =
            AppState::connecting(session.clone(), ClientId::from("c1"), false, test_theme());
        // 24 行是最常见的终端高度，也是内容最容易被挤出 viewport 的那一档。
        let mut emitter = new_emitter(80, 24);
        emitter.set_pinned(true);
        state.seed_subscribed(vec![], Pending::default(), false);

        let prompts = ["hello", "你会什么", "第三轮问题"];
        let replies = [
            "Hello — what would you like to work on in this repo?",
            "我可以读写文件、跑命令、查代码。",
            "第三轮回答。",
        ];
        let mut now = Instant::now();

        for (turn, (prompt, reply)) in prompts.iter().zip(replies).enumerate() {
            state.input_mut().insert(prompt);
            let content = state.take_submission().expect("刚输入的文本不应为空");
            state.on_turn_started(EntryId::from(format!("u{turn}")), content);
            render(&mut emitter, &state);

            let entry = EntryId::from(format!("a{turn}"));
            state.apply_event(
                Event::MessageStart {
                    session: session.clone(),
                    entry: entry.clone(),
                },
                now,
            );
            state.apply_event(
                Event::TextDelta {
                    session: session.clone(),
                    entry: entry.clone(),
                    index: 0,
                    delta: reply.to_owned(),
                },
                now,
            );
            // 把展示节奏推到底，让整条回复都吐出来。
            for _ in 0..20 {
                now += Duration::from_millis(50);
                state.tick(now);
            }
            render(&mut emitter, &state);
        }

        let seen = emitter.terminal_mut().backend_mut().scrollback_text();
        for text in prompts.iter().chain(replies.iter()) {
            assert!(
                seen.contains(text),
                "消息「{text}」在屏幕与 scrollback 里都找不到，说明渲染把它吞了\n{seen}"
            );
        }
    }

    /// 取消确认：连续两次 Ctrl-C 才真正退出，中途任意其它按键会解除确认态。
    #[test]
    fn quit_confirmation_requires_two_presses_and_resets_on_other_activity() {
        let mut state = AppState::connecting(
            SessionId::from("s1"),
            ClientId::from("c1"),
            false,
            test_theme(),
        );
        assert!(!state.should_quit());
        state.arm_quit_confirmation();
        assert!(state.quit_armed());
        state.disarm_quit_confirmation();
        assert!(!state.quit_armed());
        assert!(!state.should_quit());
        state.arm_quit_confirmation();
        state.request_quit();
        assert!(state.should_quit());
    }
}
