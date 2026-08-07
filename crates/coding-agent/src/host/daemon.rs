//! daemon 生命周期编排：拿锁 → 回收陈旧端点 → 绑定 → 写注册文件 → 宣布就绪 → accept 循环 →
//! 自查退位。
//!
//! # 顺序不可交换
//!
//! `SingleInstanceLock::acquire` → `reap_stale_endpoint` → `Listener::bind` →
//! `Registration::write_atomic` → `signal_ready`。**拿锁必须先于一切副作用**：
//! [`zcode_utils::daemon`] 模块文档（`crates/utils/src/daemon.rs:17-31`）把这一步称为
//! "陈旧端点回收是双条件的"里的第一条不变式——回收判定依赖"能拿到独占锁 ⇒ 没有别的
//! daemon 正在起来"，这条不变式只在锁先于其它副作用被拿到时成立。
//!
//! # 被抢注即自杀
//!
//! daemon 持有单实例锁不代表它是注册文件里那一份——另一个进程可能在本进程锁还没释放前
//! 就已经在写新的注册文件（例如管理员手工删了旧锁文件后重新拉起）。所以运行期还要定期
//! 重读注册文件自查 `id`：不是自己就说明被后来者顶替，此时自己已经无人可达（客户端只认
//! 文件里的端点），必须主动退出而不是继续占着端点当孤儿
//! （opencode `packages/cli/src/services/daemon.ts:174-179`）。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use zcode_utils::daemon::{
    DaemonError as PrimitiveError, READY_ENDPOINT_ENV, Registration, SingleInstanceLock,
    reap_stale_endpoint, signal_ready,
};
use zcode_utils::transport::Listener;

use crate::config::Config;

use super::{Host, HostDeps};

/// 注册文件名，挂在 `config.daemon.runtime_dir` 下。`connect.rs` 用同一个常量定位它。
pub(crate) const REGISTRATION_FILE_NAME: &str = "daemon.json";
/// 监听端点文件名（Unix socket 路径 / Windows named pipe 的路径形式名字）。
const ENDPOINT_FILE_NAME: &str = "daemon.sock";
/// 单实例锁文件名。
const LOCK_FILE_NAME: &str = "daemon.lock";

/// 自查轮询间隔：定期重读注册文件确认自己没有被后来者顶替。
///
/// opencode 用 10s（`packages/cli/src/services/daemon.ts:177`
/// `Effect.repeat(Schedule.spaced("10 seconds"))`）。这里取一半：多个客户端几乎同时启动时
/// 竞态覆盖注册文件的窗口更常见（本仓没有 opencode 那样的 spawn 互斥锁前置步骤，抢注检测
/// 完全靠这个轮询兜底），更快发现被顶替能让旧进程更快让出端点，减少两个僵持候选互相
/// 重试连接的时间。单次检查只是 stat + 读一个几百字节的 JSON 文件，5s 间隔的 I/O 成本可
/// 忽略不计。
const SELF_CHECK_INTERVAL: Duration = Duration::from_secs(5);

