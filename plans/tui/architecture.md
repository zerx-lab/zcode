# 渲染模型

## 1. 三个表面

| 表面 | 归属 | 可变性 |
|---|---|---|
| **原生 scrollback** | 终端 / tmux 拥有 | 已提交行不可改写(除整体擦除后重放) |
| **可见窗口** | 程序每帧重绘 | 完全可变 |
| **alternate screen** | 仅 modal / pager | 离场不留痕 |

transcript 留在 normal screen,换取原生滚动、原生选择复制、退出后内容保留、tmux detach 存活。

## 2. C/W/B 账本

来源:`oh-my-pi/docs/tui-core-renderer.md:41-58`。

设 composed frame 长度为 $L$,终端高度为 $h$。

| 量 | 含义 |
|---|---|
| **C** = `committedRows` | frame 行 `[0, C)` 已进入终端历史。普通 emitter 绝不重写。 |
| **W** = `windowTopRow` | 映射到 grid row 0 的 frame 行。可见窗口 = `[W, W+h)`。 |
| **B** = live-region boundary | 第一个仍可能变化的行,由组件上报。 |

普通(unpinned)帧的窗口与提交数学:

$$W = \max(C,\ L - h),\qquad \text{commit end} = \max(C,\ W)$$

**C 与 B 无序关系。** C 可以超过 B:B 之后仍可变的行离开窗口时,作为**冻结视觉快照**提交。只有 pinned live region 才把可变尾部留在 viewport 内。

对应代码 `oh-my-pi/packages/tui/src/tui.ts:3093`:

```ts
chunkTo = liveRegionPinned ? Math.min(windowTop, finalBoundary) : windowTop;
```

### 三区语义

$$\underbrace{[0,\ \min(C,B))}_{\text{exact · 参与审计}}\quad\underbrace{[B,\ C)}_{\text{frozen snapshot · audit-exempt}}\quad\underbrace{[W,\ W+h)}_{\text{可见窗口}}$$

- **exact 区**:组件声明为 FINAL 的行,以精确字节提交,每帧参与 committed-prefix 审计。审计发现被重排就 re-anchor,让后续行重新提交(旧副本留在历史里——duplication, never loss)。
- **frozen 区**:滚出窗口时仍是 live 的行,提交的是"滚出去那一刻屏幕上的样子"。故意排除在精确性声明之外,所以一个正在收缩的预览不会每帧触发 re-anchor。
- 边界回退时(markdown rewind、fence 出现),已验证的行降级为 frozen 快照,而不是去审计一段预期会变的内容。

### 接受的代价

- 已滚过窗口顶的块无法原地重排版。unpinned 可变行留下的是 stale 历史行。
- 组件树不上报 seam 时是 shell 语义:滚出去的即为最终。
- multiplexer 内 resize 后,pane 历史保持旧的换行宽度(与任何 shell 输出一致)。

## 3. 四条 emitter 路径

来源:`oh-my-pi/docs/tui-core-renderer.md:95-98`。

| emitter | 发出的字节 | 触发条件 |
|---|---|---|
| `full_paint` | home + committed chunk + 整窗;可选 ED3 | 首帧、session 替换、resize、`reset_display` |
| `update` scroll-append | 新底部行 + 变化行范围 | 有行滚出屏幕,滚出的恰好是 commit chunk |
| `update` in-window diff | **相对**光标移动 + 变化行重写 | 无滚动、无提交 |
| `update` seam rewrite | commit chunk + 整窗重写 | 提交/窗口重锚、隐藏间隙回填、mux resize |

**ED3 只在 `full_paint` 里出现,且仅当 `clear_scrollback == true`。**

普通 update 路径永不发 ED2/ED3 或绝对光标 home——多个终端家族看到这些字节会把已向上滚动的读者拽回底部。

### 固定高度活跃区走哪条

一个高度恒定的流式框(如 sliding tail window,顶部显示 `… (15 earlier lines)`)走 **in-window diff**:

$L$ 恒定 $\Rightarrow$ $W$ 恒定 $\Rightarrow$ commit end 恒定 $\Rightarrow$ 零字节进历史。

注意:$L$ 恒定省下的是 $O(\text{history})$,**不是** $O(\text{box})$。sliding tail 会让框内几乎每行都变,整个框仍要重写;逻辑上每帧仍完整重建整帧(靠 segment 账本和字符串缓存复用未变段)。ratatui 的 `Terminal::draw` 是同一个模型——`ratatui-core/src/terminal/render.rs:43-47` 明确要求 callback 必须完整渲染整帧,再只写 buffer diff。

### $L$ 恒定是刻意设计

`oh-my-pi/packages/coding-agent/src/tools/render-utils.ts:222-225`:

