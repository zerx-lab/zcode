# Changelog

格式与归属标注规则见 `rule://zcode-workflow`。

## [Unreleased]
### Breaking Changes

- 项目许可证从 `MIT OR Apache-2.0` 切换为 `AGPL-3.0-only`。

### Added

- CLI 入口骨架：启动时声明 worker host 入口，`--version` / `--help` 可用。
- **完整装配层落地**，`zcode` 现在能端到端跑一次对话。分层与各自的关键取舍：
  - `cli`：clap 命令面（默认命令 + `run` / `serve` / `auth` / `models` / `config` / `session`）、
    worker selector 分发、启动顺序。**会话选择在这一层**（`--resume` / `--continue` / 新建），
    不下放给 `render` 与 `app`——那会让同一段逻辑存在两份，第一次改 `--continue` 的定义就漂移。
  - `main.rs`：Windows 专线程 8 MiB 栈（Windows PE 主线程栈默认 1 MiB，provider 装配在 tokio
    接管前就能吃穿它，得到捕获不到的 `STATUS_STACK_OVERFLOW`；抄源 jcode `src/main.rs:83-85`）。
    手工建 runtime 而非 `#[tokio::main]`：`declare_worker_host_entry()` 与大栈线程都必须在 runtime 之外。
  - `config`：两层 TOML（全局 `~/.zcode/config.toml` ← 项目 `.zcode/config.toml` ← 少量
    `ZCODE_*` ← CLI flag）。逐字段覆盖；`approval.policies` 是 entry 级合并，`tools.disabled`
    整体替换。解析失败是硬错并带行号，**不静默回退默认值**。未知字段直接拒绝。
    不抄 jcode 那 180 个逐字段手写 env 覆盖（`crates/jcode-base/src/config/env_overrides.rs`）。
  - `workspace`：全 crate 唯一的路径解析入口。词法归一，**不 `canonicalize`**（它要求文件已存在、
    会解符号链接、在 Windows 上还会吐 `\\?\` 前缀）。越界返回 `outside_root: true` 而不是错误——
    越界是需要审批的正常分支，不是异常。
  - `prompt`：静态 `.md` + `include_str!`。**环境上下文（cwd / 平台 / 日期 / git）不进 system
    prompt**，走首条 user 消息的 `<system-reminder>` 并持久化（抄源 jcode
    `crates/jcode-base/src/session.rs:897-921`）：日期每天变，放 system 会天天打穿 prompt cache，
    且历史重放时会被改写成"今天"。`AGENTS.md` 向上遍历合并并**有字节上限**——两个上游都没有这个
    上限，是本仓补的。
  - `model`：本地目录 + 凭据 → `Arc<dyn Provider>`。**不联网**（模型发现推到 session 建好之后，
    `plans/runtime-boundary/implementation.md:93-94` 的秒开要求）。简写多候选时报错并列出候选，
    不静默选第一个；无凭据的错误文案带可执行的 `zcode auth login <provider>`。
  - `tools`：八个内置工具（`read` / `ls` / `write` / `edit` / `bash` / `glob` / `grep` / `todo`）。
    `tools::output` 是唯一的输出收尾入口，制表符展开 + 按**显示宽度**截断 + 行/字节封顶，
    错误消息同样过这一套。
  - `host`：per-session actor 持有 `AgentRuntime`（turn 属于 session 而非连接，客户端断开后
    turn 继续跑完）、`AgentEvent ↔ wire::Event` 穷尽互转、`handle_client` 三帧握手 + 取消先于
    Ack 分发 + `Lagged → Resync` 不断流；daemon 生命周期按不可交换顺序编排，被抢注即自杀。
  - `render`：headless 两种输出。text 模式 **stdout 只有模型文本**，工具/进度/压缩全走 stderr
    （jcode 把这些也写 stdout，`zcode run > out.txt` 拿到的东西因此是脏的）。broken pipe 静默
    结束而非把整轮弄挂。退出码 0 / 1 / 130。
  - `app`：TUI 客户端。不进 alternate screen，transcript 落终端原生 scrollback。
  - `smoke`：端到端冒烟，唯一一处把整条装配链串起来跑的测试——除 provider 外全部是生产实现，
    真的注册八个工具、真的读磁盘、真的走 wire 往返、真的落 JSONL。
- **TUI 视觉层对标 oh-my-pi**，`app` 的四类块全部改由 `zcode-tui::theme` 驱动，
  crate 内不再出现任何颜色字面量：
  - 用户消息：无前缀字符，整行铺满 `userMessageBg`、左右各 1 列 padding、上下各 1 行
    全宽背景空行。说话人靠**底色**区分而不是靠对齐一个装饰字符
    （oh-my-pi `modes/components/user-message.ts:46-51`）。
  - 助手消息：正文过 markdown 渲染（含 syntect 语法高亮），左 1 列 padding、上下不留白
    ——上下留白会在紧随其后的工具卡片上方多出一行（`assistant-message.ts:880` 的注释）。
  - 思考内容：`thinkingText` 色 + 斜体的 markdown，代码块不高亮。
  - 工具调用：圆角卡片，状态头嵌在顶边上，按 `pending`/`success`/`error` 三态铺底色。
  - 审批 / stdin 弹窗：同样是卡片，边框色区分「要做决定」（`warning`）与「在等输入」
    （`borderAccent`）。
  - 输入框：圆角边框，聚焦 `borderAccent` / 失焦 `borderMuted`；不画 prompt gutter
    （上游有边框时同样不画，`packages/tui/src/components/editor.ts:710,720`）。
  - 状态行：spinner 帧取自主题的符号档，不再硬编码 8 帧盲文转轮。
  - 主题在进入 raw mode 之前构造一次：色深看 `COLORTERM`/`WT_SESSION`/`TERM`，
    亮暗看 `COLORFGBG`（缺省暗色），符号档看 `ZCODE_SYMBOLS`（`unicode`/`nerd`/`ascii`）。

### 执行路径只有一条

headless 与 TUI 共用同一条路径（`plans/runtime-boundary/README.md:195` 已裁决）：
daemon 在就连它，不在就 `stream_pair()` 自托管接同一个 `handle_client`。
**没有** "headless 直接建 `AgentRuntime`" 的近路——同文档 `:179` 把 jcode 的那条绕行
（`src/cli/commands.rs:2362-2415`）明确列为不抄项，理由是两套执行路径 = 两套 bug。

### Fixed

以下几条都是**真机跑二进制**才暴露的：当时的 891 个单元/集成测试全绿，没有一条能
抓到它们。每条都补了会在退回旧写法时立刻变红的回归测试。

- **daemon fork 炸弹。** `Cli` 上的 `args_conflicts_with_subcommands = true` 语义是
  "本命令任何参数一出现就拒绝子命令"，**全局 flag 也算数**。于是 `spawn_daemon` 发出的
  `zcode --cwd X serve` 里的 `serve` 落进了 `prompt` 位置参数：子进程不去当 daemon，
  而是又跑一遍客户端流程 → 发现没有 daemon → 再 spawn 一个自己。进程表里迅速堆满
  `zcode.exe`，客户端永远等不到就绪。去掉那个开关（clap 默认行为本来就正确：
  第一个非 flag 词优先匹配子命令），并把 argv 改成子命令在前
  （`zcode serve --cwd X`）。两条测试钉住：全局 flag 在前时子命令仍被正确路由、
  `spawn_daemon` 实际发出的那条 argv 解析成 `Serve`。
  旧测试之所以漏掉：它只断言 `--model` 被收到，**没断言 `command` 是子命令**。
- **跨工作区串台。** daemon 端点原先是全机唯一的，而 `HostDeps` 的 `workspace` /
  `registry` / `prompts` 全由工作区根派生、`SessionCreate` 又忽略客户端自报的 `cwd`
  （那是防越权的安全约束）。两者叠加的后果：**先启动的那个工作区成为所有客户端的工作区**——
  实测在 B 项目里跑 `zcode`，`ls` 列出的是 A 项目的文件。现在端点按工作区根分桶
  （`host::scoped_runtime_dir`），一个工作区一个 daemon。
  分桶前先规范化路径形态（Windows：剥 `\\?\`、`\`→`/`、转小写），否则父进程的
  `C:\tmp\projA` 与子进程 `--cwd` 拿到的 `C:/tmp/projA` 会哈希成两个桶，
  客户端在 A 桶等就绪、daemon 在 B 桶注册，一样是无限挂起。Unix 不做任何折叠：
  那里 `\` 是合法文件名字符、路径大小写敏感，折叠反而会造成本函数要防的串台。
- **`ReadyChannel` 在 Windows 上必然超时**（修在 `zcode-utils`）。
  `transport::windows::Listener::accept` 在第一次 poll、任何 `await` 之前就
  `self.idle.take()`，而 `ReadyChannel::wait` 每 50 ms 用 `tokio::time::timeout` 轮一次
  （好在等待期间 `try_wait` 子进程）——第一次超时把 future 丢掉，空闲 pipe 实例就永久丢失，
  下一次 accept 直接 `BrokenPipe`。现在 `idle` 只在 `connect()` 成功返回**之后**才取走，
  `accept` 因此是取消安全的（`NamedPipeServer::connect` 本身由 tokio 保证）。
- **TUI 无视 `ui.show_thinking`。** headless 侧一直遵守这个开关（默认关），TUI 侧
  无条件把 `思考: …` 画进 transcript——同一个配置项两个客户端行为不一致。现在
  `run_tui` 收 `show_thinking`，且**在入库时**就把思考块滤掉而不是渲染时滤：
  留着不显示会让 `RevealPacer` 的 backlog 把这些字符算进去，流式展示节奏莫名卡顿。
- **环境上下文被渲染成用户自己说的话。** 会话开头那段 `<system-reminder>`
  （cwd / 日期 / git status）在 API 层是 user 消息、但带 `display_role: System`，
  UI 层必须显示成系统消息（契约见 `crates/agent/src/session/message.rs:212-217`）。
  `message_to_block` 原先直接丢掉 `display_role`，于是它顶着 `›` 前缀出现，
  看起来像用户自己打了十行 git status。
- **消息在多轮之后凭空消失。** 发一句、模型回复、再发第二句，第一句不见了。
  根因在 viewport 高度：那时由 `AppState` 自己数活跃区行数再传给 `Emitter::render`。
  加了气泡上下留白与卡片/输入框边框之后这个估算必然偏小，viewport 装不下活跃内容时
  顶部几行既没被提交进 scrollback、也没画进窗口，就此丢失且不会自愈。
  改成由 `compose` 的 boundary 直接得出（`zcode-tui` 侧），本层不再猜；顺带砍掉了
  「为了数行数而多渲染一遍」的双倍开销。回归测试 `every_message_survives_multiple_turns_on_a_short_terminal`
  在 24 行终端上跑三轮，断言每条消息都还在屏幕或 scrollback 里。
- **退出后输入框残留在 shell 提示符旁。** 活跃区里的东西从没提交进 scrollback，
  进程一走就原样烙在终端上——半个圆角框加上没关的边框 SGR，之后每一行都染色。
  退出路径现在统一调 `Emitter::shutdown()` 收起活跃区，且它排在退 raw mode 之前、
  覆盖每一条返回路径（含 `?` 冒泡与提前 return）。
- **流式期间每帧重跑语法高亮。** 助手正文带代码块时，syntect 实测 40 行 7.9 ms、
  200 行 40 ms（`cargo run -p zcode-tui --release --example render_cost`），单块就吃穿
  30 fps 的帧预算。改成流式期间不高亮、定稿那一帧再上色，与上游同一取舍
  （`packages/tui/src/components/markdown.ts:2008`）。

### Changed

- MSRV 从 1.92 提到 **1.95**（workspace `rust-version`，全体成员继承）。下限由依赖决定：
  `libsqlite3-sys 0.38` 的 build script 用 `cfg_select!`（Rust 1.95 稳定），且它自己没声明
  `rust-version`，Cargo 拦不住——继续写 1.92 只会让 CI 的 MSRV job 在编译期炸。