/// daemon 主循环失败。
#[derive(Debug, thiserror::Error)]
pub(crate) enum DaemonServeError {
    /// 单实例锁已被另一个 daemon 持有。
    #[error("另一个 daemon 实例已经在运行（锁文件 {path}）")]
    AlreadyRunning {
        /// 锁文件路径。
        path: PathBuf,
    },
    /// 绑定监听端点失败。
    #[error("绑定端点 {path} 失败：{source}")]
    Bind {
        /// 端点路径。
        path: PathBuf,
        /// 底层错误。
        #[source]
        source: std::io::Error,
    },
    /// 接受连接失败——监听器已损坏，daemon 必须退出。
    #[error("接受客户端连接失败：{0}")]
    Accept(#[source] std::io::Error),
    /// daemon 原语（锁、回收、注册、就绪握手）失败。
    #[error(transparent)]
    Primitive(#[from] PrimitiveError),
}

/// daemon 主循环：把原语按不可交换的顺序编排成一个完整生命周期。
///
/// `deps.secret` 会被**丢弃并重建**：真 daemon 的密钥来源是
/// [`Registration::create`]（内部生成新密钥），写进注册文件的必须和实际用于握手校验的是
/// 同一份，因此由本函数统一生成后回填进 `deps`，调用方不需要（也不应该）自己传一份进来。
///
/// 端点目录按 `deps.workspace.root()` 分桶（[`super::scoped_runtime_dir`]），**一个工作区
/// 一个 daemon**。桶必须由 `deps` 里的工作区算出、而不是另传一个参数：算错桶的后果是
/// daemon 在 A 桶里注册、客户端在 B 桶里找不到它，然后无限重复 spawn。
pub(crate) async fn serve(config: &Config, deps: HostDeps) -> Result<(), DaemonServeError> {
    let runtime_dir = super::scoped_runtime_dir(&config.daemon.runtime_dir, deps.workspace.root());
    let lock_path = runtime_dir.join(LOCK_FILE_NAME);
    let endpoint_path = runtime_dir.join(ENDPOINT_FILE_NAME);
    let registration_path = runtime_dir.join(REGISTRATION_FILE_NAME);
    std::fs::create_dir_all(&runtime_dir).map_err(|source| DaemonServeError::Bind {
        path: runtime_dir.clone(),
        source,
    })?;

    // 拿锁必须先于一切副作用：见模块文档。
    let lock = SingleInstanceLock::acquire(&lock_path)?.ok_or_else(|| {
        DaemonServeError::AlreadyRunning {
            path: lock_path.clone(),
        }
    })?;

    reap_stale_endpoint(&lock, &endpoint_path).await?;

    let mut listener = Listener::bind(&endpoint_path).map_err(|source| DaemonServeError::Bind {
        path: endpoint_path.clone(),
        source,
    })?;

    let registration = Registration::create(endpoint_path.clone(), env!("CARGO_PKG_VERSION"))?;
    let outcome = run_registered(&mut listener, &registration, &registration_path, deps).await;
    // 无论主循环怎么结束都尝试清理：`remove_if_mine` 只删自己那份，被抢注的情况下已经
    // 不是自己的注册文件，此调用会安全地什么都不做。
    let _ = registration.remove_if_mine(&registration_path);
    // 锁必须存活到这里：它绑定在 `SingleInstanceLock` 持有的文件对象上，作用域结束时
    // 随对象一起被内核释放，提前 drop 会让互斥窗口提前结束。
    drop(lock);
    outcome
}

/// 写注册文件、宣布就绪、跑 accept 循环，直到自查发现被抢注或 accept 致命失败。
///
/// accept 循环与自查循环各自是**一个完整的、不被中途打断的内层循环**，只在最外层用
/// `select!` 二选一，而不是每次自查 tick 都去跟单次 `listener.accept()` 抢跑。
///
/// 这不是风格偏好：本仓 Windows 版 `Listener::accept(&mut self)`
/// （`crates/utils/src/transport/windows.rs:179-189`）在**第一次 poll、任何 `.await`
/// 之前**就会 `self.idle.take()`；若这次 `accept()` 调用被 `select!` 因为另一个分支
/// 先就绪而中途丢弃，`self.idle` 就此变成 `None`，下一次 `listener.accept()` 会立刻报
/// `BrokenPipe`（`listener 没有空闲 pipe 实例`）。把"每 tick 都跟 accept 抢跑"
/// 换成"自查整段跑在自己的内层循环里，只有真正需要退出时才让最外层 `select!` 解出"，
/// 这样 accept 循环内部永远是自己独占轮询、一路跑到底（要么真收到一个连接，要么
/// 致命报错），不会被无关的周期性 tick 打断。等到自查那一路真正解出（被抢注/读失败）
/// 时，`serve()` 本来就要整体退出，`listener` 的内部状态是否还完好已经不重要。
async fn run_registered(
    listener: &mut Listener,
    registration: &Registration,
    registration_path: &Path,
    mut deps: HostDeps,
) -> Result<(), DaemonServeError> {
    registration.write_atomic(registration_path)?;
    deps.secret = registration.secret.clone();

    // 只有真被 spawn 出来（父进程在等就绪）时才有这个环境变量；`zcode serve` 被用户手动
    // 直接执行时没有父进程在等，跳过握手本身就是正确行为。
    if let Ok(ready_value) = std::env::var(READY_ENDPOINT_ENV) {
        signal_ready(&ready_value).await?;
    }

    let host = Host::new(deps);

    tokio::select! {
        result = accept_forever(listener, &host) => result,
        () = self_check_forever(registration_path, &registration.id) => Ok(()),
    }
}

/// accept 循环本体：一路跑到底，绝不与任何其它 future 在同一个 `select!` 里竞争单次
/// `accept()` 调用（理由见 [`run_registered`] 文档）。
async fn accept_forever(listener: &mut Listener, host: &Arc<Host>) -> Result<(), DaemonServeError> {
    loop {
        let stream = listener.accept().await.map_err(DaemonServeError::Accept)?;
        let host = Arc::clone(host);
        tokio::spawn(async move {
            if let Err(err) = host.handle_client(stream).await {
                tracing::warn!(error = %err, "客户端连接处理失败");
            }
        });
    }
}

/// 自查循环本体：只有确认被抢注（或读注册文件失败）时才返回，把"退出"这一个事实
/// 交给外层 `select!`；期间的每一次正常 tick 都在这个 `async fn` 内部消化掉，不会
/// 让外层 `select!` 因为一次无关紧要的 tick 就去打断 accept 循环。
async fn self_check_forever(registration_path: &Path, expected_id: &str) {
    let mut interval = tokio::time::interval(SELF_CHECK_INTERVAL);
    loop {
        interval.tick().await;
        match Registration::read(registration_path) {
            Ok(Some(current)) if current.id == expected_id => {}
            Ok(_) => {
                tracing::warn!("注册文件已被后来者覆盖，daemon 自行退出");
                return;
            }
            Err(source) => {
                tracing::warn!(error = %source, "自查注册文件失败，daemon 自行退出");
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use zcode_protocol::version::{ClientAuth, ClientHello, Nonce as ProtoNonce, Proof};
    use zcode_protocol::wire::{ClientFrame, Reply, Request, ServerFrame};
    use zcode_protocol::{Envelope, FrameDecoder, FrameError, Hello, IdGen, encode};
    use zcode_utils::daemon::{Domain, Nonce as DaemonNonce, proof, verify_proof};
    use zcode_utils::transport::Stream;

    use super::*;

    /// 端到端跑一次 `serve()`：绑定成功、客户端能读注册文件连上并完成握手 + 一次
    /// Ping/Pong，随后覆盖注册文件模拟"被后来者抢注"，确认 `serve()` 在自查轮询内
    /// 自行退出（不是被外部杀掉）。
    #[tokio::test]
    async fn serve_binds_serves_one_client_then_self_evicts() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = test_config(dir.path().to_path_buf(), true);
        let deps = test_host_deps(
            &config,
            zcode_utils::daemon::Secret::generate().expect("secret"),
        );

        // 注册文件落在**按工作区分桶**后的子目录里，不是 `runtime_dir` 本身。
        let registration_path =
            crate::host::scoped_runtime_dir(&config.daemon.runtime_dir, deps.workspace.root())
                .join(REGISTRATION_FILE_NAME);
        let serve_config = test_config(dir.path().to_path_buf(), true);
        let mut serve_handle = tokio::spawn(async move { serve(&serve_config, deps).await });

        // 等注册文件出现——`serve()` 内部顺序保证 bind 在 write_atomic 之前完成。
        let registration = tokio::select! {
            registration = wait_for_registration(&registration_path) => registration,
            result = &mut serve_handle => panic!("serve() 提前结束：{result:?}"),
        };

        let stream = Stream::connect(&registration.endpoint)
            .await
            .expect("客户端应当能连上刚绑定的端点");
        let reply = handshake_and_ping(stream, &registration.secret).await;
        assert!(matches!(reply, Reply::Pong));

        // 模拟被后来者抢注：直接覆盖注册文件，id 不同。
        let impostor = Registration::create(registration.endpoint.clone(), "impostor")
            .expect("构造覆盖用注册信息");
        impostor
            .write_atomic(&registration_path)
            .expect("覆盖注册文件");

        let outcome = tokio::time::timeout(Duration::from_secs(10), serve_handle)
            .await
            .expect("serve() 应当在自查轮询内自行退出，不应该一直挂着")
            .expect("serve 任务不应 panic");
        assert!(
            outcome.is_ok(),
            "被抢注后 serve() 应当返回 Ok(()) 而不是报错：{outcome:?}"
        );
    }

    /// 单实例锁已被持有时，第二次 `serve()` 必须立刻报 `AlreadyRunning`，不阻塞等待。
    #[tokio::test]
    async fn serve_reports_already_running_when_lock_is_held() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = test_config(dir.path().to_path_buf(), true);
        let deps = test_host_deps(
            &config,
            zcode_utils::daemon::Secret::generate().expect("secret"),
        );

        // 锁必须落在 `serve` 实际会锁的那条路径上——按工作区分桶后的子目录。
        // 路径错位的话 `serve` 会顺利拿到锁并进入 accept 循环，测试直接挂到超时。
        let runtime_dir =
            crate::host::scoped_runtime_dir(&config.daemon.runtime_dir, deps.workspace.root());
        std::fs::create_dir_all(&runtime_dir).expect("建分桶目录");
        let _lock = SingleInstanceLock::acquire(&runtime_dir.join(LOCK_FILE_NAME))
            .expect("acquire 不应失败")
            .expect("锁应当空闲，第一次拿必须成功");

        let result = serve(&config, deps).await;
        assert!(
            matches!(result, Err(DaemonServeError::AlreadyRunning { .. })),
            "锁被占用时应当立刻报 AlreadyRunning，实际是 {result:?}"
        );
    }

    /// 测试专用 provider 桩：`Ping` 走的是协议层健康探测，不会触发任何 turn，
    /// 因此这个 provider 的 `stream()` 在本文件的测试里永远不会被真正调用。
    #[derive(Debug)]
    struct StubProvider;

    #[async_trait::async_trait]
    impl zcode_ai::Provider for StubProvider {
        fn id(&self) -> zcode_ai::ProviderId {
            zcode_ai::ProviderId::Anthropic
        }

        async fn stream(
            &self,
            _request: &zcode_ai::CompletionRequest,
        ) -> Result<zcode_ai::EventStream, zcode_ai::AiError> {
            Err(zcode_ai::AiError::Aborted)
        }
    }

    fn test_config(runtime_dir: PathBuf, enabled: bool) -> Config {
        Config {
            model: crate::config::ModelConfig {
                id: None,
                thinking: None,
                provider: None,
            },
            approval: crate::config::ApprovalConfig {
                mode: zcode_agent::ApprovalMode::Yolo,
                policies: std::collections::HashMap::new(),
            },
            tools: crate::config::ToolsConfig {
                disabled: Vec::new(),
                bash_timeout_secs: 30,
                read_max_lines: 2000,
            },
            session: crate::config::SessionConfig {
                dir: runtime_dir.join("sessions"),
            },
            daemon: crate::config::DaemonConfig {
                enabled,
                runtime_dir,
            },
            ui: crate::config::UiConfig {
                show_thinking: false,
            },
        }
    }

    fn test_host_deps(config: &Config, secret: zcode_utils::daemon::Secret) -> HostDeps {
        HostDeps {
            provider: Arc::new(StubProvider),
            registry: Arc::new(zcode_agent::tool::registry::ToolRegistry::new()),
            config: Arc::new(test_config(
                config.daemon.runtime_dir.clone(),
                config.daemon.enabled,
            )),
            prompts: Arc::new(crate::prompt::PromptSet {
                system: Vec::new(),
                session_context: String::new(),
            }),
            model: crate::model::ResolvedModel {
                id: "stub".to_owned(),
                provider: zcode_ai::ProviderId::Anthropic,
                context_window: 200_000,
                thinking: zcode_ai::Thinking::Disabled,
            },
            workspace: Arc::new(crate::workspace::Workspace::new(
                config.daemon.runtime_dir.clone(),
            )),
            sessions_dir: config.session.dir.clone(),
            secret,
        }
    }

    async fn wait_for_registration(path: &Path) -> Registration {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(Some(registration)) = Registration::read(path) {
                return registration;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "等待注册文件写入超时"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn handshake_and_ping(stream: Stream, secret: &zcode_utils::daemon::Secret) -> Reply {
        let (mut read_half, mut write_half) = stream.into_split();
        let mut decoder = FrameDecoder::new();
        let mut buf = [0_u8; 8192];
        let id_gen = IdGen::default();

        let nonce_c = DaemonNonce::generate().expect("nonce");
        encode_and_send(
            &mut write_half,
            &Envelope::new(
                id_gen.next_id(),
                ClientFrame::Hello(ClientHello {
                    hello: Hello::local("test-client"),
                    nonce: ProtoNonce(nonce_c.as_str().to_owned()),
                }),
            ),
        )
        .await;

        let server_hello_frame = read_server_frame(&mut read_half, &mut decoder, &mut buf)
            .await
            .expect("应当收到 ServerHello");
        let ServerFrame::Hello(server_hello) = server_hello_frame.payload else {
            panic!("首帧应当是 Hello");
        };
        assert!(
            verify_proof(secret, Domain::Server, &nonce_c, &server_hello.proof.0),
            "服务端应当能证明持有握手密钥"
        );
        let server_nonce = DaemonNonce::from(server_hello.nonce.0.clone());
        let client_proof = proof(secret, Domain::Client, &server_nonce);
        encode_and_send(
            &mut write_half,
            &Envelope::new(
                id_gen.next_id(),
                ClientFrame::Auth(ClientAuth {
                    proof: Proof(client_proof),
                }),
            ),
        )
        .await;

        let ping_id = id_gen.next_id();
        encode_and_send(
            &mut write_half,
            &Envelope::new(ping_id, ClientFrame::Request(Request::Ping)),
        )
        .await;

        loop {
            let frame = read_server_frame(&mut read_half, &mut decoder, &mut buf)
                .await
                .expect("应当收到 Ping 的回应");
            if let ServerFrame::Reply(reply) = frame.payload {
                assert_eq!(frame.reply_to, Some(ping_id));
                return reply;
            }
        }
    }

    async fn encode_and_send(
        write_half: &mut zcode_utils::transport::WriteHalf,
        envelope: &Envelope<ClientFrame>,
    ) {
        let mut out = Vec::new();
        encode(envelope, &mut out).expect("编码不应失败");
        write_half.write_all(&out).await.expect("写入不应失败");
        write_half.flush().await.expect("flush 不应失败");
    }

    async fn read_server_frame(
        read_half: &mut zcode_utils::transport::ReadHalf,
        decoder: &mut FrameDecoder,
        buf: &mut [u8],
    ) -> Option<Envelope<ServerFrame>> {
        loop {
            match decoder.decode::<Envelope<ServerFrame>>() {
                Ok(Some(envelope)) => return Some(envelope),
                Ok(None) => {}
                Err(FrameError::Json(_)) => continue,
                Err(FrameError::TooLarge { .. }) => return None,
            }
            match read_half.read(buf).await {
                Ok(0) | Err(_) => return None,
                Ok(n) => decoder.push(buf.get(..n).unwrap_or_default()),
            }
        }
    }
}