```ts
export function previewWindowRows(): number {
	const rows = process.stdout.rows || PREVIEW_WINDOW_FALLBACK_ROWS;
	return Math.max(PREVIEW_WINDOW_MIN_LINES, rows - PREVIEW_WINDOW_RESERVED_ROWS);
}
```

tail window 高度 = 终端行数 − chrome 保留,保证框塞得进可见窗口。超出就丢头保尾并显示隐藏计数(`capPreviewLines:245-248`)。

**违反的代价是真实事故**(`oh-my-pi/packages/coding-agent/CHANGELOG.md:2342`):框超出 viewport 时,可变尾部滚到 commit window 之上,按三区规则每帧提交一份新快照,往 scrollback 堆了几十条重复 banner。

修复有两条路:把高度钉进 viewport(oh-my-pi 对流式框的选择),或把该区域声明为 pinned live region(对固定高度仪表盘的选择)。

## 4. DECSTBM 历史注入

来源:`codex-rs/tui/src/insert_history.rs:194-246`。

```
┌─Screen───────────────────────┐
│┌╌Scroll region╌╌╌╌╌╌╌╌╌╌╌╌╌╌┐│   <- DECSTBM 限制在 viewport 之上
│┆                            ┆│
│█╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌┘│   <- 光标停在滚动区末行
│╭─Viewport───────────────────╮│   <- 活跃 UI，完全不受影响
│╰────────────────────────────╯│
└──────────────────────────────┘
```

```rust
queue!(writer, SetScrollRegion(1..area.top()))?;      // CSI 1;N r
queue!(writer, MoveTo(0, cursor_top))?;
for line in &wrapped {
    queue!(writer, Print("\r\n"))?;
    write_history_line(writer, line, wrap_width)?;
}
queue!(writer, ResetScrollRegion)?;                    // CSI r
queue!(writer, MoveTo(last_cursor_pos.x, last_cursor_pos.y))?;
```

**注意这里的两次绝对 `MoveTo` 是不变量 2 的豁免。** 普通增量帧禁止绝对定位(会拽动已向上滚动的读者),但历史注入有额外保护:此处是 DECSTBM 把影响锁在 viewport 之上。**豁免不等于无条件**——另一条注入路径 `ZellijRaw` 不用滚动区,靠一组不同的条件保证安全,见下节。

codex 刻意用 `MoveTo` 而非 `set_cursor_position`,注释在 `insert_history.rs:234-236`:

> NB: we are using MoveTo instead of set_cursor_position here to avoid messing with the terminal's last_known_cursor_position, which hopefully will still be accurate after we fetch/restore the cursor position. insert_history_lines should be cursor-position-neutral :)

即:绕过 `Terminal` 的光标跟踪,让存/恢复那一对成为唯一的权威。

viewport 尚未触底时,先用 `\x1bM`(RI, Reverse Index)在受限区内把 viewport 往下推腾出空间(`insert_history.rs:196-215`)。

### 硬约束:滚动区必须包含 row 0

`ratatui-core/src/backend.rs:340-343`:

> If the region includes row 0, then `line_count` rows are copied into the bottom of the scrollback buffer.

codex 的 `SetScrollRegion(1..area.top())` 是 1-based,含 row 0。若滚动区不含 row 0,滚出的行**直接丢弃**,不进 scrollback。

`backend.rs:382-383` 承认这个行为是 "this how terminals seem to implement things",非标准强制——所以真机烟测不可省。

### 另一条路径:`ZellijRaw`(不用滚动区)

Zellij 不把 soft-wrap 续行约束在滚动区内(`insert_history.rs:51-53`),所以当 batch 用 terminal-wrap 策略时,codex 走一条完全不同的注入路径(`:164-193`):

```rust
// 先清 viewport，防止终端滚动把 composer 内容推进 scrollback
terminal.clear_after_position(area.as_position())?;          // :167
let writer = terminal.backend_mut();
queue!(writer, MoveTo(0, area.top()))?;                      // :169 绝对定位，无滚动区
for (index, line) in wrapped.iter().enumerate() {
    if index > 0 { queue!(writer, Print("\r\n"))?; }
    write_history_line(writer, line, wrap_width)?;
}
// 预留 area.height 个空行，让历史紧贴 composer 上方
for _ in 0..area.height {
    queue!(writer, Print("\r\n"), Clear(ClearType::UntilNewLine))?;   // :180-182
}
queue!(writer, MoveTo(last_cursor_pos.x, last_cursor_pos.y))?;        // :183 恢复光标
// 重算 viewport 顶部
```

**这条路径同样绝对定位,但没有 DECSTBM 保护。** 安全性来自 `:165-166` 的注释所述的另一组条件:

> The existing viewport is immediately replaced in the same draw pass. Clear it before terminal scrolling can move composer contents into scrollback.

即:(a) 先 `clear_after_position` 清掉 viewport,(b) 在**同一 draw pass** 内完整替换它(含预留空行)。少了任一条,终端滚动会把 composer 内容推进 scrollback。

