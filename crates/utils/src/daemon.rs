//! daemon 端点原语：注册文件、单实例锁、陈旧端点回收、就绪握手、握手证明。
//!
//! 本模块**不实现 daemon**，只提供它需要的四件不可自明的东西。连接处理与生命周期编排是
//! 装配层（CLI）的事。
//!
//! # 为什么本机 IPC 也要认证
//!
//! Unix domain socket 落在 owner-only 的目录里，文件权限就是访问控制。**Windows named pipe
//! 没有这个模型**：任何本机进程都能用同一个名字先把 pipe 建出来，客户端连过去毫无察觉。
//! 上游对这一点的结论是"token 是唯一防线"（oh-my-pi
//! `packages/coding-agent/src/launch/paths.ts:8-11` + `client.ts:90`）。
//!
//! 但**明文 bearer 不够**：占坑者收下 token 就等于拿到了钥匙。所以握手是双向挑战应答，
//! 由服务端先证明持有 [`Secret`]（见 [`proof`] 与 `zcode_protocol::version` 的模块文档）。
//! 密钥只存在于 owner-only 的注册文件里，一次都不上线。
//!
//! # 陈旧端点回收是双条件的
//!
//! "无活监听 **且** 能拿到独占锁"，且拿锁后**再探一次**。抄源 jcode
//! `crates/jcode-app-core/src/server/socket.rs:105-139`，四步的语义逐条对应：
//!
//! | 条件 | 防的是什么 |
//! |---|---|
//! | 先探活 | 活 daemon 正在应答，绝不能碰它的端点 |
//! | 能拿独占锁 | 活 daemon 整个生命周期握着锁；抢到即证明没有 daemon。它与探活不重复：**探测超时**与**进程已死**是两回事 |
//! | 拿锁后再探一次 | 一个刚 spawn、还没执行到"拿锁"那一行的新 daemon 可能已经 bind 上了端点。只看前两条会误杀它 |
//! | 只在 Unix 删节点 | Windows named pipe 没有文件节点，pipe 随进程消失，没有"陈旧"这回事 |
//!
//! **本模块不删锁文件。** jcode 在守卫 `Drop` 里删（`socket.rs:171-175`），那会造成一个
//! 真实竞态：进程 A 删掉锁文件的瞬间，进程 B 已经打开了同一个路径的旧 inode，此后 A 与 B
//! 各锁各的 inode，互斥静默失效。一个残留的空锁文件不花任何代价，删它才要命。

use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Child;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hmac::{Hmac, KeyInit as _, Mac as _};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use crate::transport::{Listener, Stream};

/// 探活单次尝试的上限。
///
/// **不要调大**：这是"端点上有没有人应答"，不是"服务端是否已经能干活"。活 daemon 可能正忙，
/// 所以超时一律**判定为活**（见 [`probe_live_listener`]），短超时因此是安全的。
const PROBE_TIMEOUT: Duration = Duration::from_millis(250);

/// 就绪等待的轮询间隔。
const READY_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// 就绪等待的默认上限。
///
/// 取 120 秒不是拍脑袋：jcode 在 Windows Server VPS 上实测过 auth preflight + provider 初始化
/// 耗时 15–60 秒（`src/cli/dispatch.rs:1293` 的注释绑定 issue #503）。等待期间**每轮都检查
/// 子进程是否已退出**，所以宽预算只会推迟"真正挂死"这一种情况的报错，不会掩盖崩溃。
pub const READY_TIMEOUT: Duration = Duration::from_mins(2);

/// 密钥与 nonce 的字节数。32 字节 = 256 bit，与 HMAC-SHA256 的输出等宽。
const SECRET_BYTES: usize = 32;

/// 就绪端点文件名里随机 slug 的字节数。
///
/// 它**不是**安全参数（认证靠令牌），只需在同一个目录内不撞名：9 字节 = 72 bit，
/// base64url 后 12 字符，加上 `ready-` 与 `.sock` 共 23 字符，给 macOS 的
/// 104 字节 `sun_path` 上限留足了目录前缀余量。
const ENDPOINT_SLUG_BYTES: usize = 9;

/// 传给子进程的就绪端点环境变量名。
pub const READY_ENDPOINT_ENV: &str = "ZCODE_READY_ENDPOINT";

