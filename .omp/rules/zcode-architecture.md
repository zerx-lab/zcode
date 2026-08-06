---
description: ZCode 特有构件约束：crate 职责与导入边界、worker 子进程重入 CLI、prompt 存静态 .md、生成物禁改、中央工具函数、TUI 输出清理。新增 crate / 动 worker / 改 prompt / 改渲染路径 / 改生成物前必读。
---

# ZCode 构件与运行时约束

## Crate 职责表

目标 workspace 由 9 个 crate 组成；下表是**目标形态**，crate 尚未落盘，路径均标 `(planned)`。对应 crate 落盘后，从该行删除 `(planned)` 标记，不要整表一次性摘掉。

| Crate | 包名 | 职责 |
| --- | --- | --- |
| `crates/ai/` (planned) | `zcode-ai` | 支持流式传输的多提供商 LLM 客户端 |
| `crates/catalog/` (planned) | `zcode-catalog` | 模型目录：内置 `models.json`、提供商描述符、模型身份识别/分类 |
| `crates/agent/` (planned) | `zcode-agent` | 支持工具调用和状态管理的 Agent 运行时 |
| `crates/coding-agent/` (planned) | `zcode` | 主 CLI 应用程序，是首要关注对象 |
| `crates/tui/` (planned) | `zcode-tui` | 支持差分渲染的终端 UI 库 |
| `crates/text/` (planned) | `zcode-text` | 性能关键型文本、图像及 grep 操作 |
| `crates/stats/` (planned) | `zcode-stats` | 本地可观测性仪表盘（`zcode stats`） |
| `crates/schema/` (planned) | `zcode-schema` | JSON Schema 校验，惰性编译运行时 |
| `crates/utils/` (planned) | `zcode-utils` | 共享工具（日志、流、临时文件、进程包装） |

## Catalog 导入边界

内置模型、模型思考辅助函数、身份信息、描述符、模型管理器/缓存等**值**，一律从 `zcode_catalog::<module>` 导入，绝不经由 `zcode_ai` 的 re-export 导入。`zcode_ai` 的 `lib.rs` 仅 re-export 其自身签名用到的模型/effort **类型**（`Model`、`Api`、`ThinkingConfig`、`Effort` 等）；只需要这些类型时可从 `zcode_ai` 导入，需要值时必须绕过它直接打 `zcode_catalog`。

## Worker 子进程契约

Worker **必须重入 CLI 入口**，绝不编译独立的 worker 二进制。CLI 入口 crate（`crates/coding-agent/` (planned)）的 `main.rs` (planned) 在启动时通过 `zcode_utils::env::declare_worker_host_entry()` 将自身声明为 worker host，并在加载命令注册表之前分派隐藏的 argv selector（`__zcode_worker_stats_sync`、`__zcode_worker_tab`、`__zcode_worker_eval`、`__zcode_worker_tiny_inference`）。启动方必须走这条路径：

```rust
use zcode_utils::env::worker_host_entry;

let child = Command::new(worker_host_entry()?)
    .arg("__zcode_worker_<name>")
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .spawn()?;
```

进程经 zcode CLI 启动时（`cargo run`、`cargo install` 安装的二进制、发布的静态二进制均适用），`worker_host_entry()` 就是 `std::env::current_exe()`，worker 重入同一个二进制。不处于 CLI host 中时（`cargo test`、库嵌入、独立运行的 `zcode-stats`），该函数返回 `Err`，必须回退到进程内线程实现（`tokio::spawn` + 同一份 worker 逻辑）。新增 worker 类型时**必须**：把 selector 加入 `main.rs` (planned) 的分发表、保留进程内 fallback 分支、补一个同级 smoke 测试。

**历史缘由**（不得丢）：早期方案是每个 worker 声明独立 `[[bin]]` 目标，导致 `cargo install` 与打包脚本必须始终同步两套目标列表，漏掉一个就在安装后静默失败——这是为何所有 worker 现在统一走重入 CLI 入口这条路径。

**Smoke probe 契约**：`zcode --smoke-test` 启动 stats sync worker 和 tiny-model 子进程，各发一次 ping 后退出，已接入 `ci:test:smoke` 和 `scripts/install-tests/run-ci.sh` (planned)，因此二进制安装、`cargo install --path` 安装、crate 包安装都会执行该验证。新 worker 若不在同一模块图上，必须补一个同级 smoke 测试，否则该验证覆盖不到它。

## Prompt 必须存静态 `.md`

绝不在代码里拼 prompt——不用内联字符串、不用 `format!`、不做字符串拼接。Prompt 一律存放在静态 `.md` 文件中；动态内容用 Handlebars 模板变量。导入方式是 `include_str!("./prompt.md")`，绝不用运行时 `fs::read_to_string`（否则 prompt 会随发布环境漂移，且无法被编译期检查覆盖）。

## 生成物禁改

`crates/catalog/src/models.json` (planned) 由 `crates/catalog/build/generate_models.rs` (planned)（经 `cargo run -p zcode-catalog --bin gen-models` 调用）及 `crates/catalog/src/provider_models/` (planned) 下的描述符/解析器根据上游来源（stencil.so、提供商 catalog discovery、OpenCode 文档）生成，**绝不能手工编辑**，下次重新生成会覆盖手改内容。要改条目，改源头：

