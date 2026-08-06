---
description: ZCode 特有构件约束：crate 职责与导入边界、worker 子进程重入 CLI、prompt 存静态 .md、生成物禁改、中央工具函数、TUI 输出清理。新增 crate / 动 worker / 改 prompt / 改渲染路径 / 改生成物前必读。
---

# ZCode 构件与运行时约束

## Crate 职责表

目标 workspace 由 10 个 crate 组成。十个 crate 目录均已落盘（`zcode-utils`、`zcode-protocol`、`zcode-schema`、`zcode-text`、`zcode-catalog`、`zcode-ai`、`zcode-agent`、`zcode-tui` 有实现；`zcode`、`zcode-stats` 暂为骨架）；表内其他路径仍标 `(planned)` 的，落盘后逐条删除标记，不要整表一次性摘掉。

| Crate | 包名 | 职责 |
| --- | --- | --- |
| `crates/ai/` | `zcode-ai` | 支持流式传输的多提供商 LLM 客户端 |
| `crates/catalog/` | `zcode-catalog` | 模型目录：内置 `models.json`、提供商描述符、模型身份识别/分类 |
| `crates/agent/` | `zcode-agent` | 支持工具调用和状态管理的 Agent 运行时 |
| `crates/coding-agent/` | `zcode` | 主 CLI 应用程序，是首要关注对象 |
| `crates/tui/` | `zcode-tui` | 支持差分渲染的终端 UI 库 |
| `crates/text/` | `zcode-text` | 性能关键型文本、图像及 grep 操作 |
| `crates/stats/` | `zcode-stats` | 本地可观测性仪表盘（`zcode stats`） |
| `crates/schema/` | `zcode-schema` | JSON Schema 校验，惰性编译运行时 |
| `crates/utils/` | `zcode-utils` | 共享工具（日志、流、临时文件、进程包装） |
| `crates/protocol/` | `zcode-protocol` | 客户端与运行时之间的 wire 协议：版本/握手、信封、NDJSON 分帧、协议错误码 |

## 进程边界：客户端 ↔ 运行时

**决策已定**（调研证据与三仓对照见 `plans/runtime-boundary/`）：agent 运行时活在独立 daemon 进程，
所有客户端（TUI、编辑器插件、移动端）跨进程连它。**别把"UI 秒开"当作 daemon 的理由**——
opencode 文档明确否认这个因果（`packages/web/src/content/docs/server.mdx:57`），
秒开来自客户端不做重活 + 第一帧先画，与传输形态无关。daemon 买到的是多端共享会话与
agent 脱离 UI 存活。

### 五条硬约束

1. **`zcode-protocol` 是唯一编译边界。** 依赖方向 `tui -> protocol <- runtime`，
   所有 wire 类型归 protocol 所有；领域类型与 wire 类型的互转是 host adapter 的职责。
   **绝不**用 `pub use <runtime>::*` 整体转出来省 import——jcode
   `crates/jcode-tui/src/lib.rs:23` 正是这么做的，代价是协议边界退化成自律，
   且它的 CI 检查（`scripts/check_dependency_boundaries.py:26-51`）只护 `*-types` crate，看不见。
2. **运行时 crate 绝不依赖 `ratatui` / `crossterm`。** jcode 的 `jcode-app-core` 依赖了
   （`crates/jcode-app-core/Cargo.toml:73-75`），daemon 进程白扛渲染栈。
3. **只有一条执行路径。** headless（`zcode -p`）与 TUI 共用同一套连接处理函数：daemon 在就连它，
   不在就同进程自托管，用 `zcode_utils::transport::stream_pair()` 把自己接上去。
   **绝不**为 headless 另开一条进程内直调路径——jcode 的 `jcode run`
   （`src/cli/commands.rs:2362-2415`）就是反例，headless 的 MCP 冷缓存问题得单独打补丁 +
   `JCODE_RUN_MCP_WAIT_MS` 兜底，而这个坑在统一路径下根本不存在。
4. **进程内不付序列化成本。** 跨进程走 `zcode_utils::transport` + NDJSON；进程内直接传 `enum`
   走 channel。opencode 的 Worker RPC 为了"和 HTTP 一致"把每个请求体整体字符串化
   （`packages/opencode/src/util/rpc.ts:8`），不要抄。
5. **未知变体：推送可丢，请求不可。** 常驻 daemon 必然遇到新旧混连。`Event` 一类推送用
   internally tagged + `#[serde(other)]` 兜底静默跳过；**`Request` 绝不可跳过**——请求方在等
   `reply_to` 指向自己 `id` 的那一帧，跳过等于让它永久挂着，必须回
   `ErrorCode::UnsupportedRequest`。这个失败形状在 opencode 上有实证：权限询问的 pending
   只在内存、重连不重拉，SSE 在 `permission.asked` 后断开则服务端工具永久挂着而 UI 无显示
   （`packages/opencode/src/permission/index.ts:98-107`，重连路径
   `packages/tui/src/context/sync.tsx:451-532` 无 `permission.list` 调用）。
   规则本体与示例在 `crates/protocol/src/lib.rs` 与 `error.rs` 的模块文档里。