/// daemon 原语的失败。
#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    /// 文件或端点 I/O 失败。
    #[error("{context}：{source}")]
    Io {
        /// 出错时在做什么。
        context: String,
        /// 底层错误。
        #[source]
        source: io::Error,
    },
    /// 注册文件内容不是本协议认识的 JSON。
    #[error("注册文件 {path} 解析失败：{source}")]
    Registration {
        /// 文件路径。
        path: PathBuf,
        /// 底层错误。
        #[source]
        source: serde_json::Error,
    },
    /// 熵源不可用。
    #[error("获取随机数失败：{0}")]
    Entropy(String),
    /// 子进程在就绪之前退出。
    #[error("子进程在就绪前退出：{status}")]
    ChildExited {
        /// 退出状态的渲染值。
        status: String,
    },
    /// 等待就绪超时。
    #[error("等待子进程就绪超过 {}s", .timeout.as_secs())]
    ReadyTimeout {
        /// 生效的上限。
        timeout: Duration,
    },
    /// 就绪握手拿到的令牌不对。**必须**当作攻击处理，不要重试。
    #[error("就绪握手令牌不匹配：连上来的不是我们 spawn 的那个子进程")]
    ReadyTokenMismatch,
}

fn io_error(context: impl Into<String>, source: io::Error) -> DaemonError {
    DaemonError::Io {
        context: context.into(),
        source,
    }
}

fn random_bytes(buffer: &mut [u8]) -> Result<(), DaemonError> {
    getrandom::fill(buffer).map_err(|err| DaemonError::Entropy(err.to_string()))
}

/// 注册文件里的共享密钥。
///
/// `Debug` 是**手写**的：密钥进日志一次就等于泄露一次，派生的 `Debug` 会把它原样打出来。
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Secret(String);

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

impl Secret {
    /// 生成一个新密钥。
    pub fn generate() -> Result<Self, DaemonError> {
        let mut bytes = [0_u8; SECRET_BYTES];
        random_bytes(&mut bytes)?;
        Ok(Self(URL_SAFE_NO_PAD.encode(bytes)))
    }

    /// 借出编码后的字符串。仅用于写注册文件，**不要**打日志。
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// 一次性随机数，base64url 无填充。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Nonce(String);

impl Nonce {
    /// 生成一个新 nonce。
    pub fn generate() -> Result<Self, DaemonError> {
        let mut bytes = [0_u8; SECRET_BYTES];
        random_bytes(&mut bytes)?;
        Ok(Self(URL_SAFE_NO_PAD.encode(bytes)))
    }

    /// 借出编码后的字符串。
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for Nonce {
    fn from(value: String) -> Self {
        Self(value)
    }
}

/// 证明的用途。
///
/// 域分隔串防的是**反射攻击**：没有它，占坑者可以把服务端刚发来的应答原样回给真 daemon
/// 冒充客户端应答。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Domain {
    /// 服务端对客户端 nonce 的应答。
    Server,
    /// 客户端对服务端 nonce 的应答。
    Client,
}

impl Domain {
    const fn tag(self) -> &'static [u8] {
        match self {
            Self::Server => b"zcode-daemon-server-proof-v1",
            Self::Client => b"zcode-daemon-client-proof-v1",
        }
    }
}

/// 计算 `HMAC-SHA256(secret, 域分隔串 ‖ nonce)`，base64url 无填充编码。
#[must_use]
pub fn proof(secret: &Secret, domain: Domain, nonce: &Nonce) -> String {
    URL_SAFE_NO_PAD.encode(mac(secret, domain, nonce))
}

/// 校验一份证明。比对由 HMAC 实现做常数时间处理，**不要**换成 `==`。
#[must_use]
pub fn verify_proof(secret: &Secret, domain: Domain, nonce: &Nonce, candidate: &str) -> bool {
    let Ok(decoded) = URL_SAFE_NO_PAD.decode(candidate) else {
        return false;
    };
    let expected = mac(secret, domain, nonce);
    constant_time_eq(&expected, &decoded)
}

fn mac(secret: &Secret, domain: Domain, nonce: &Nonce) -> Vec<u8> {
    // `new_from_slice` 对 HMAC 而言任何长度都合法，错误分支不可达；即便如此也不 unwrap，
    // 退化成一个恒不匹配的空摘要，让校验失败而不是让进程死。
    let Ok(mut hmac) = Hmac::<Sha256>::new_from_slice(secret.0.as_bytes()) else {
        return Vec::new();
    };
    hmac.update(domain.tag());
    hmac.update(nonce.0.as_bytes());
    hmac.finalize().into_bytes().to_vec()
}