- 解析规则 / 按 ID 覆盖 → `crates/catalog/src/provider_models/openai_compat.rs` (planned) 中的解析器（如 `create_opencode_api_resolution` 的 ID 覆盖映射）。
- 提供商 catalog 条目（默认模型、discovery factory/flag）→ `crates/catalog/src/provider_models/descriptors.rs` (planned) 的 `CATALOG_PROVIDERS` 表。
- 生成器级修正（premium multiplier、Codex 定价 fallback、fallback 模型、后处理）→ `crates/catalog/src/bin/gen_models.rs` (planned)。
- 思考元数据 / 生成策略 → `crates/catalog/src/model_thinking.rs` (planned)（`apply_generated_model_policies`）；模型 ID 分类（family/version 解析）→ `crates/catalog/src/identity/classify.rs` (planned)。

改完用 `cargo run -p zcode-catalog --bin gen-models` 重新生成，把 `models.json` (planned) 与源码修改一并提交。回归测试要打在**解析器/描述符**上，不要打在内置 JSON 上，这样上游元数据变化后测试仍然有效。

## 中央工具函数优先

写 helper 前先搜现有实现——`crates/coding-agent/src/utils/` (planned)、`zcode-utils`、`zcode-tui`，以及调用点附近的领域模块。此规则适用于**所有内容**：VCS 包装器、格式化/截断/路径显示辅助函数、图像处理、剪贴板、流、临时文件、缓存。中央实现里包含的健壮性处理（超时、输出上限、非交互式环境判断、锁规避、缓存、TUI 清理）是新拷贝一份通常会丢失的东西。

- 先用 `grep` 搜对应操作；两个实现都能跑也算重复实现，是缺陷。
- git/jj 只能经统一包装层调用：`src/utils/git.rs` (planned)、`src/utils/jj.rs` (planned) 是唯一许可入口，绝不手写 `Command::new("git")`。
- 缺能力就扩展中央 helper（加参数或加子函数），不要局部复制分叉逻辑。

## TUI 输出清理

所有显示在工具渲染器中的文本必须清理：制表符会造成视觉空洞，长行会溢出，路径会泄露用户主目录。

- 制表符 → 空格：`replace_tabs()`。
- 截断：`truncate_to_width()` / `ui::truncate()`，用 `TRUNCATE_LENGTHS` 常量。
- 缩短路径：`shorten_path()`，把主目录替换为 `~`。
- 预览限制：统一用 `PREVIEW_LIMITS`，不得临时硬编码数字。
- 宽度计算：一律经 `unicode-width`，绝不用 `str::len()`。

这些规则必须应用到**每一条渲染路径**，不只是成功路径：成功输出（文件预览、命令输出、搜索结果）、**错误消息**（patch 失败消息常带未匹配的原始行，含文件内容就必须过 `replace_tabs()`）、diff 内容（新增/删除两侧）、流式预览。

**流式工具预览的多路径陷阱**：工具调用预览存在多条渲染路径——实时事件路径和 transcript 重建路径都要走 `decode_streamed_tool_args` / `ToolArgsRevealController`（`modes/controllers/tool_args_reveal.rs` (planned)）解码显示参数，绝不能把提供商已解析的 `arguments` 与原始 `partial_json` 并列展开（已解析参数受节流解析窗口影响，会滞后于流）。新增仅用于预览的字段或依赖部分流式参数时，必须同步更新所有路径，不能只改最终渲染器。

对 bash 工具尤其要注意：待执行预览可能需要原始 `partial_json` 而非已解析的 `arguments`——已解析参数要等到 JSON 对象闭合才会出现，内联环境变量赋值可能到最后一刻才可见。仅用于预览的字段必须贯穿 `event_controller.rs` (planned)、`ui_helpers.rs` (planned) 中的 transcript 重建，以及 `tool_execution.rs` (planned) 中合并后的调用/结果渲染，遗漏任一条都会导致预览不一致；`ToolExecutionComponent::build_render_context()` 必须在结果为 `None` 时也能工作。每次改动 bash 预览，都要同时验证实时流式路径和重建 transcript 路径——只修一条不算修好。

## 日志与 CLI 输出边界

TUI、RPC、SDK、worker 或后台运行时处于活动状态期间可能执行的代码，绝不用 `println!`/`eprintln!`/`dbg!`，会破坏渲染或协议。改用 `tracing`：

```rust
use tracing::{debug, error, warn};

error!(url = %url, method = %method, "MCP request failed");
warn!(path = %path.display(), "Theme file invalid, using fallback");
```

无需进入 TUI、执行后即退出的独立 CLI 命令可以用 `println!` 或直接写 `stdout`/`stderr`。结构化 stdout 必须保持干净。这个例外由**语义**决定，不是由文件名决定——共享代码一律用 `tracing` 或显式传入的输出 sink，不能因为文件名像是"独立命令"就假定安全。
