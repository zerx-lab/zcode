# ZCode — Agent 索引

Rust 实现的 agent harness。**本文件是记忆索引，不是文档**：只放跨会话必需的坐标、命令、导航。
细则在 `.omp/rules/`，行为红线在 `.omp/RULES.md`（自动生效，无需手动读）。
索引维护契约见文末，**行数上限 120**。

## 现状

- 阶段：**workspace 骨架已落盘**，正在逐个 crate 填实现。根 `Cargo.toml` 集中管版本/依赖/lint；
  CI 闸门清单以 `.github/workflows/ci.yml` 为准。
- **本文件不记录瞬时状态**：门禁是否全绿、哪个 crate 写到什么程度、有哪些文件 —— 一律不写。
  那些跑一次 `/gate`、看一眼目录列表与 `git status` 就知道，写进来只会变成过时的错误记忆。
- 下一步：按 `rule://zcode-architecture` 的职责表逐个填实现；`crates/tui/` 的设计事实源是 `plans/tui/`。

## 命令

工具链版本由 `rust-toolchain.toml` 固定（rustup 自动切换）。未引入 `just` / `xtask` 之前，任何地方都不要写它们的命令。

| 目的     | 命令                                                                |
| -------- | ------------------------------------------------------------------- |
| 编译检查 | `cargo check --workspace --all-targets`                             |
| Lint     | `cargo clippy --workspace --all-targets --all-features -- -D warnings` |
| 格式化   | `cargo fmt --all`（校验用 `cargo fmt --all --check`）                |
| 测试     | `cargo nextest run --workspace`                                     |
| Doctest  | `cargo test --doc --workspace`（doctest 不进 nextest）              |
| 文档     | `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features`（rustdoc 的私有链接/歧义链接只在这里报） |
| 交叉 lint | `rustup target add <另一平台的 triple>` 后 `cargo clippy -p zcode-utils --lib --all-features --target <同一 triple> -- -D warnings`（选哪个 triple 与理由见 `/gate` 第 7 步） |
| 依赖审计 | `cargo deny check`、`cargo machete`                                 |
| 交叉编译检查 | `cargo check --workspace --all-targets --target <triple>`（目标清单见 CI 的 cross-check job） |
| 索引校验 | `bun .omp/checks/index-guard.check.ts`                              |

一次跑全套：`/gate`。聚焦验证：`cargo nextest run -p <crate> <filter>`。

## 知识分层

| 要找什么                                              | 去哪                                                  |
| ----------------------------------------------------- | ----------------------------------------------------- |
| 行为红线（提交、装完成、验证义务）                    | `.omp/RULES.md` — always-apply，已在上下文里           |
| 参考仓坐标与实现路由（三个真实仓库 + 线索表）        | `.omp/rules/reference-first.md`（`rule://reference-first`）— 写非平凡实现前必读，`/ref` 派调研 |
| 通用 Rust 质量约束（错误、转换、可见性、异步、依赖）  | `rule://rust-quality`                                 |
| 测试契约与并行安全                                    | `rule://rust-testing`                                 |
| ZCode 特有构件约束（crate 边界、worker、prompt、TUI） | `rule://zcode-architecture`                           |
| 交付流程（GitHub、commit、changelog、发布）           | `rule://zcode-workflow`                               |
| 索引本身怎么维护                                      | `rule://agents-index`                                 |
| panic / `as` 写法替代方案                             | `rule://rust-forbidden-patterns` — 写 `.rs` 首次命中时注入 |
| omp harness 配置分层与理由                            | `.omp/README.md`                                      |

## 仓库地图

一行一条：`<反引号路径>` — 职责 — 主要入口符号。只写**已存在**的路径；
计划中的 crate 布局归 `rule://zcode-architecture`，不要提前搬进这里。