/// 定长常数时间比较。长度不同直接判否——长度本身不是秘密。
fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() || left.is_empty() {
        return false;
    }
    let mut diff = 0_u8;
    for (a, b) in left.iter().zip(right.iter()) {
        diff |= a ^ b;
    }
    diff == 0
}

/// daemon 注册文件的内容。
///
/// 只放**发现**与**认证**需要的东西。它不是就绪证明：文件写下去与端点能 accept 是两回事，
/// 就绪走 [`ReadyChannel`]。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Registration {
    /// 本次注册的唯一 id。**用途是自查**：daemon 定期重读注册文件，`id` 不是自己就说明
    /// 被后来者抢注了，此时它已经无人可达（客户端只认文件里的端点），必须自行退出，
    /// 否则变成占着端点的孤儿（opencode `packages/cli/src/services/daemon.ts:174-179`）。
    pub id: String,
    /// daemon 的版本串。版本不同的客户端据此决定是复用还是重启。
    pub version: String,
    /// 端点路径。
    pub endpoint: PathBuf,
    /// daemon 进程 id。**只用于诊断，绝不能单独作为存活判据**：PID 会被 OS 复用
    /// （opencode `daemon.ts:152-159` 为此在发信号前先做一次带密钥的健康认证）。
    pub pid: u32,
    /// 握手密钥。
    pub secret: Secret,
}

impl Registration {
    /// 为一个端点生成一份新注册信息。
    pub fn create(endpoint: PathBuf, version: impl Into<String>) -> Result<Self, DaemonError> {
        Ok(Self {
            id: Nonce::generate()?.0,
            version: version.into(),
            endpoint,
            pid: std::process::id(),
            secret: Secret::generate()?,
        })
    }

    /// 原子写入：先写临时文件再 `rename`。
    ///
    /// `rename` 是必须的——注册文件会被并发读取，就地覆写会让读者读到半截 JSON。临时文件名
    /// 带随机后缀，两个并发注册者不会互相踩（opencode `daemon.ts:164-173`）。
    pub fn write_atomic(&self, path: &Path) -> Result<(), DaemonError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|source| io_error(format!("创建目录 {}", parent.display()), source))?;
        }
        let mut temp = path.as_os_str().to_owned();
        temp.push(".");
        temp.push(&self.id);
        temp.push(".tmp");
        let temp = PathBuf::from(temp);

        let payload =
            serde_json::to_vec_pretty(self).map_err(|source| DaemonError::Registration {
                path: path.to_path_buf(),
                source,
            })?;
        {
            let file = create_private_file(&temp)?;
            write_all(&file, &payload, &temp)?;
        }
        std::fs::rename(&temp, path)
            .map_err(|source| io_error(format!("重命名 {} 失败", temp.display()), source))
    }

    /// 读一份注册文件；文件不存在返回 `None`。
    pub fn read(path: &Path) -> Result<Option<Self>, DaemonError> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(io_error(format!("读取 {}", path.display()), source));
            }
        };
        serde_json::from_str(&text)
            .map(Some)
            .map_err(|source| DaemonError::Registration {
                path: path.to_path_buf(),
                source,
            })
    }

    /// 删除注册文件，但**只在它仍然是本次注册时**。
    ///
    /// 无条件删会把后来者的注册文件一起删掉，让一个活着的 daemon 变成无人可达的孤儿。
    pub fn remove_if_mine(&self, path: &Path) -> Result<bool, DaemonError> {
        match Self::read(path)? {
            Some(current) if current.id == self.id => {
                std::fs::remove_file(path)
                    .map_err(|source| io_error(format!("删除 {}", path.display()), source))?;
                Ok(true)
            }
            _ => Ok(false),
        }
    }
}

fn create_private_file(path: &Path) -> Result<File, DaemonError> {
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    // Windows 上 `%USERPROFILE%` 默认只有属主可写，没有等价于 0600 的可移植设置；
    // 这与 `zcode_ai::auth::store` 的凭据文件采用同一套处理。
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options
        .open(path)
        .map_err(|source| io_error(format!("创建 {}", path.display()), source))
}

fn write_all(mut file: &File, payload: &[u8], path: &Path) -> Result<(), DaemonError> {
    use std::io::Write as _;
    file.write_all(payload)
        .and_then(|()| file.flush())
        .map_err(|source| io_error(format!("写入 {}", path.display()), source))
}