### 落地时必须一起抄的东西

缺一个就是一类 bug，全部坐标在 `plans/runtime-boundary/README.md` 第 7 节：

- turn 属于 session scope 而非连接（否则 UI 一断 agent 就死，多端共享无从谈起）；
- 取消三层：`InterruptSignal`（`AtomicBool` + epoch + `Notify`）+ 进程级 turn 注册表 +
  cancel 请求优先于 Ack 分发。`CancellationToken` 不够用，理由是同一次取消要打到可能是多个
  实例的信号上，且延时 reset 不能抹掉新 fire；
- 权限审批走 oneshot-by-request-id 回环，且**重连后必须重拉 pending 列表**——
  opencode 漏了这一步，后果是 SSE 在询问后断开则服务端工具永久挂着而 UI 无显示；
- 解帧器四件套（已落在 `zcode-protocol` 的 `frame` 模块）；
- 单实例锁 + 就绪握手 + 陈旧端点**双条件**回收（无活监听 **且** 能拿独占锁）。

## Catalog 导入边界

内置模型、模型思考辅助函数、身份信息、描述符、模型管理器/缓存等**值**，一律从 `zcode_catalog::<module>` 导入，绝不经由 `zcode_ai` 的 re-export 导入。`zcode_ai` 的 `lib.rs` 仅 re-export 其自身签名用到的模型/effort **类型**（`Model`、`Api`、`ThinkingConfig`、`Effort` 等）；只需要这些类型时可从 `zcode_ai` 导入，需要值时必须绕过它直接打 `zcode_catalog`。

## Worker 子进程契约

Worker **必须重入 CLI 入口**，绝不编译独立的 worker 二进制。CLI 入口 crate（`crates/coding-agent/`）的 `src/main.rs` 在启动时通过 `zcode_utils::env::declare_worker_host_entry()` 将自身声明为 worker host（已落盘），并须在加载命令注册表之前分派隐藏的 argv selector（`__zcode_worker_stats_sync`、`__zcode_worker_tab`、`__zcode_worker_eval`、`__zcode_worker_tiny_inference`）——**分发表尚未落盘**，第一个 worker 落地时一并加。启动方必须走这条路径：

```rust
use zcode_utils::env::worker_host_entry;

let child = Command::new(worker_host_entry()?)
    .arg("__zcode_worker_<name>")
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .spawn()?;
```

进程经 zcode CLI 启动时（`cargo run`、`cargo install` 安装的二进制、发布的静态二进制均适用），`worker_host_entry()` 返回启动时记录的 `std::env::current_exe()`，worker 重入同一个二进制。不处于 CLI host 中时（`cargo test`、库嵌入、独立运行的 `zcode-stats`），该函数返回 `Err(WorkerHostError::NotDeclared)`，必须回退到进程内线程实现（`tokio::spawn` + 同一份 worker 逻辑）。新增 worker 类型时**必须**：把 selector 加入 `crates/coding-agent/src/main.rs` 的分发表、保留进程内 fallback 分支、补一个同级 smoke 测试。

**历史缘由**（不得丢）：早期方案是每个 worker 声明独立 `[[bin]]` 目标，导致 `cargo install` 与打包脚本必须始终同步两套目标列表，漏掉一个就在安装后静默失败——这是为何所有 worker 现在统一走重入 CLI 入口这条路径。

**Smoke probe 契约（未落盘，落盘时按此设计）**：本仓目前**没有** `--smoke-test` flag、没有对应 CI job、没有 `scripts/install-tests/`。落盘时的目标是：`zcode --smoke-test` 逐个启动已注册的 worker 子进程，各发一次 ping 后退出，并接进 CI 与安装后验证，使二进制安装、`cargo install --path` 安装、crate 包安装都跑到这条路径。新 worker 若不在同一模块图上，必须补一个同级 smoke 测试，否则该验证覆盖不到它。

## Prompt 必须存静态 `.md`

绝不在代码里拼 prompt——不用内联字符串、不用 `format!`、不做字符串拼接。Prompt 一律存放在静态 `.md` 文件中；动态内容用 Handlebars 模板变量。导入方式是 `include_str!("./prompt.md")`，绝不用运行时 `fs::read_to_string`（否则 prompt 会随发布环境漂移，且无法被编译期检查覆盖）。

## 生成物禁改

`crates/catalog/src/models.json` 由 `crates/catalog/src/bin/gen_models.rs`（经
`cargo run -p zcode-catalog --features gen --bin gen-models` 调用）从上游快照
`https://catalog.stencil.so/models.json.zstd` 生成，**绝不能手工编辑**，下次重新生成会覆盖手改内容。

生成器**只读这一个公开只读 URL**：不读本机凭据、不发带 key 的 discovery 请求。因此同一份上游快照在
任何机器上产出的字节完全一致（容器一律 `BTreeMap`、字段顺序由结构体声明顺序固定）。
oh-my-pi 的生成器读 env + 本机 `agent.db` 再发真实请求，导致不同机器产出不同的 `models.json`
（`packages/catalog/scripts/generate-models.ts:80-113`）——那是债，本仓不继承。要改条目，改源头：