两条路径的对比——这是不变量 3 分层的原因:

| | `Standard` | `ZellijRaw` |
|---|---|---|
| 滚动区 | `SetScrollRegion` / `ResetScrollRegion` 成对 | 不用 |
| viewport | 不动一个字节 | 先清空,同帧完整重画 |
| 绝对 `MoveTo` | `:203` / `:237` / `:245` | `:169` / `:183` |
| cursor-position-neutral | 是 | 是 |
| 换行元数据 | 预先 wrap(`PreWrap`) | 交给终端(`Terminal`),保留 soft-wrap 元数据以便选择复制 |

**共同点只有 cursor-position-neutral。** 保护机制不同,不可混用:在 `ZellijRaw` 里加滚动区,或在 `Standard` 里省掉清 viewport,都会坏。

### 自定义 Command

crossterm 不内置 DECSTBM,需要自己实现(`insert_history.rs:333-369`)。Windows 分支必须注意:见 `platform.md`。

## 5. ctrl+o:改写已提交内容

DECSTBM 无法更新已提交行。要让折叠/展开作用于已滚出屏幕的内容,唯一途径是**擦掉整个 scrollback 并重放**。

oh-my-pi 的链路:

| 跳 | 位置 |
|---|---|
| `app.tools.expand` -> `ctrl+o` | `packages/coding-agent/src/config/keybindings.ts:124-126` |
| 编辑器截获 -> `onExpandTools()` | `modes/components/custom-editor.ts:911-913` |
| `toggleToolOutputExpansion()` -> `setToolsExpanded()` | `modes/controllers/input-controller.ts:459-460` |
| 给每个 chat child 派发 `setExpanded(flag)` | 同上 `:1906-1921` |
| **`ui.resetDisplay()`** | 同上 `:1921` |
| invalidate 全部组件 + 置 `clearScrollbackOnNextRender` | `packages/tui/src/tui.ts:1899` |
| `full_paint` 发 ED3,从 home 重放整个 transcript,不截断 | `tui.ts:3599-3603, 3646-3650` |

`input-controller.ts:1916-1921` 的注释解释了为什么必须走这条重路径:普通重绘会把已冻结的离屏快照原样重放,所以 toggle 在视口上方看起来毫无反应。

### 触发条件必须窄

`oh-my-pi/docs/tui-core-renderer.md:100-117` 列出两类 ED3 调用者:

| 调用者 | 默认 |
|---|---|
| 显式用户手势(session 替换、resize、`reset_display`) | 启用 |
| divergence rebuild(committed prefix 结构性 resync,或帧塌缩进已提交行) | **关闭** |

即:流式渲染中的内容漂移**不修历史**,宁可留 stale row。照抄这个默认。

### multiplexer 降级

`oh-my-pi/packages/tui/src/tui.ts:12-13`:

> Multiplexer panes, where ED3 is unsafe, instead re-anchor and recommit below the stale fragment — **duplication, never loss**.

tmux 下 `ctrl+o` 会让历史出现重复段。这比"toggle 无反应"好,也比"擦错东西"安全。

**注意:mux 只禁 ED3,DECSTBM 照用。** 两者不能绑在同一个开关上。

## 6. 不变量(与 README 一致,此处附证据)

1. **ED3 单一 callsite** — `docs/tui-core-renderer.md:157-159` 的第一条禁令。
2. **普通增量帧零绝对定位** — `docs/tui-core-renderer.md:119-120`。适用于 in-window diff 与 scroll-append 的窗口重绘。
3. **历史注入是唯一允许绝对定位的增量路径,但豁免有条件且按模式分层。** 两条路径都用绝对 `MoveTo`(`Standard`:`:203` RI 分支 / `:237` 移到滚动区末行 / `:245` 恢复;`ZellijRaw`:`:169` / `:183`),这是刻意的——`:234-236` 的注释说明选 `MoveTo` 而非 `set_cursor_position` 是为了不动到 Terminal 的 `last_known_cursor_position` 跟踪,让存/恢复那一对成为唯一权威。
   - **共同要求**:cursor-position-neutral(`:236` 的函数契约)。
   - **`Standard` 附加**:`SetScrollRegion` / `ResetScrollRegion` 成对,滚动区把影响锁在 viewport 之上,viewport 零重绘。
   - **`ZellijRaw` 附加**:先 `clear_after_position` 清 viewport(`:167`),并在同一 draw pass 内完整替换它含预留空行(`:180-182`)。理由见 `:165-166`。
   - 保护机制不可混用。详见第 4 节的对比表。
4. **Windows VT 启用独立于 crossterm** — 见 `platform.md`。
5. **能力判定与 mode 施加分离** — `docs/tui-core-renderer.md:205-208`:行为不再依赖哪个终端在渲染,所以没有风险类需要检测。