/// 单实例锁：整个 daemon 生命周期持有。
///
/// 用 `std::fs::File::try_lock`（1.89 稳定）：锁绑定在**已打开的文件对象**上，进程无论怎么
/// 死，内核都会释放它。这正是"能拿到锁就说明没有活 daemon"成立的原因，也是它比"看 PID 活没活"
/// 可靠的原因。
#[derive(Debug)]
pub struct SingleInstanceLock {
    #[expect(dead_code, reason = "持有即上锁；文件对象一 drop 内核就释放锁")]
    file: File,
    path: PathBuf,
}

impl SingleInstanceLock {
    /// 尝试取得锁；已被别人持有时返回 `Ok(None)`。
    ///
    /// **daemon 必须把这一步放在任何副作用之前**（先于 reap、先于 bind）。这条不变式让
    /// "拿到锁 ⇒ 没有别的 daemon 正在起来"成立，[`reap_stale_endpoint`] 的正确性依赖它。
    pub fn acquire(path: &Path) -> Result<Option<Self>, DaemonError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|source| io_error(format!("创建目录 {}", parent.display()), source))?;
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)
            .map_err(|source| io_error(format!("打开锁文件 {}", path.display()), source))?;
        match file.try_lock() {
            Ok(()) => Ok(Some(Self {
                file,
                path: path.to_path_buf(),
            })),
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(source)) => Err(io_error(
                format!("给锁文件 {} 上锁", path.display()),
                source,
            )),
        }
    }

    /// 锁文件路径。
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// 端点上是否有人在应答。
///
/// **只探一次，探完不要接着 connect。** Windows 上探针会临时占掉唯一已发布的 pipe 实例，
/// 紧接着的真 connect 会掉进 `ERROR_PIPE_BUSY` 重试循环（jcode
/// `crates/jcode-app-core/src/server/socket.rs:72-83`）。本函数的唯一调用点是回收路径，
/// 那条路径之后走的是 bind，不是 connect。
///
/// **超时判定为"活"**：忙碌的 daemon 与死掉的 daemon 在超时这一点上无法区分，
/// 而误判为死会删掉活端点，误判为活只是不回收。
pub async fn probe_live_listener(endpoint: &Path) -> bool {
    match tokio::time::timeout(PROBE_TIMEOUT, Stream::connect(endpoint)).await {
        Ok(Ok(stream)) => {
            drop(stream);
            true
        }
        Ok(Err(_)) => false,
        Err(_elapsed) => true,
    }
}

/// 回收陈旧端点。返回是否真的删掉了东西。
///
/// 取 `&SingleInstanceLock` 而不是锁路径：**独占锁是双条件里的一条**，用类型把它变成调用
/// 方无法绕过的前提，比在文档里叮嘱可靠。
pub async fn reap_stale_endpoint(
    _lock: &SingleInstanceLock,
    endpoint: &Path,
) -> Result<bool, DaemonError> {
    // 拿锁之后再探一次：一个刚 spawn、还没走到"拿锁"那一行的新 daemon 可能已经 bind 上了。
    if probe_live_listener(endpoint).await {
        return Ok(false);
    }
    #[cfg(windows)]
    {
        // named pipe 没有文件节点，随进程消失，不存在陈旧端点。
        let _ = endpoint;
        Ok(false)
    }
    #[cfg(not(windows))]
    {
        match std::fs::remove_file(endpoint) {
            Ok(()) => {
                tracing::warn!(endpoint = %endpoint.display(), "回收了上一个进程留下的陈旧端点");
                Ok(true)
            }
            Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(source) => Err(io_error(format!("删除端点 {}", endpoint.display()), source)),
        }
    }
}

/// 一次性就绪通道：父进程等子进程宣布"我的监听已经能 accept 了"。
///
/// # 为什么不是注册文件、也不是 stdout
///
/// - **注册文件只证明"曾写入"**，不与本次 spawn 绑定：陈旧文件、被复用的 PID 都能让父进程
///   拿到假就绪。计划把 PID 复用明确列为安全边界。
/// - **stdout 文本匹配**是 opencode 的反例（`packages/sdk/js/src/v2/server.ts:55-70`），
///   日志格式一改就断，而且会污染结构化输出。
///
/// jcode 用 `libc::pipe` + `JCODE_READY_FD`（`server/socket.rs:237-324`），语义正确但只有
/// Unix 版，且要 `unsafe`。本实现用 [`crate::transport`] 造一个**带一次性令牌的临时端点**：
/// 跨平台、无 `unsafe`、令牌与本次 spawn 一一绑定，冒名者连上来也过不了令牌校验。
#[derive(Debug)]
pub struct ReadyChannel {
    listener: Listener,
    endpoint: PathBuf,
    token: String,
}

