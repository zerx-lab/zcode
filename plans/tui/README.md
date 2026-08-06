# Transcript-first TUI 调研与实施计划

目标:构建一个 Rust 终端 UI,transcript 落在终端**原生 scrollback**(不进 alternate screen),底部保留可重绘的活跃区,且支持 `ctrl+o` 折叠/展开已滚出屏幕的历史内容。

参照实现:oh-my-pi(TypeScript,自研引擎)、OpenAI codex(Rust + ratatui fork)。

目标平台:macOS / Linux / Windows。开发环境:Windows。

## 文档索引

| 文件 | 内容 |
|---|---|
| `architecture.md` | 渲染模型:C/W/B 账本、四条 emitter 路径、DECSTBM 历史注入、五条不变量 |
| `dependencies.md` | crate 选型、维护活跃度数据、被排除方案及依据 |
| `modules.md` | 自研模块清单、抄源文件与行号、分期计划、验证矩阵 |
| `platform.md` | 四处分叉:Windows VT 启用 / Unix job control / Unix 光标探测 / per-batch Zellij |
| `sources.md` | 全部证据的本地路径与上游 commit |
| `research.md` | 调研过程与关键发现 |

## 核心决策

### 1. 用 ratatui,但 fork 它的 `Terminal`

保留 `Buffer` / `Line` / `Span` / `Paragraph` / `Widget` 作为排版原语;`Terminal` 必须自己实现,因为它假定独占一块固定矩形,而我们的 composed frame 长度可以是终端高度的几十倍,且需要控制"哪些行已交付给终端历史"。

codex 走的正是这条路:`custom_terminal.rs` 文件头保留了 ratatui 的 MIT 声明。

必开 feature:

```toml
ratatui = { version = "0.30", default-features = false, features = [
    "crossterm",
    "scrolling-regions",            # DECSTBM
    "unstable-backend-writer",      # backend_mut() -> &mut impl Write
    "unstable-rendered-line-info",  # widget 渲染后行数，用于高度测量
    "unstable-widget-ref",
] }
```

### 2. 后端:全平台 crossterm,零后端分叉

- Unix 上 crossterm 无能力短板:Kitty 键盘协议、focus change、bracketed paste 齐全。
- codex 三平台单一后端量产,macOS 是其主力平台。
- Windows 上的复杂组合键限制来自 ConPTY/终端本身,与后端选择无关——换 termina 也解决不了。
- helix 用 termina 是为了更前沿的终端特性,且它**接受**维护两套 backend;而我们的输出侧自己发 escape,后端只需提供 `impl Write`。

排除 termina 的具体依据见 `dependencies.md`。

**零后端分叉不等于零平台分叉。** 平台相关代码集中在四处,合计约 580 行(详见 `platform.md`):

| 分叉 | 位置 | 平台 | 行数 |
|---|---|---|---|
| VT 启用 | `caps.rs` | Windows only | ~120 |
| job control(Ctrl-Z suspend) | `job_control.rs` | Unix only | ~210 |
| 光标位置探测(`CSI 6n`) | `terminal_probe.rs` | Unix only | ~250 |
| 历史注入模式 | `insert_history.rs` 内 per-batch 决策 | 跨平台(按 env 事实) | ~10 |

`terminal_probe.rs` 是 `job_control.rs` 的硬依赖(`job_control.rs:83`),不是可选优化。第四处既非平台分叉也非启动时能力,列出因为它常被误当成平台能力。

### 3. 历史注入用 DECSTBM,viewport 零重绘

把滚动区限制在 viewport 之上,在区内打印,内容滚出区顶即进入 native scrollback。viewport 在区外,一个字节都不用重写。

这比 oh-my-pi 的 `insert_before`(需要重画 viewport)更优。

**但 DECSTBM 只让"追加"变便宜,不让"改写"变可能。** 要支持 `ctrl+o` 改写已提交内容,仍必须保留完整 transcript model、C/W/B 账本、frozen snapshot 语义,以及手势触发的 ED3 全量重放。

### 4. 能力模型:两项启动时能力 + 一项 per-batch 决策

```rust
/// 启动时判定一次，全程只读。
struct OutputCaps {
    /// 整条 ANSI emit 是否可用（光标相对移动、SGR、行级重写）。
    /// false -> 完全不进 inline TUI，退化为纯 stdout 打印。
    interactive_output: bool,
    /// ED3（CSI 3J）可用。任何 multiplexer 下为 false。
    scrollback_purge: bool,
}
```

历史注入模式**不是**启动时能力,每个 batch 独立计算(见 `platform.md`)。

`interactive_output == false` 时不做部分降级:emit 层直写裸字节,无 VT 时相对光标移动同样不被解析,所以只有"完整 TUI"和"纯 stdout"两态。

## 五条不变量

1. **ED3 单一 callsite。** `CSI 3 J` 只出现在手势驱动的全量重放里,永不在增量路径,永不在 multiplexer 下。
2. **普通增量帧零绝对定位。** in-window diff 与 scroll-append 的窗口重绘只用相对光标移动;`MoveTo(0,0)` / `Clear` 会把已向上滚动的读者猛拽回底部,而这个 bug 在本机大概率复现不出来。
3. **历史注入是唯一允许绝对定位的增量路径。** 共同要求:cursor-position-neutral(进函数存光标,出函数以绝对 `MoveTo` 恢复)。此外按模式各有附加条件——`Standard` 要求 `SetScrollRegion` 与 `ResetScrollRegion` 成对,绝对定位的影响被锁在 viewport 之上;`ZellijRaw` 不用滚动区,改为先清 viewport 再于**同一 draw pass** 内完整替换它。两者都不会拽动读者视图,但保护机制不同,不可混用。
4. **Windows VT 启用独立于 crossterm,在任何 escape 之前。** 跑过子进程后重新施加。`GetConsoleMode` 失败静默通过,`SetConsoleMode` 失败才报错。
5. **能力判定与 mode 施加分离。** 启动时定、全程只读;不在渲染路径反复探测终端。

## 明确不做的事

- **不探测终端滚动位置。** 没有跨终端 API 能查询它。oh-my-pi 的架构文档记录了旧引擎在这个不可观测变量上的失败史:每种猜测策略只是在 `yank / flash / corruption / invisible-until-resize` 四个失败家族之间来回交换。
- **不做终端品牌探测来选择渲染策略。** oh-my-pi 已删除该路线(`eagerEraseScrollbackRisk` / `PI_TUI_ED3_SAFE`)。env 只用于选择**优化**,而非正确性。
- **不让渲染器自作主张擦历史。** 内容漂移时宁可在 scrollback 留一行 stale,ED3 只由用户手势触发。oh-my-pi 的 divergence rebuild 默认关闭。
