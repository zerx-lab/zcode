# 证据源

采集时间:2026-08-06。

## 上游仓库

| 项目 | 用途 | commit | 日期 | 本地路径(临时) |
|---|---|---|---|---|
| `openai/codex` | Rust + ratatui 的量产实现。fork Terminal、DECSTBM 注入、Windows VT、Unix job control | `928bda82cf54e62ec1a96afd8e12f544441056df` | 2026-08-05 | `/tmp/codex-tui/codex-rs/tui/` |
| `helix-editor/termina` | 后端候选(已排除)。Windows 输入实现 | `974b0ab8c27ae9bb8d224d020a5d58ffdd06bb31` | 2026-07-20 | `/tmp/termina/` |
| `helix-editor/helix` | termina 的参考消费者。证明 Windows 上仍用 crossterm | `079a789e8cb08ead67f19e1971a1b7438b37354b` | 2026-07-23 | `/tmp/helix/` |
| `AnswerDotAI/teleprint` | Python,设计笔记参考 | — | — | 仅读 README |

临时路径不持久。需要复查时重新 clone:

```bash
git clone --depth 1 --filter=blob:none --sparse https://github.com/openai/codex.git
cd codex && git sparse-checkout set codex-rs/tui
git clone --depth 1 https://github.com/helix-editor/termina.git
git clone --depth 1 --filter=blob:none --sparse https://github.com/helix-editor/helix.git
cd helix && git sparse-checkout set helix-view helix-term helix-tui
```

## 本地仓库

| 项目 | 用途 | 路径 |
|---|---|---|
| `oh-my-pi` | TypeScript 自研引擎。C/W/B 账本、四条 emitter、ED3 手势重放、架构合同文档 | `C:/Users/zero/Desktop/code/github/oh-my-pi/` |

关键文件:

- `docs/tui-core-renderer.md` — 渲染合同,338 行。**最重要的单一文档**
- `docs/tui-runtime-internals.md` — 运行时:requestRender 节流、compose→audit→commit→emit 流水线
- `packages/tui/src/tui.ts` — 引擎核心,4273 行
- `packages/coding-agent/src/modes/controllers/input-controller.ts` — ctrl+o 链路
- `packages/coding-agent/src/tools/render-utils.ts` — `previewWindowRows` / `capPreviewLines`

## crate 源码

均在 `C:/Users/zero/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/`:

| crate | 版本 | 引用到的文件 |
|---|---|---|
| ratatui | 0.30.2 | `Cargo.toml`(feature 列表) |
| ratatui-core | 0.1.2 | `src/backend.rs`(ClearType、scroll_region 契约)、`src/terminal/{viewport,inline,render,resize,buffers,init}.rs` |
| crossterm | 0.29.0 | `src/{command,ansi_support,terminal,event,lib}.rs`、`src/terminal/sys/{unix,windows}.rs` |

Rust std:`C:/Users/zero/.rustup/toolchains/stable-x86_64-pc-windows-msvc/lib/rustlib/src/rust/library/std/src/sys/io/is_terminal/windows.rs`

## 关键引用索引

按主题归类,便于复查。

### C/W/B 账本

| 断言 | 位置 |
|---|---|
| 三量定义 | `oh-my-pi/docs/tui-core-renderer.md:41-50` |
| unpinned 的窗口/提交数学 | 同上 `:52-53` |
| 三区语义(exact / frozen / 窗口) | 同上 `:47-49, 54-56` |
| pinned 才 clamp 到 B | `oh-my-pi/packages/tui/src/tui.ts:3093` |
| 接受的代价 | `docs/tui-core-renderer.md:60-70` |
| 审计与 resync | `tui.ts:3264-3276` |
| 提交数学实现 | `tui.ts:3049-3153` |

### 四条 emitter

| 断言 | 位置 |
|---|---|
| 路径表 | `docs/tui-core-renderer.md:95-98` |
| ED3 唯一 callsite | 同上 `:100-101, 157-159` |
| 普通 update 禁发 ED2/ED3/绝对 home | 同上 `:119-120` |
| divergence rebuild 默认关闭 | 同上 `:110-117` |
| ratatui 也要求每帧完整渲染 | `ratatui-core/src/terminal/render.rs:43-47` |