impl ReadyChannel {
    /// 在 `dir` 下建一个一次性就绪端点。
    ///
    /// **文件名里放的不是令牌。** 两个独立理由：
    ///
    /// - Unix socket 的整条路径要塞进 `sockaddr_un.sun_path`，macOS 上只有 104 字节
    ///   （Linux 108）。43 字符的 base64 令牌加上 `/var/folders/…/T/` 这种 macOS 临时目录
    ///   前缀就会溢出，报 `InvalidInput: path must be shorter than SUN_LEN`。
    /// - Windows named pipe 名字对**本机任意进程可见**，令牌进名字等于把唯一防线公开。
    ///
    /// 所以名字用一段独立的短随机 slug（只需在 `dir` 内唯一），令牌只走
    /// [`READY_ENDPOINT_ENV`] 与握手本身。
    ///
    /// # 三仓对照
    ///
    /// **"密钥不进端点名"是三仓的一致约定**，本函数是回到该约定，不是新发明：
    /// oh-my-pi 把令牌单独放 `broker.token`（0600 文件、0700 目录），端点名是确定性的
    /// `broker.sock`（`packages/coding-agent/src/launch/paths.ts:7-14`、
    /// `packages/utils/src/dirs.ts:854-858`、`client.ts:77-95`）；jcode 的端点名全是固定
    /// 字面量 `jcode.sock` / `jcode-api.sock`（`crates/jcode-app-core/src/server/socket.rs:7-24`、
    /// `crates/jcode-harness-api/src/sockets.rs:67-72`），生产代码里零随机成分；
    /// opencode 压根不用 unix socket，走回环 TCP + `server.json` 注册文件
    /// （`packages/cli/src/services/daemon.ts:164-173`），并显式 `Effect.die` 掉非 TCP 地址
    /// （`packages/opencode/src/server/server.ts:139-146`）。
    ///
    /// **三仓都没有显式的 `sun_path` 长度处理**，它们靠"名字里没有变长成分"天然规避
    /// （jcode 最长的默认端点在 macOS `$TMPDIR` 下约 65 字节，距 104 有余量）。
    /// 唯一的平台规避是 oh-my-pi 对 DAP 适配器在 macOS 上放弃 unix socket 改回环 TCP
    /// （`packages/coding-agent/src/dap/client.ts:217-233`），那是因为路径由第三方适配器
    /// 接口决定、名字不可控；本函数的名字可控，所以不需要。
    ///
    /// **本函数保留随机 slug 而不照抄确定性短名**，因为语义相反：三仓那些端点是长驻的、
    /// 要让任意进程自己算出来去 connect，所以必须确定性；就绪端点是一次性的、只有父子两方
    /// 知道，且 `dir` 可能被并发的多次 spawn 共用，确定性名字会撞。9 字节 = 72 bit 的
    /// 撞名余量远超需要，而 12 字符的名字仍留足 `sun_path` 预算。
    pub fn bind(dir: &Path) -> Result<Self, DaemonError> {
        std::fs::create_dir_all(dir)
            .map_err(|source| io_error(format!("创建目录 {}", dir.display()), source))?;
        let token = Nonce::generate()?.0;
        let mut slug_bytes = [0_u8; ENDPOINT_SLUG_BYTES];
        random_bytes(&mut slug_bytes)?;
        let endpoint = dir.join(format!("ready-{}.sock", URL_SAFE_NO_PAD.encode(slug_bytes)));
        let listener = Listener::bind(&endpoint)
            .map_err(|source| io_error(format!("绑定就绪端点 {}", endpoint.display()), source))?;
        Ok(Self {
            listener,
            endpoint,
            token,
        })
    }

    /// 传给子进程的环境变量值（配合 [`READY_ENDPOINT_ENV`]）。
    ///
    /// 令牌在前、路径在后，按**第一个** `@` 切分：令牌是 base64url，不含 `@`；路径可能含任何字符。
    #[must_use]
    pub fn env_value(&self) -> String {
        format!("{}@{}", self.token, self.endpoint.display())
    }

