# Changelog

格式与归属标注规则见 `rule://zcode-workflow`。

## [Unreleased]
### Breaking Changes

- 项目许可证从 `MIT OR Apache-2.0` 切换为 `AGPL-3.0-only`。

### Added

- 初始化 crate 骨架：继承 workspace 元数据与 lint 配置。
- transcript-first 渲染引擎落地，十个模块按 `plans/tui/` 的设计实现：
  - `caps`：启动时能力判定（`interactive_output` / `scrollback_purge`）与 Windows VT 启用，
    判定与施加分离；
  - `terminal`：fork 自 codex `custom_terminal.rs` 的 `Terminal`，viewport 是可变 `Rect`，
    暴露裸 writer；
  - `wrap`：span 感知的按显示宽度换行，宽度求值全部转调 `zcode-text`（不引入 `textwrap`）；
  - `insert_history`：DECSTBM 历史注入 + 非滚动区注入两条路径，模式逐 batch 计算；
  - `compose` / `ledger` / `audit`：segment 账本、C/W/B 窗口数学、committed-prefix 审计；
  - `emit`：四条发射路径的调度器；
  - `job_control` / `terminal_probe`（`cfg(unix)`）：Ctrl-Z 挂起与 `CSI 6n` 有界光标探测。
- `compose::normalize_line`：组件渲染出的行进入帧之前的唯一清洗点，
  `strip_ansi` → `sanitize_text` → 按整行列位展开制表符，全部转调 `zcode-text`。
  viewport diff、历史注入、full paint 重放、纯文本输出四条路径共用同一份已清洗的行。
- `examples/inline_demo.rs`：真机冒烟用例，在终端里跑一遍"底部固定高度活跃区 +
  transcript 进原生 scrollback"。
- `tests/shadow.rs`：影子提交账本，独立重算 C/W 并逐行比对屏幕上可观察的整条 tape；
  另含 full paint 三种几何、resize/`ctrl+o` 的 ED3 触发面、固定高度框的终端网格对齐断言。

### Changed

- 相对以下参考实现的**刻意分歧**（各自在代码注释里写明理由与出处）：
  - `Terminal::draw` 只发相对光标移动（`CUU`/`CUD`/`CUF`/`CUB`），不发 `CUP`/`CHA`。
    codex 每个绘制命令都发绝对 `MoveTo`，那会违反本仓不变量 2
    （`plans/tui/README.md:88`，上游依据 `oh-my-pi` `tui.ts:4007-4010,4130-4133`）。
  - ED3（`CSI 3 J`）的字节只存在于 `emit::Emitter::emit_full_paint`，`terminal` 模块里一个都没有：
    `terminal` 是公开模块，公开的 ED3 方法等于绕过手势与能力检查的后门。
  - full paint 保留 ED2。`oh-my-pi` 的 window 高度恒等于终端高度、重放覆盖每个可见行，
    所以能省掉 ED2 以免露出空屏；本引擎的 viewport 高度由调用方决定，重放覆盖不到它下方。
  - `line_rows` 与 `wrap_line` 共用同一套贪心切分，不用 `ceil(总宽度 / width)`：
    宽字符配奇数宽度、以及跨 span 的边界都会让算术公式少记一行，而这个计数直接决定
    viewport 锚点与上报的历史行数。
  - `SetScrollRegion` / `ResetScrollRegion` 的 `execute_winapi` 返回 `Err` 而不是 codex 的
    `panic!`（本仓 `clippy::panic` 为 `deny`；helix 也是这么做的）。
  - `area.top() == 0`（活跃区占满屏幕）时不走 DECSTBM：`CSI 1;0 r` 是非法参数，
    会被终端忽略，随后的历史行直接打进活跃区。改走非滚动区注入，它的两个附加保护条件
    在这里天然成立。
  - 不抄 codex 的 URL 特判换行、ConPTY 大帧截断、流式增量换行、resize 期间借用 alt screen
    （见 `plans/tui/modules.md:67-75`）。
  - `write_history_line` 的行级/span 级样式合并方向与 codex 相反：本仓是
    `line.style.patch(span.style)`（行级打底、span 覆盖），与 viewport 一侧 `Line` 的渲染
    语义一致。`Style::patch` 是参数一侧优先（`ratatui-core/src/style.rs:471-474`），
    codex 的 `span.style.patch(line.style)` 会让 span 级颜色被行级颜色吞掉，
    同一行在 viewport 和 scrollback 里显示成两种颜色。
  - 清洗顺序是 `strip_ansi` **先于** `sanitize_text`：OSC 在实践中多用 `BEL` 收尾，
    而 `BEL` 是 C0；反过来先清 C0 会删掉终止符，让状态机把 OSC 之后的正文一起吞掉
    （`"\x1b]0;title\x07plain"` 会退化成 `""`）。
  - 制表符按**整行**的当前列展开，不逐 span 各自从第 0 列起算：后者会把
    `["ab", "\t X"]` 的 tab 展成整 4 格。实现是把已展开前缀增量喂给同一个
    `expand_tabs`，制表位规则仍然只有 `zcode-text` 一份。