### DECSTBM

| 断言 | 位置 |
|---|---|
| 三段式实现 + 图示 | `codex-rs/tui/src/insert_history.rs:217-245` |
| RI 反向推 viewport | 同上 `:196-215` |
| cursor-position-neutral 契约 | 同上 `:236` |
| 自定义 Command | 同上 `:333-369` |
| 滚动区含 row 0 才进 scrollback | `ratatui-core/src/backend.rs:340-343` |
| 该行为非标准强制 | 同上 `:382-383` |
| per-batch 模式选择 | `codex-rs/tui/src/tui.rs:908-913` |
| `ZellijRaw` 路径与理由 | `insert_history.rs:51-53`(为何需要)、`:164-193`(实现) |
| `ZellijRaw` 的安全条件:先清 viewport + 同帧替换 | 同上 `:165-167` |
| `ZellijRaw` 预留空行 | 同上 `:180-182` |
| 两条路径的绝对 `MoveTo` | `Standard` `:203, 237, 245`;`ZellijRaw` `:169, 183` |
| 为何用 `MoveTo` 而非 `set_cursor_position` | 同上 `:234-236` |
| 两种 wrap policy(`PreWrap` / `Terminal`) | 同上 `:43-46, 51-52` |
| mux 只禁 ED3 | `oh-my-pi/packages/tui/src/tui.ts:12-13` |

### ctrl+o 链路

| 跳 | 位置 |
|---|---|
| 键绑定 | `oh-my-pi/packages/coding-agent/src/config/keybindings.ts:124-126` |
| 编辑器截获 | `modes/components/custom-editor.ts:911-913` |
| 控制器接线 | `modes/controllers/input-controller.ts:459-460` |
| 派发 + `resetDisplay()` | 同上 `:1906-1921`,注释在 `:1916-1921` |
| 置 clear 标志 | `packages/tui/src/tui.ts:1899` |
| ED3 + 不截断重放 | 同上 `:3599-3603, 3646-3650` |

### 固定高度活跃区

| 断言 | 位置 |
|---|---|
| tail window 高度 = 终端行数 − chrome | `oh-my-pi/packages/coding-agent/src/tools/render-utils.ts:222-225` |
| 丢头保尾 + 隐藏计数 | 同上 `:239-248` |
| 契约注释("only ctrl+o uncaps") | `packages/coding-agent/src/tools/bash.ts:1783-1785` |
| 违反的真实事故 | `packages/coding-agent/CHANGELOG.md:2342` |
| 修复方案 | 同上 `:3530` |

### Windows VT

| 断言 | 位置 |
|---|---|
| codex 的独立启用 | `codex-rs/tui/src/tui.rs:1137-1177` |
| 在 `set_modes()` 第一行 | 同上 `:217-218` |
| 每个裸 escape 入口重新确保 | 同上 `:298, 942, 1003, 1022, 1044, 1073` |
| `GetConsoleMode` 失败静默通过 | 同上 `:1154-1156` |
| 自定义 Command override | `insert_history.rs:345-349`、`tui.rs:255-258, 276-279` |
| crossterm 唯一 VT 启用点 | `crossterm/src/ansi_support.rs:16-27`,唯一调用点 `:39` |
| `Once` 守卫 | 同上 `:34` |
| `TERM` fallback(OR,无 tty 门) | 同上 `:39-40` |
| 默认 `is_ansi_code_supported` | `crossterm/src/command.rs:34-36` |
| ANSI/WinAPI 分派 | 同上 `:122-123, 289-291` |
| std 识别 MSYS pty | `std/.../is_terminal/windows.rs:21-22, 25-69` |
| helix 用 Err 而非 panic | `helix-tui/src/backend/crossterm.rs:461-464` |

### Unix job control