    /// 等子进程宣布就绪。
    ///
    /// 每轮都 `try_wait` 子进程：崩溃立刻报错，超时只可能发生在"进程还活着但真的挂死"。
    pub async fn wait(&mut self, child: &mut Child, timeout: Duration) -> Result<(), DaemonError> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if let Some(status) = child
                .try_wait()
                .map_err(|source| io_error("检查子进程状态", source))?
            {
                return Err(DaemonError::ChildExited {
                    status: status.to_string(),
                });
            }
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(DaemonError::ReadyTimeout { timeout });
            }
            let slice = remaining.min(READY_POLL_INTERVAL);
            match tokio::time::timeout(slice, self.listener.accept()).await {
                Ok(Ok(stream)) => return self.verify(stream).await,
                Ok(Err(source)) => return Err(io_error("接受就绪连接", source)),
                Err(_elapsed) => {}
            }
        }
    }

    async fn verify(&self, stream: Stream) -> Result<(), DaemonError> {
        use tokio::io::{AsyncBufReadExt as _, BufReader};

        let (read_half, _write_half) = stream.into_split();
        let mut line = String::new();
        BufReader::new(read_half)
            .read_line(&mut line)
            .await
            .map_err(|source| io_error("读取就绪令牌", source))?;
        if constant_time_eq(line.trim_end().as_bytes(), self.token.as_bytes()) {
            Ok(())
        } else {
            Err(DaemonError::ReadyTokenMismatch)
        }
    }
}

impl Drop for ReadyChannel {
    fn drop(&mut self) {
        // 就绪端点是一次性的；留在磁盘上只会攒垃圾。Windows 上 pipe 无文件节点，删除必然
        // 是 NotFound，忽略即可。
        let _ = std::fs::remove_file(&self.endpoint);
    }
}