- `crates/` — 十个 workspace 成员；职责表、导入边界、worker 契约、**进程边界**见 `rule://zcode-architecture`。CLI 入口是 `crates/coding-agent`（包名 `zcode`），同时是所有 worker 的 host 二进制
- `crates/protocol/` — 客户端 ↔ 运行时的唯一编译边界：wire 类型全归它，依赖方向 `tui -> protocol <- runtime`，两侧不得绕过它互相直连
- `crates/utils/src/transport/` — 跨平台本机 IPC：Unix socket 与 Windows named pipe 包装成同名类型，上层零 `cfg`。`stream_pair()` 让 headless 与 TUI 共用同一个连接处理函数。不在此层做探活：Windows 上先探一次会占掉唯一 pipe 实例，随后的 connect 会卡在 `ERROR_PIPE_BUSY`
- `crates/utils/src/env.rs` — worker 子进程重入 CLI 的路径解析：`declare_worker_host_entry` / `worker_host_entry`
- `crates/ai/src/auth/store.rs` — 凭据文件落在用户主目录的 `.zcode` 下，`ZCODE_AUTH_FILE` 可覆盖；读-改-写全程持排他文件锁 + OAuth 刷新走 CAS。别退回"无锁整文件重写"：多进程并发刷新会丢轮换后的 refresh token
- `crates/ai/src/http.rs` — 全 crate 唯一的 `reqwest` 客户端。TLS 走 `rustls-no-provider` + ring 并在建 Client 前装 provider：默认的 `rustls` feature 会拖进 aws-lc-sys（需 NASM/CMake）。同理不用 `dirs`（依赖 MPL-2.0 的 option-ext，被 `deny.toml` 挡）
- `crates/agent/src/session/` — 会话持久化 = **JSONL 条目树**（`parent_id` 成树，上下文 = 根到 head 的路径，`/branch` 与 `/rewind` 只换 head）。已否决 snapshot+journal 与 SQLite 事件溯源，取舍见 `plans/runtime-boundary/README.md` 第 7 节
- `crates/agent/src/approval.rs` — 审批 = **tier × policy，默认 yolo**；`always` 的授权键是 `(工具名, 工具声明的作用域)`。已显式否决有序 allow/deny/ask ruleset，别再造一套
- `crates/agent/src/interrupt.rs` — `InterruptSignal`（AtomicBool + epoch + Notify）。持有它的 turn 结束时**必须自己 reset**，没有别人会清；不清则下一个 turn 秒退
- `crates/agent/src/cancel.rs` — 取消入口。取消请求只带 session id，必须经 `CancelRegistry::cancel_session`：它先递归打后台作业（循环到无新增）再打 runner，别在别处直接 fire 单个信号
- `crates/protocol/src/wire/` — 所有 wire 变体；与 `crates/agent` 的落盘类型是**两套形状**，互转归 host adapter。改形状要跑 `ZCODE_UPDATE_WIRE_SCHEMA=1` 刷 `crates/protocol/tests/wire-schema.json` 并判 major/minor
- `crates/utils/src/daemon.rs` — daemon 端点原语：注册文件、单实例锁（`File::try_lock`，**不删锁文件**）、双条件回收、一次性就绪握手、握手 HMAC。拿锁必须先于一切副作用
- `crates/catalog/src/models.json` — 生成物；源头、生成命令与可复现前提见 `rule://zcode-architecture`
- `crates/catalog/src/effort.rs` — `Effort` 是 workspace 唯一的推理档位类型，`zcode-ai` 只 re-export；`Effort::Off` 的线上值是 `"none"`
- `crates/text/src/width.rs`、`crates/text/src/truncate.rs`、`crates/text/src/path.rs` — 显示宽度 / 输出截断 / 路径脱敏的唯一实现，渲染点一律调它们；要求见 `rule://zcode-architecture` 的「TUI 输出清理」
- `crates/tui/src/emit.rs` — 四条发射路径的调度器，也是全 crate 唯一发 ED3（`CSI 3J`）的地方；改渲染路径前先读 `crates/tui/src/lib.rs` 顶部的五条不变量
- `crates/tui/src/terminal.rs` — fork 自 codex 的 `Terminal`，但 `draw` 只发相对光标移动（上游发绝对 `MoveTo`）：绝对定位会把已向上滚动的读者拽回底部
- `crates/tui/src/wrap.rs` — `line_rows` 与 `wrap_line` 必须共用同一套贪心切分；行数被 `insert_history` 用来推进 viewport 锚点，算术公式会少记行
- `crates/schema/src/compile.rs` — JSON Schema 校验是 fail-closed：schema 形状非法或 `pattern` 编译不了都在 `compile` 期报错，绝不降级成跳过约束
- `.github/workflows/ci.yml` — 闸门编排：fmt、三平台 clippy/test、cross-check（目标清单以该 workflow 为准）、MSRV（从 `rust-version` 读）、docs、deny、machete、index-guard、dep-boundary
- `.github/dependabot.yml` — cargo 与 actions 每周更新，次版本/补丁合并成一个 PR
- `.config/nextest.toml` — nextest profile：`default`（本机）与 `ci`
- `plans/tui/` — TUI 的调研与实施计划（架构、模块、平台、依赖、来源）；`crates/tui/` 的设计事实来源
- `plans/runtime-boundary/` — 运行时与 UI 解耦的三仓调研原文 + 已裁决决策 + 分期实施计划；daemon 相关设计的事实来源
- `.omp/` — agent 协作层：红线、领域规则、专用 subagent、slash 命令、索引守卫；分层理由见 `.omp/README.md`

## 完成定义

1. `cargo check --workspace --all-targets` 通过；
2. clippy 零告警（`-D warnings`）；
3. 覆盖改动契约的测试通过；只有引入新的可观察契约时才新增测试；
4. 受影响 crate 自己的 CHANGELOG（如 `crates/utils/CHANGELOG.md`）已更新 — 见 `rule://zcode-workflow`；
5. 若改动影响本索引的坐标，同步更新本文件并刷新文末锚。

## 索引维护契约

**写入条件**（三者同时满足才写）：跨会话仍成立、无法由工具强制、能用一行说清。
典型：路径 → 职责、决策及其理由、已踩过两次的坑。

**不写**：一次性任务状态、clippy / rustfmt / 测试能强制的规则、超过 3 行的细则（→ `.omp/rules/`）、
尚未验证的猜测、任何粘贴的代码块。

**删除条件**（满足任一即删该条目，无需征询）：

- 反引号路径已不存在，且该行没有 `(planned)` 标记；
- 与代码或 `.omp/rules/` 冲突 —— 代码是事实来源，索引让路；
- 所列命令跑不通；
- 条目描述的决策已被替换。

**硬约束**：本文件 ≤120 行；单节超 25 行就拆进 `.omp/rules/`；不复制 clippy / rustfmt 能管的规则。

**自动检查**：`.omp/extensions/index-guard.ts` 校验行数、路径存在性、**顶层目录是否已有坐标**、锚新鲜度。
`write` 超长版本直接被拒；会话收尾前若仍不同步会**自动续跑一轮对账**。
人工或 CI 跑 `bun .omp/checks/index-guard.check.ts`。确实不该入索引的目录用 `<!-- index-ignore: <名字> -->`。

**维护动作**：`/sync-index` 派 `index-keeper` 与代码对账，补新坐标并删过时条目。
锚指向最后一次与索引对过账的 commit；`/sync-index` 负责推进它。

<!-- index-verified: 03b21e9 2026-08-06 -->