| 断言 | 位置 |
|---|---|
| `SUSPEND_KEY` | `codex-rs/tui/src/tui/job_control.rs:25` |
| 事件流截获 | `codex-rs/tui/src/tui/event_stream.rs:256-258` |
| `suspend()` 全流程 | `job_control.rs:64-99` |
| alt screen 退出 + resume action | 同上 `:65-72` |
| `kill(0, SIGTSTP)` | 同上 `:200-207` |
| resume 后重探测光标 | 同上 `:83`,理由注释 `:78-82` |
| `reapply_raw_mode_after_resume` | `codex-rs/tui/src/tui.rs:344-348`,竞态说明 `:339-343` |
| `tcflush` | 同上 `:376-382` |
| 只用 libc,不用 signal-hook | `codex-rs/tui/Cargo.toml:127-128` |

### 光标位置探测(`terminal_probe.rs`,966 行)

| 断言 | 位置 |
|---|---|
| `cursor_position` 实现 | `codex-rs/tui/src/terminal_probe.rs:240-244` |
| 只在 event stream 缺席/暂停时运行 | 同上 `:10`、`:269-271` |
| `DEFAULT_TIMEOUT = 100ms` | 同上 `:18` |
| 超时/无响应返回 `Ok(None)` | 同上 `:284-289` |
| `Tty::open` dup stdin/stdout,回退 `/dev/tty` | 同上 `:62-115` |
| 只把 reader 设 `O_NONBLOCK`,Drop 恢复原 flags | 同上 `:66-67, 128` |
| `poll_readable` 用 `libc::poll` | 同上 `:171-189` |
| `dup_file` | 同上 `:212-218` |
| resume 时 focus report 可能先到,复用 startup parser | 同上 `:236-239` |
| Windows 路径用 raw console handles(我们不需要) | 同上 `:592-598, 726-737` |

### 后端决策

| 断言 | 位置 |
|---|---|
| codex 的 ratatui feature 集 | `codex-rs/tui/Cargo.toml:82-87` |
| codex 全平台单一后端 | 同上 `:71` |
| helix 编译期后端分叉 | `helix-term/src/application.rs:47-61, 109-116` |
| 开 termina 仍编译 crossterm backend | `helix-tui/src/backend/mod.rs:17-20` |
| helix Windows 依赖 crossterm | `helix-term/Cargo.toml:93` |
| crossterm Kitty 协议 | `crossterm/src/event.rs:292-527` |
| `supports_keyboard_enhancement` 双实现 | `crossterm/src/terminal/sys/unix.rs:188-190`、`sys/windows.rs:75-77` |
| termina 类型化 DECSTBM | `termina/src/escape/csi.rs:802-803, 893-897` |
| termina 类型化 ED3 | 同上 `:1175-1176` |
| termina Windows 丢键 | `termina/src/parse/windows.rs:54-58` |
| `windows-legacy` 语义 | 同上 `:24, 30-38`;`termina/src/terminal/windows.rs:415-416` |
| termina 唯一输入示例需 legacy | `termina/Cargo.toml:57-59` |
| ratatui feature 存在性 | `ratatui-0.30.2/Cargo.toml:88-138` |

### 测试方法

| 断言 | 位置 |
|---|---|
| codex 的 VT100Backend + insta | `codex-rs/tui/src/insert_history.rs:918-941, 957-981` |
| codex dev-dep vt100 | `codex-rs/tui/Cargo.toml:164` |
| shadow commit ledger | `oh-my-pi/docs/tui-core-renderer.md:243-251` |
| 验证要求(不能单终端单 seed) | 同上 `:255-258` |

### 反面教材

| 断言 | 位置 |
|---|---|
| 探测滚动位置的失败史 | `oh-my-pi/docs/tui-core-renderer.md:28-36` |
| 已删除的品牌探测 | 同上 `:205-208` |
| 已移除的 env 开关 | 同上 `:315-317` |
| Windows Terminal 历史 DECSTBM bug | `microsoft/terminal#1849`,已由 PR #1881 修复 |

## crates.io 维护数据

采集方式:

```js
fetch(`https://crates.io/api/v1/crates/${name}`)  // crate.updated_at / max_stable_version / recent_downloads
```

GitHub 活跃度:

```js
fetch(`https://api.github.com/repos/${repo}/commits?per_page=1`)  // commit.author.date
```

结果表见 `dependencies.md`。
