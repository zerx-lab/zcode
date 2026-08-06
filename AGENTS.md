# ZCode — Agent 索引

Rust 实现的 agent harness。**本文件是记忆索引，不是文档**：只放跨会话必需的坐标、命令、导航。
细则在 `.omp/rules/`，行为红线在 `.omp/RULES.md`（自动生效，无需手动读）。
索引维护契约见文末，**行数上限 120**。

## 现状

- 阶段：**workspace 骨架已落盘** —— 根 `Cargo.toml` 集中管版本/依赖/lint，`crates/` 九个成员全部通过
  fmt / clippy / nextest / doctest / deny / machete；CI 覆盖三平台原生矩阵、交叉编译 check 与 MSRV。
  不要在本文件枚举仓库里有哪些文件：那种陈述必然过时，仓库现状看目录列表。
- 唯一已实现的业务逻辑是 `zcode-utils` 的 worker host 入口解析；其余七个 crate 只有骨架与职责文档，
  CLI 只有 `--help` / `--version`。
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
| 依赖审计 | `cargo deny check`、`cargo machete`                                 |
| 跨平台   | `cargo check --workspace --all-targets --target <triple>`（目标清单见 CI 的 cross-check job） |
| 索引校验 | `bun .omp/checks/index-guard.check.ts`                              |

一次跑全套：`/gate`。聚焦验证：`cargo nextest run -p <crate> <filter>`。

## 知识分层

| 要找什么                                              | 去哪                                                  |
| ----------------------------------------------------- | ----------------------------------------------------- |
| 行为红线（提交、装完成、验证义务）                    | `.omp/RULES.md` — always-apply，已在上下文里           |
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

- `crates/` — 九个 workspace 成员；职责表、导入边界、worker 契约见 `rule://zcode-architecture`。CLI 入口是 `crates/coding-agent`（包名 `zcode`），同时是所有 worker 的 host 二进制
- `crates/utils/src/env.rs` — worker 子进程重入 CLI 的路径解析：`declare_worker_host_entry` / `worker_host_entry`
- `.github/workflows/ci.yml` — 闸门编排：fmt、三平台 clippy/test、cross-check（目标清单以该 workflow 为准）、MSRV（从 `rust-version` 读）、docs、deny、machete、index-guard
- `.github/dependabot.yml` — cargo 与 actions 每周更新，次版本/补丁合并成一个 PR
- `.config/nextest.toml` — nextest profile：`default`（本机）与 `ci`
- `plans/tui/` — TUI 的调研与实施计划（架构、模块、平台、依赖、来源）；`crates/tui/` 的设计事实来源
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

<!-- index-verified: 7594a90 2026-08-06 -->