/// 子进程侧：连上就绪端点、回写令牌。
///
/// **必须在监听已经能 accept 之后调用**，不能在 bind 之前——否则父进程会在端点还不可连的
/// 瞬间就认为一切就绪（jcode `server.rs:1198-1205` 把 `signal_ready_fd()` 排在两个 accept
/// loop spawn 之后，正是这个理由）。
pub async fn signal_ready(env_value: &str) -> Result<(), DaemonError> {
    use tokio::io::AsyncWriteExt as _;

    let Some((token, endpoint)) = env_value.split_once('@') else {
        return Err(DaemonError::ReadyTokenMismatch);
    };
    let endpoint = PathBuf::from(endpoint);
    let stream = Stream::connect(&endpoint)
        .await
        .map_err(|source| io_error(format!("连接就绪端点 {}", endpoint.display()), source))?;
    let (_read_half, mut write_half) = stream.into_split();
    write_half
        .write_all(format!("{token}\n").as_bytes())
        .await
        .map_err(|source| io_error("回写就绪令牌", source))?;
    write_half
        .flush()
        .await
        .map_err(|source| io_error("刷新就绪令牌", source))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Duration;

    use super::{
        Domain, Nonce, ReadyChannel, Registration, Secret, SingleInstanceLock, constant_time_eq,
        proof, reap_stale_endpoint, verify_proof,
    };

    #[test]
    fn proofs_are_domain_separated_and_nonce_bound() {
        let secret = Secret::generate().expect("生成密钥");
        let nonce = Nonce::generate().expect("生成 nonce");
        let server = proof(&secret, Domain::Server, &nonce);

        assert!(verify_proof(&secret, Domain::Server, &nonce, &server));
        assert!(
            !verify_proof(&secret, Domain::Client, &nonce, &server),
            "服务端应答被原样反射回去时必须校验失败，否则占坑者可以冒充客户端"
        );

        let other = Nonce::generate().expect("生成 nonce");
        assert!(
            !verify_proof(&secret, Domain::Server, &other, &server),
            "换一个 nonce 就该失效，否则旧握手可以重放"
        );

        let intruder = Secret::generate().expect("生成密钥");
        assert!(!verify_proof(&intruder, Domain::Server, &nonce, &server));
        assert!(!verify_proof(
            &secret,
            Domain::Server,
            &nonce,
            "不是 base64"
        ));
    }

    #[test]
    fn secret_never_leaks_through_debug() {
        let secret = Secret::generate().expect("生成密钥");
        let rendered = format!("{secret:?}");
        assert!(
            !rendered.contains(secret.as_str()),
            "密钥进日志一次就是泄露"
        );
    }

    #[test]
    fn constant_time_eq_rejects_length_mismatch_and_empty() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
        assert!(!constant_time_eq(b"", b""), "空对空不算通过");
    }

    #[test]
    fn registration_round_trips_atomically() {
        let dir = tempfile::tempdir().expect("临时目录");
        let path = dir.path().join("nested").join("daemon.json");
        let registration =
            Registration::create(PathBuf::from("/tmp/zcode.sock"), "0.1.0").expect("造注册信息");

        registration.write_atomic(&path).expect("原子写");
        let read = Registration::read(&path)
            .expect("读注册")
            .expect("文件应存在");
        assert_eq!(read, registration);
        assert!(
            std::fs::read_dir(path.parent().expect("有父目录"))
                .expect("列目录")
                .filter_map(Result::ok)
                .all(|entry| !entry.file_name().to_string_lossy().ends_with(".tmp")),
            "临时文件必须已被 rename 掉"
        );
    }

    #[test]
    fn reading_a_missing_registration_is_not_an_error() {
        let dir = tempfile::tempdir().expect("临时目录");
        assert!(
            Registration::read(&dir.path().join("absent.json"))
                .expect("缺文件不是错误")
                .is_none()
        );
    }

    #[test]
    fn rewriting_an_existing_registration_replaces_it_atomically() {
        // daemon 重启 / 接任时目标文件已经存在。Windows 的部分 rename 实现遇到已存在目标
        // 会失败——真机跑一遍定论，不靠推测（Rust 的 `fs::rename` 走
        // `MoveFileEx(MOVEFILE_REPLACE_EXISTING)`，这个用例钉住它）。
        let dir = tempfile::tempdir().expect("临时目录");
        let path = dir.path().join("daemon.json");
        let first =
            Registration::create(PathBuf::from("/tmp/first.sock"), "0.1.0").expect("造注册信息");
        let second =
            Registration::create(PathBuf::from("/tmp/second.sock"), "0.2.0").expect("造注册信息");

        first.write_atomic(&path).expect("首次写入");
        second.write_atomic(&path).expect("覆盖写入必须成功");

        let read = Registration::read(&path)
            .expect("读注册")
            .expect("文件应存在");
        assert_eq!(read, second, "读到的必须是后写的那一份");

        let residue: Vec<_> = std::fs::read_dir(dir.path())
            .expect("列目录")
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| {
                std::path::Path::new(name)
                    .extension()
                    .is_some_and(|ext| ext == "tmp")
            })
            .collect();
        assert!(
            residue.is_empty(),
            "临时文件必须已被 rename 掉：{residue:?}"
        );
    }

    #[test]
    fn remove_if_mine_never_deletes_a_successors_file() {
        let dir = tempfile::tempdir().expect("临时目录");
        let path = dir.path().join("daemon.json");
        let mine = Registration::create(PathBuf::from("/tmp/a.sock"), "0.1.0").expect("造注册信息");
        let successor =
            Registration::create(PathBuf::from("/tmp/b.sock"), "0.1.0").expect("造注册信息");

        successor.write_atomic(&path).expect("后来者写入");
        assert!(!mine.remove_if_mine(&path).expect("检查归属"));
        assert!(path.exists(), "绝不能删掉后来者的注册文件");

        assert!(successor.remove_if_mine(&path).expect("检查归属"));
        assert!(!path.exists());
    }

    #[test]
    fn the_lock_is_exclusive_while_held() {
        let dir = tempfile::tempdir().expect("临时目录");
        let path = dir.path().join("daemon.lock");

        let held = SingleInstanceLock::acquire(&path)
            .expect("首次上锁")
            .expect("首次必须拿到");
        assert_eq!(held.path(), path);
        assert!(
            SingleInstanceLock::acquire(&path)
                .expect("二次尝试不该报错")
                .is_none(),
            "锁被持有时第二次必须拿不到——单实例互斥全靠这一条"
        );

        drop(held);
        assert!(
            SingleInstanceLock::acquire(&path)
                .expect("释放后再上锁")
                .is_some()
        );
        assert!(
            path.exists(),
            "锁文件绝不删：删它会让两个进程各锁各的 inode"
        );
    }

    #[tokio::test]
    async fn a_live_endpoint_is_never_reaped() {
        let dir = tempfile::tempdir().expect("临时目录");
        let endpoint = dir.path().join("live.sock");
        let _listener = crate::transport::Listener::bind(&endpoint).expect("绑定端点");
        let lock = SingleInstanceLock::acquire(&dir.path().join("daemon.lock"))
            .expect("上锁")
            .expect("必须拿到");

        assert!(super::probe_live_listener(&endpoint).await);
        assert!(
            !reap_stale_endpoint(&lock, &endpoint)
                .await
                .expect("回收判定"),
            "有人在监听时绝不能碰端点"
        );
    }

    #[tokio::test]
    async fn a_dead_endpoint_is_reaped_on_unix_only() {
        let dir = tempfile::tempdir().expect("临时目录");
        let endpoint = dir.path().join("dead.sock");
        {
            let _listener = crate::transport::Listener::bind(&endpoint).expect("绑定端点");
        }
        let lock = SingleInstanceLock::acquire(&dir.path().join("daemon.lock"))
            .expect("上锁")
            .expect("必须拿到");

        let reaped = reap_stale_endpoint(&lock, &endpoint)
            .await
            .expect("回收判定");
        if cfg!(windows) {
            // named pipe 随进程消失，没有文件节点可回收。
            assert!(!reaped);
        } else {
            assert!(reaped);
            assert!(!endpoint.exists());
        }
    }

    /// macOS 的 `sockaddr_un.sun_path` 只有 104 字节，而它的临时目录前缀
    /// （`/var/folders/xx/…/T/`）就占掉 60 多字节。就绪端点的文件名必须留出余量，
    /// 否则 `bind` 直接报 `path must be shorter than SUN_LEN`。
    #[tokio::test]
    async fn ready_endpoint_name_stays_within_the_sun_path_budget() {
        let dir = tempfile::tempdir().expect("临时目录");
        let channel = ReadyChannel::bind(dir.path()).expect("绑定就绪端点");
        let name = channel
            .endpoint
            .file_name()
            .expect("端点必须有文件名")
            .to_string_lossy()
            .into_owned();
        assert!(
            name.len() <= 24,
            "端点文件名 {name} 过长（{} 字节），macOS 上会撑爆 sun_path",
            name.len()
        );
        // 令牌不得出现在名字里：Windows named pipe 名对本机任意进程可见。
        assert!(!name.contains(&channel.token), "令牌泄露进了端点名 {name}");
    }

    #[tokio::test]
    async fn ready_channel_accepts_only_the_matching_token() {
        let dir = tempfile::tempdir().expect("临时目录");
        let channel = ReadyChannel::bind(dir.path()).expect("绑定就绪端点");
        let env_value = channel.env_value();
        let (token, endpoint) = env_value.split_once('@').expect("环境变量格式");
        assert!(!token.is_empty());

        // 冒名者：连上来但令牌不对。
        let intruder = format!("bogus-token@{endpoint}");
        let mut channel = channel;
        let handle = tokio::spawn(async move { super::signal_ready(&intruder).await });
        let accepted = channel.listener.accept().await.expect("接受连接");
        let verdict = channel.verify(accepted).await;
        assert!(
            matches!(verdict, Err(super::DaemonError::ReadyTokenMismatch)),
            "令牌不匹配必须被识破：{verdict:?}"
        );
        let _ = handle.await;

        // 真子进程：令牌正确。
        let good = channel.env_value();
        let handle = tokio::spawn(async move { super::signal_ready(&good).await });
        let accepted = channel.listener.accept().await.expect("接受连接");
        channel.verify(accepted).await.expect("令牌匹配必须通过");
        handle.await.expect("任务不该 panic").expect("回写令牌");
    }

    #[tokio::test]
    async fn waiting_reports_a_child_that_died_before_signalling() {
        let dir = tempfile::tempdir().expect("临时目录");
        let mut channel = ReadyChannel::bind(dir.path()).expect("绑定就绪端点");

        let program = if cfg!(windows) { "cmd" } else { "sh" };
        let args: &[&str] = if cfg!(windows) {
            &["/C", "exit 3"]
        } else {
            &["-c", "exit 3"]
        };
        let mut child = std::process::Command::new(program)
            .args(args)
            .spawn()
            .expect("拉起子进程");

        let error = channel
            .wait(&mut child, Duration::from_secs(10))
            .await
            .expect_err("子进程已经死了，绝不能报成功");
        assert!(
            matches!(error, super::DaemonError::ChildExited { .. }),
            "必须报成子进程退出而不是超时：{error:?}"
        );
    }
}