- 磁盘契约（字段增删、序列化形态）→ `crates/catalog/src/spec.rs`，生成器与运行时共用同一套类型。
- 上游 → 本仓的字段映射、模态与状态白名单、`0` 视作未知的归一 → `crates/catalog/src/bin/gen_models.rs`。
- 提供商描述符（base URL、环境变量、默认模型、discovery、线格式）→ `crates/catalog/src/descriptors.rs`。
  该文件的测试会断言每条非 `discovery_only` 的 id 与 `default_model` 在 `models.json` 里真实存在——
  上游的描述符表与生成物已经漂移且用不安全 cast 掩盖，缺的正是这个一致性检查，不要删掉它。
- 思考元数据 / effort ladder → `crates/catalog/src/thinking.rs`；模型 ID 分类（family/version 解析）
  → `crates/catalog/src/identity/classify.rs`。

改完重新生成，把 `models.json` 与源码修改一并提交。回归测试要打在**描述符与解析函数**上，
不要打在内置 JSON 的具体条目上，这样上游元数据变化后测试仍然有效。

## 中央工具函数优先

写 helper 前先搜现有实现——`crates/coding-agent/src/utils/` (planned)、`zcode-utils`、`zcode-tui`，以及调用点附近的领域模块。此规则适用于**所有内容**：VCS 包装器、格式化/截断/路径显示辅助函数、图像处理、剪贴板、流、临时文件、缓存。中央实现里包含的健壮性处理（超时、输出上限、非交互式环境判断、锁规避、缓存、TUI 清理）是新拷贝一份通常会丢失的东西。

- 先用 `grep` 搜对应操作；两个实现都能跑也算重复实现，是缺陷。
- git/jj 只能经统一包装层调用：`src/utils/git.rs` (planned)、`src/utils/jj.rs` (planned) 是唯一许可入口，绝不手写 `Command::new("git")`。
- 缺能力就扩展中央 helper（加参数或加子函数），不要局部复制分叉逻辑。

## TUI 输出清理

清洗能力的**唯一实现落点是 `zcode-text`**，不是 `zcode-tui`：`width`（显示宽度、按宽度截断、
换行、制表符展开、ANSI 剥离、控制字符清洗）、`truncate`（行/字节/列三维截断与 `OutputSink`）、
`path::shorten_path`（主目录 → `~`）。渲染代码一律调它们，**绝不在渲染点另写一份**。

要求：

- 制表符按列展开成空格（`width::expand_tabs`）——制表符在等宽网格里会造成视觉空洞；
- 长行按**显示宽度**截断（`width::truncate_to_width` / `truncate::cap_columns`），
  绝不用 `str::len()` 或 `chars().count()`。上游同一个 512 喂给两套实现——native 侧按字节、
  JS 侧按字符——CJK 下截断位置差三倍；本仓只有一套，按显示宽度；
- 多码点 grapheme 簇整簇交给 `unicode-width` 求宽，不得逐字符求和（否则 ZWJ 家庭 emoji 算 8 列，
  实际 2 列，jcode `crates/jcode-tui/src/tui/ui/display_width.rs:1-19` 就是反例）；
- 路径显示走 `path::shorten_path`，不泄露用户目录；Windows 大小写不敏感由它内部处理；
- 预览行数 / 条目数走 `truncate::TruncateLimits` 的统一常量，不得在渲染点硬编码数字；
- 以上适用于**每一条渲染路径**，不只是成功路径：错误消息（patch 失败消息常带未匹配的原始文件行）、
  diff 的新增与删除两侧、流式预览，都要过同一套清理。

**流式预览的多路径陷阱**（oh-my-pi 的实战教训，落盘时照此设计）：工具调用预览通常存在
实时事件与 transcript 重建两条渲染路径，两条必须共用同一个解码器；不能把提供商已解析的参数
与原始 partial JSON 并列展开——已解析参数受节流解析窗口影响，会滞后于流
（`packages/coding-agent/src/modes/controllers/tool-args-reveal.ts:10-14,433-437`）。
bash 的待执行预览尤其需要原始 partial JSON：内联环境变量赋值可能要到 JSON 对象闭合前一刻才可见。
新增仅用于预览的字段时，所有渲染路径必须同步更新；只修一条不算修好，两条路径都要实测。

## 日志与 CLI 输出边界

TUI、RPC、SDK、worker 或后台运行时处于活动状态期间可能执行的代码，绝不用 `println!`/`eprintln!`/`dbg!`，会破坏渲染或协议。改用 `tracing`：

```rust
use tracing::{debug, error, warn};

error!(url = %url, method = %method, "MCP request failed");
warn!(path = %path.display(), "Theme file invalid, using fallback");
```

无需进入 TUI、执行后即退出的独立 CLI 命令可以用 `println!` 或直接写 `stdout`/`stderr`。结构化 stdout 必须保持干净。这个例外由**语义**决定，不是由文件名决定——共享代码一律用 `tracing` 或显式传入的输出 sink，不能因为文件名像是"独立命令"就假定安全。
