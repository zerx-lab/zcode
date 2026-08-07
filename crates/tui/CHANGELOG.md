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
- 主题与视觉原语，取值全量对标 oh-my-pi（`packages/coding-agent/src/modes/theme/`）：
  - `theme`：66 键调色板（`vars` → `colors` 两层间接、递归解析带环检测）、
    truecolor / 256 两档色深与自研的 256 量化、`statusLineBg` 感知亮度定亮暗、
    内置 `dark` / `light` 两套 JSON；
  - `theme::symbols`：`unicode` / `nerd` / `ascii` 三档全量符号表（框线、树线、状态、
    导航、分隔符、markdown、徽章、勾选、思考档位、工具图标、两组 spinner 帧）；
  - `markdown`：pulldown-cmark（GFM）→ `Vec<Line>`，覆盖标题三级形态、强调、行内代码、
    围栏代码块、有序/无序/任务列表（悬挂缩进按 bullet 实际显示宽）、引用块、表格
    三段式列宽、水平线、链接同文去重；
  - `highlight`：syntect `ParseState` → `Span`，13 条 scope 前缀规则映射到主题的
    11 个语义色，scope→类别按 `Scope` 缓存；
  - `card`：圆角卡片（标题嵌顶边、分节分隔行、状态底色）与工具头行 `render_status_line`，
    外加 `truncate_line` / `pad_line` / `patch_line` 三个行级原语。
- `examples/theme_gallery.rs`：视觉验收用例，把调色板、状态头、三态卡片、用户气泡、
  markdown + 语法高亮、spinner 一次性画到真实终端，可切主题与符号档
  （`cargo run -p zcode-tui --example theme_gallery -- light nerd`）。
- `Emitter::render` 的活跃区高度改由 `ComposeOutcome::boundary` 得出，调用方传的
  参数降级为**下限**。让调用方自己数行数是错误设计：它必须把所有活跃组件预渲染一遍
  （双倍成本），而且估算与 `compose` 的实际结果必然漂移——漂移时 viewport 装不下活跃
  内容，顶部几行既不进历史也不进窗口，表现是消息凭空消失。
- `Emitter::render` 增加溢出保护：活跃区比整块屏幕还高时本帧降级 unpinned。
  pinned 语义把 `commit_end` 卡在 `B`，此时 `W > B`，`[B, W)` 会同时逃出历史与窗口。
  取舍沿用引擎既定的 duplication, never loss。回归测试
  `oversized_live_region_never_drops_rows` 钉住「每一行都还在」。
- `Emitter::shutdown`：收起活跃区并把光标停在它原来的顶行。退出路径必须调它，
  否则输入框边框与未关闭的 SGR 会烙在 shell 提示符上。
- `examples/render_cost.rs`：测量 markdown 排版与 syntect 高亮的真实单帧成本。
  上游那份「100 行 ~26 ms」含 FFI 往返与 ANSI 编解码，不能直接拿来给本仓做决策；
  实测（release）散文 40 行 84 µs、40 行代码 7.9 ms、200 行代码 40 ms，不高亮 425 µs
  ——「流式期间关掉高亮」这条决定就是从这组数字来的。

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
- 相对 oh-my-pi 的**刻意分歧**（各自在代码注释里写明理由与出处）：
  - 不预先把颜色烘焙成 ANSI 字符串。上游在构造主题时就拼好 `\x1b[38;2;…m`
    （`theme.ts:1509-1536`），代价是 `getContrastFgAnsi`（`theme.ts:1669-1675`）
    只能用正则从字符串里把 RGB 抠回来，256 色档抠不出来就丢失对比度保障。
  - `dim` 是**颜色**不是 `Modifier::DIM`：上游 `colors.dim` 就是具体色值
    （`dark.json:11,30`），映射成修饰符会同时丢掉颜色又叠上修饰符。
  - 不做全局主题单例。上游是 `export var theme` + 每个 getter 手写 `undefined` 守卫
    （`theme.ts:2178`、issue #2998），本仓把 `&Theme` 当参数传，预初始化状态不存在。
  - 水平线铺满完整宽度，不截到 `min(width, 80)`：那个 80 在上游无注释无依据，
    宽终端下看起来像遗留缺陷（`markdown.ts:2427`）。
  - 无序列表 bullet 从主题读。上游三处写法不一致——渲染器硬编码 `"- "`
    （`markdown.ts:2681`）、单行版用 `"• "`（`markdown.ts:3048`）、
    主题里还定义了用不上的 `md.bullet`（`theme.ts:380`）。
  - 嵌套列表对齐到父项正文起点，而不是上游固定的每级 2 空格：上游的嵌套缩进
    （`markdown.ts:2643`）与续行悬挂（`markdown.ts:2685`）在 `"10. "` 这类 4 列
    bullet 下对不齐。bullet 宽为 2 的常见情形两者结果相同。
  - 不移植上游为纯 ANSI 字符串模型打的补丁：`stylePrefix` 重开、`\x1b[0m` 之后补背景、
    `\x1b[39m` 单通道 reset。ratatui 的 `Span` 各自持有 `Style`，这一整类问题不存在。
  - OSC 8 超链接与 Kitty OSC 66 双倍字号未实现：`Buffer` 没有对应属性，
    硬塞进 `Span` 会被当普通字符计宽、破坏整个布局。
