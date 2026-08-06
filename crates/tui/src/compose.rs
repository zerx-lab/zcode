//! segment 账本：把一组 [`Component`] 拼成一帧 transcript，未变的组件直接复用上一帧
//! 渲出的行，只重新 ingest stable prefix 之后的部分。
//!
//! 抄源 `oh-my-pi/packages/tui/src/tui.ts:1195-1309`（`compose()` 主循环）。上游那份
//! 实现还带一层"partial compose root"：只有被显式请求重渲染的子树才调用 `render()`，
//! 未被请求的兄弟即使 revision 变了也直接复用旧行（`tui.ts:1205-1206`）。这依赖一份
//! "本帧请求了谁"的跨帧状态，本实现按 `plans/tui/modules.md:31` 简化成更纯的
//! `(id, revision, width)` 三元组比较：三者与缓存里**同下标**的段完全一致才复用，
//! 否则整段重渲染。粒度更粗，但不需要额外的请求集合，逻辑更容易审计。

use ratatui::text::{Line, Span};
use zcode_text::width::{expand_tabs, sanitize_text, strip_ansi};

/// 组件渲染出的行进入帧之前的**唯一**清洗点。
///
/// 组件的行文本随时可能来自模型输出、工具 stdout、文件内容——里面混进
/// `ESC[2J`、OSC 序列或裸 C0 都是常态。这些字节最终会经 `Print` 原样交给终端：
/// 在 viewport 里它们污染 ratatui 的 cell 网格，在历史注入与 full paint 重放里
/// 它们直接被终端当成真命令执行（清屏、改标题、设滚动区）。
///
/// 收敛在 ingest 而不是各渲染路径各清一遍，是 `rule://zcode-architecture`
/// 「TUI 输出清理」的要求：清洗能力的唯一实现落点是 `zcode-text`，渲染点一律调它，
/// 且**每一条**渲染路径都要过同一套清理。放在这里，viewport diff、历史注入、
/// full paint 重放、纯文本输出四条路径共用同一份已清洗的行，不可能漏掉某一条。
///
/// # 三步，全部转调 [`zcode_text::width`]
///
/// 1. [`strip_ansi`]：剥完整的 ANSI/OSC/DCS 序列。**必须先做这一步**——OSC 在实践中
///    多用 `BEL`（`\x07`）收尾，而 `BEL` 本身是 C0；先清 C0 会把终止符删掉，留下的
///    `ESC ]0;title` 成为永不结束的序列，状态机只能一路吞到行尾，把后面的正文一起
///    吃掉（`"\x1b]0;title\x07plain"` 会退化成 `""` 而不是 `"plain"`）。
/// 2. [`sanitize_text`]：清剩下的裸 C0（保留 `\t`）、DEL、C1，并折叠上游解码噪声
///    产生的连续 `U+FFFD`。此时文本里已无完整序列，它内部的顺序不再影响结果。
/// 3. [`expand_tabs`]：把制表符按列展开成空格。等宽网格里原样保留的 tab 会造成视觉
///    空洞，也让显示宽度与实际占位对不上。
///
/// 样式**不受影响**：颜色和修饰走 ratatui 的 `Style`，不靠内嵌 escape。
/// 这也意味着组件不能靠往文本里塞 OSC8 来做超链接——真需要时应该加结构化字段，
/// 而不是把可执行字节混进正文。
fn normalize_line(line: &Line<'static>) -> Line<'static> {
    let cleaned: Vec<String> = line
        .spans
        .iter()
        .map(|span| {
            let stripped = strip_ansi(&span.content);
            sanitize_text(&stripped).into_owned()
        })
        .collect();

    let texts = if cleaned.iter().any(|text| text.contains('\t')) {
        expand_tabs_across_spans(&cleaned)
    } else {
        cleaned
    };

    Line {
        style: line.style,
        alignment: line.alignment,
        spans: line
            .spans
            .iter()
            .zip(texts)
            .map(|(span, content)| Span {
                style: span.style,
                content: content.into(),
            })
            .collect(),
    }
}

/// 把一整行（跨 span）的制表符按**当前列**展开。
///
/// 不能逐 span 各调一次 [`expand_tabs`]：它每次都从第 0 列起算
/// （`crates/text/src/width.rs` 的 `expand_tabs_with_tab`），于是
/// `["ab", "\tX"]` 会把 tab 当成行首的 tab 展成 4 个空格，得到 `"ab    X"`，
/// 而正确结果是补到下一个制表位、即 2 个空格的 `"ab  X"`。列位一错，
/// 后续 span 的列号与账本记的显示宽度全跟着错。
///
/// 做法是**增量喂给同一个中央实现**，而不是在这里重写一遍制表位算法：
/// 已展开的前缀里不含 tab，所以 `expand_tabs(前缀 + 本段)` 必然以该前缀原样开头，
/// 差集就是"本段在该列位上的展开结果"。这样制表位规则仍然只有 `zcode-text` 一份。
///
/// 只在整行确实含 tab 时才走这条 O(span 数 × 行长) 的路径；不含 tab 是常态，
/// 调用方直接跳过。
fn expand_tabs_across_spans(cleaned: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(cleaned.len());
    let mut expanded_prefix = String::new();
    for text in cleaned {
        let mut probe = expanded_prefix.clone();
        probe.push_str(text);
        let full = expand_tabs(&probe).into_owned();
        // 按 `expand_tabs` 的契约这里必然命中：它只把 `\t` 换成空格，而前缀里已经没有
        // `\t`。没命中就退回"本段独立展开"（列位从 0 起算）——宁可这一段缩进偏了，
        // 也不要丢内容或把前缀重复一遍。
        let Some(piece) = full.strip_prefix(expanded_prefix.as_str()) else {
            tracing::warn!("expand_tabs 未保留已展开前缀，退回按段展开");
            let piece = expand_tabs(text).into_owned();
            expanded_prefix.push_str(&piece);
            out.push(piece);
            continue;
        };
        let piece = piece.to_owned();
        expanded_prefix = full;
        out.push(piece);
    }
    out
}

/// 组件身份。同一个 id 在相邻两帧之间代表同一个组件；[`Composer`] 靠它（连同
/// [`Component::revision`]）判断某个位置上的段是否可以直接复用。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ComponentId(pub u64);

/// 可组合进 transcript 的一段内容（一条消息、一个工具调用块……）。
///
/// 以 `&dyn Component` 传入 [`Composer::compose`]，因此这个 trait 必须是
/// object-safe 的：不带泛型方法，也不返回 `Self`。
pub trait Component {
    /// 组件身份，跨帧稳定。
    fn id(&self) -> ComponentId;

    /// 单调递增的版本号。与上一帧同一下标的段相比，`(id, revision, width)`
    /// 三者都相同才会被 [`Composer::compose`] 判定为可复用，从而跳过
    /// [`render`](Self::render)。
    fn revision(&self) -> u64;

    /// 把组件渲染成若干行。只在缓存未命中（不可复用）时才会被调用。
    fn render(&self, width: u16) -> Vec<Line<'static>>;

    /// 组件内第一条仍可能变化的行，下标相对本段起点（即 [`render`](Self::render)
    /// 返回的那个 `Vec` 里的下标）。`None` 表示本次渲出的内容已经全部定稿。
    ///
    /// 默认返回 `None`：多数组件（已发送的用户消息、已完成的工具调用……）渲出即定
    /// 稿，只有流式输出、进度条这类组件需要覆盖它。
    fn live_boundary(&self) -> Option<usize> {
        None
    }
}

/// 一次 [`Composer::compose`] 调用的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ComposeOutcome {
    /// 本帧总行数。
    pub total_rows: usize,
    /// 与上一帧逐行相同的前缀长度。
    pub stable_prefix_rows: usize,
    /// 全帧的 live-region boundary `B`：第一条仍可能变化的行。`B` 之后的所有行一律
    /// 视作可变——即便某一行所属的组件自己没有上报 seam，见
    /// `plans/tui/architecture.md:23`。
    pub boundary: usize,
}

/// 上一帧里某个组件贡献的行区间，连同判断"是否可复用"所需的身份三元组。
///
/// `live_boundary` 随 `rows` 一起缓存：复用整段时一并复用它，不重新调用
/// [`Component::live_boundary`]——见 [`Composer::compose`] 复用分支的注释。
#[derive(Debug, Clone)]
struct Segment {
    id: ComponentId,
    revision: u64,
    width: u16,
    /// 本段第一行在（本帧）frame 里的下标。
    start: usize,
    rows: Vec<Line<'static>>,
    live_boundary: Option<usize>,
}

/// transcript 的 segment 账本：缓存上一帧每个组件渲出的行，逐帧只重算变化的部分。
///
/// 抄源 `oh-my-pi/packages/tui/src/tui.ts:1195-1309`。
#[derive(Debug, Default)]
pub struct Composer {
    /// 上一次 `compose` 留下的段缓存，下标即组件在组件列表里的位置。
    segments: Vec<Segment>,
    /// 扁平化后的整帧行；`frame()` 直接借出这个缓冲区，逐帧原地增量更新。
    frame: Vec<Line<'static>>,
}

impl Composer {
    /// 新建一个空 composer：第一次 `compose` 没有缓存可比对，会全量渲染。
    #[must_use]
    pub fn new() -> Self {
        Self {
            segments: Vec::new(),
            frame: Vec::new(),
        }
    }

    /// 把 `components` 拼成一帧。
    ///
    /// # 复用与位置对齐
    ///
    /// 组件顺序就是 transcript 顺序，所以比对**按下标对齐**：下标 `i` 的组件只跟
    /// 上一帧缓存里下标 `i` 的段比较，不做按 id 的乱序匹配。原因是组件列表中间
    /// 插入或删除一个组件时，它之后每个组件的下标都会整体平移；如果改用"在缓存里
    /// 按 id 搜同一个组件"来判定复用，会把它们的行安在与 transcript 实际顺序脱节
    /// 的相对位置上。下标对齐带来的代价只是"插入点之后的组件本帧多渲染一次"，比
    /// 行序错乱更可接受。
    ///
    /// 缓存段数多于本次组件数时，多出的尾部段随着整体替换 `self.segments` 被丢弃。
    ///
    /// # `stable_prefix_rows` 的 O(段数) 论证
    ///
    /// 定义是"新旧两帧逐行相同的前缀长度"，但不需要逐行比较：从第 0 段开始按序
    /// 累计各段行数，只要某段同时满足 (a) 被判定复用、(b) 它在本帧的起始行号与它
    /// 在上一帧的起始行号相同，就把它的行数计入前缀、继续看下一段；一旦某段不满
    /// 足，立刻停止累计，即便它后面还有别的段单独看也满足 (a)(b)。
    ///
    /// 停止之后不会再有相同的行：段不满足 (a) 意味着它自己的第一行内容就已经跟
    /// 上一帧不同,前缀到此为止;不满足 (b) 意味着它前面那些段的本帧总行数和上一帧
    /// 总行数不相等（有段增删了行），这段本身的起始位置已经错位，它的行即便字节
    /// 相同也对不上"新旧同一行号"这件事。而且一旦某段触发停止,更靠后的段的起始
    /// 行号必然继承同一个偏移量,不可能再次对齐——所以"从头扫到第一个不满足处"
    /// 就是整个前缀,不用管后面。
    pub fn compose(&mut self, components: &[&dyn Component], width: u16) -> ComposeOutcome {
        let old_segments = std::mem::take(&mut self.segments);
        let mut old_iter = old_segments.into_iter();
        let mut new_segments = Vec::with_capacity(components.len());

        let mut offset = 0usize;
        let mut stable_prefix_rows = 0usize;
        let mut chain_stable = true;
        let mut boundary: Option<usize> = None;

        for component in components {
            let id = component.id();
            let revision = component.revision();
            let previous = old_iter.next();
            let prev_start = previous.as_ref().map(|seg| seg.start);
            let reused = previous
                .as_ref()
                .is_some_and(|seg| seg.id == id && seg.revision == revision && seg.width == width);

            // 复用：直接搬走缓存段的行与 live_boundary，不调用 render()/live_boundary()。
            // 不复用（内容变了或组件被替换）：重新渲染，并现查一次 live_boundary，
            // clamp 到本次渲出的行数——组件上报的 k 允许超界，这里兜底。
            let (rows, live_boundary) = if let Some(seg) = previous.filter(|_| reused) {
                (seg.rows, seg.live_boundary)
            } else {
                let rows = component
                    .render(width)
                    .iter()
                    .map(normalize_line)
                    .collect::<Vec<_>>();
                let live_boundary = component.live_boundary().map(|k| k.min(rows.len()));
                (rows, live_boundary)
            };

            let row_count = rows.len();
            let start = offset;

            if chain_stable {
                if reused && prev_start == Some(start) {
                    stable_prefix_rows += row_count;
                } else {
                    chain_stable = false;
                }
            }

            // Topmost seam wins：第一个上报的组件已经界定了它之后一切皆可变，
            // 更靠后的组件即便也上报，也不能把边界往后推——那会把前一个组件仍在
            // 变化的行错误地划进"已定稿"区间。
            if boundary.is_none()
                && let Some(k) = live_boundary
            {
                boundary = Some(start + k);
            }

            new_segments.push(Segment {
                id,
                revision,
                width,
                start,
                rows,
                live_boundary,
            });
            offset += row_count;
        }

        let total_rows = offset;
        // 没有任何组件上报 live boundary：shell 语义，全部视为已定稿。
        let boundary = boundary.unwrap_or(total_rows);

        // frame 重建：stable prefix 之前的行本来就已经在 `self.frame` 里（上一帧写
        // 进去的），原样保留、不比较也不拷贝。只在 `stable_prefix_rows` 之后
        // truncate + extend——这正是"O(变化部分)而非 O(全帧)"的来源：那些完全落在
        // stable prefix 内的段，下面的 `from` 会算出 `>= rows.len()`，
        // `rows.get(from..)` 拿到空切片，extend 零个元素，不产生任何 clone；只有
        // 真正变化、或者因为前面的段增删行而被"推移"到 stable prefix 之后的段，
        // 才会被逐行 clone 进 frame。
        if stable_prefix_rows != total_rows || self.frame.len() != total_rows {
            self.frame.truncate(stable_prefix_rows);
            for segment in &new_segments {
                let from = stable_prefix_rows.saturating_sub(segment.start);
                if let Some(remaining) = segment.rows.get(from..) {
                    self.frame.extend(remaining.iter().cloned());
                }
            }
        }

        self.segments = new_segments;

        ComposeOutcome {
            total_rows,
            stable_prefix_rows,
            boundary,
        }
    }

    /// 返回当前帧扁平化后的行切片。
    #[must_use]
    pub fn frame(&self) -> &[Line<'static>] {
        &self.frame
    }

    /// 清空缓存与 frame。下一次 `compose` 不会复用任何段，全量重建。
    pub fn reset(&mut self) {
        self.segments.clear();
        self.frame.clear();
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::{Component, ComponentId, Composer};
    use ratatui::text::{Line, Span};

    /// 测试用假组件：`renders` 记录 `render()` 被调用的次数，用来验证复用是否真的
    /// 跳过了重渲染。
    struct Fake {
        id: u64,
        rev: u64,
        lines: Vec<String>,
        live: Option<usize>,
        renders: Cell<u32>,
    }

    impl Fake {
        fn new(id: u64, rev: u64, lines: &[&str]) -> Self {
            Self {
                id,
                rev,
                lines: lines.iter().map(|line| (*line).to_string()).collect(),
                live: None,
                renders: Cell::new(0),
            }
        }

        fn with_live(mut self, boundary: usize) -> Self {
            self.live = Some(boundary);
            self
        }
    }

    impl Component for Fake {
        fn id(&self) -> ComponentId {
            ComponentId(self.id)
        }

        fn revision(&self) -> u64 {
            self.rev
        }

        fn render(&self, _width: u16) -> Vec<Line<'static>> {
            self.renders.set(self.renders.get() + 1);
            self.lines
                .iter()
                .map(|line| Line::from(line.clone()))
                .collect()
        }

        fn live_boundary(&self) -> Option<usize> {
            self.live
        }
    }

    /// 逐 span 拼出一行的纯文本，只在测试里用来断言 frame 内容。
    fn line_text(line: &Line<'static>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn unchanged_components_are_not_rerendered() {
        let a = Fake::new(1, 1, &["a1", "a2"]);
        let b = Fake::new(2, 1, &["b1"]);
        let components: Vec<&dyn Component> = vec![&a, &b];
        let mut composer = Composer::new();

        composer.compose(&components, 80);
        assert_eq!(a.renders.get(), 1);
        assert_eq!(b.renders.get(), 1);

        let second = composer.compose(&components, 80);
        assert_eq!(a.renders.get(), 1, "第二帧不应重新渲染 a");
        assert_eq!(b.renders.get(), 1, "第二帧不应重新渲染 b");
        assert_eq!(second.total_rows, 3);
        assert_eq!(second.stable_prefix_rows, second.total_rows);
    }

    #[test]
    fn only_changed_trailing_component_rerenders() {
        let a = Fake::new(1, 1, &["a1", "a2"]);
        let b1 = Fake::new(2, 1, &["b1"]);
        let first: Vec<&dyn Component> = vec![&a, &b1];
        let mut composer = Composer::new();
        composer.compose(&first, 80);

        let b2 = Fake::new(2, 2, &["b1", "b2"]);
        let second: Vec<&dyn Component> = vec![&a, &b2];
        let outcome = composer.compose(&second, 80);

        assert_eq!(a.renders.get(), 1, "a 的三元组没变，不应重新渲染");
        assert_eq!(b2.renders.get(), 1, "b 的 revision 变了，必须重新渲染");
        assert_eq!(outcome.stable_prefix_rows, 2, "前缀只到 a 的 2 行为止");
        assert_eq!(outcome.total_rows, 4);
    }

    #[test]
    fn leading_row_count_change_breaks_prefix_without_forcing_rerender() {
        let a1 = Fake::new(1, 1, &["a1", "a2"]);
        let b = Fake::new(2, 1, &["b1"]);
        let first: Vec<&dyn Component> = vec![&a1, &b];
        let mut composer = Composer::new();
        composer.compose(&first, 80);

        let a2 = Fake::new(1, 2, &["a1", "a2", "a3"]);
        let second: Vec<&dyn Component> = vec![&a2, &b];
        let outcome = composer.compose(&second, 80);

        assert_eq!(
            outcome.stable_prefix_rows, 0,
            "第一段行数变了，前缀直接归零"
        );
        assert_eq!(a2.renders.get(), 1);
        assert_eq!(
            b.renders.get(),
            1,
            "b 本身没变，即便前缀断了也不该被重新渲染"
        );
    }

    #[test]
    fn width_change_forces_full_rerender() {
        let a = Fake::new(1, 1, &["a1"]);
        let b = Fake::new(2, 1, &["b1"]);
        let components: Vec<&dyn Component> = vec![&a, &b];
        let mut composer = Composer::new();

        composer.compose(&components, 80);
        composer.compose(&components, 40);

        assert_eq!(a.renders.get(), 2, "宽度变化必须让 a 重新渲染");
        assert_eq!(b.renders.get(), 2, "宽度变化必须让 b 重新渲染");
    }

    #[test]
    fn boundary_uses_first_reporting_component() {
        let a = Fake::new(1, 1, &["a1", "a2"]);
        let b = Fake::new(2, 1, &["b1", "b2", "b3"]).with_live(2);
        let c = Fake::new(3, 1, &["c1"]).with_live(0);
        let components: Vec<&dyn Component> = vec![&a, &b, &c];
        let mut composer = Composer::new();

        let outcome = composer.compose(&components, 80);
        // b 段起始行 = a 的行数 = 2，live_boundary = 2 → B = 2 + 2 = 4。
        // c 也上报了，但 b 是第一个上报的，c 的报告必须被忽略。
        assert_eq!(outcome.boundary, 4);
    }

    #[test]
    fn boundary_defaults_to_total_rows_when_nobody_reports() {
        let a = Fake::new(1, 1, &["a1", "a2"]);
        let b = Fake::new(2, 1, &["b1"]);
        let components: Vec<&dyn Component> = vec![&a, &b];
        let mut composer = Composer::new();

        let outcome = composer.compose(&components, 80);
        assert_eq!(outcome.boundary, outcome.total_rows);
        assert_eq!(outcome.boundary, 3);
    }

    #[test]
    fn boundary_clamps_to_segment_end() {
        let a = Fake::new(1, 1, &["a1", "a2"]).with_live(999);
        let components: Vec<&dyn Component> = vec![&a];
        let mut composer = Composer::new();

        let outcome = composer.compose(&components, 80);
        assert_eq!(
            outcome.boundary, 2,
            "超界的 live_boundary 必须 clamp 到段末"
        );
    }

    #[test]
    fn shrinking_component_list_drops_trailing_segments() {
        let a = Fake::new(1, 1, &["a1"]);
        let b = Fake::new(2, 1, &["b1", "b2"]);
        let first: Vec<&dyn Component> = vec![&a, &b];
        let mut composer = Composer::new();
        composer.compose(&first, 80);

        let second: Vec<&dyn Component> = vec![&a];
        let outcome = composer.compose(&second, 80);

        assert_eq!(outcome.total_rows, 1);
        assert_eq!(composer.frame().len(), 1);
    }

    /// 组件文本里的 escape / 控制字符必须在 ingest 时就被剥掉。
    ///
    /// 漏了它，这些字节会经 `Print` 原样交给终端：history 注入与 full paint 重放
    /// 会把 `ESC[2J` 当成真的清屏命令执行。
    #[test]
    fn ingest_strips_escapes_and_expands_tabs() {
        let dirty = Fake::new(
            0,
            0,
            &[
                "before\x1b[2Jafter",
                "\x1b]0;title\x07plain",
                "\x1b]8;;https://example.com\x1b\\st-term",
                "bell\x07and\x08backspace",
                "a\tb",
            ],
        );
        let mut composer = Composer::new();
        let components: Vec<&dyn Component> = vec![&dirty];
        composer.compose(&components, 40);

        let text: Vec<String> = composer
            .frame()
            .iter()
            .map(|line| line.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        assert_eq!(text[0], "beforeafter", "CSI 序列必须被剥掉");
        // 先 strip_ansi 再 sanitize_text，所以 BEL 终止符还在、OSC 能被正确定界，
        // 其后的正文必须保留。顺序颠倒会让整行退化成 ""。
        assert_eq!(text[1], "plain", "BEL 终止的 OSC 之后的正文必须保留");
        assert_eq!(text[2], "st-term", "ST 终止的 OSC 之后的正文必须保留");
        assert_eq!(text[3], "bellandbackspace", "裸 C0 必须被剥掉");
        assert_eq!(
            text[4], "a   b",
            "制表符按列展开：col 1 → 下一个制表位 col 4"
        );
        for line in composer.frame() {
            for span in &line.spans {
                assert!(
                    !span.content.contains('\x1b'),
                    "帧里不许残留 ESC: {:?}",
                    span.content
                );
            }
        }
    }

    /// 制表符必须按**整行的当前列**展开，不是每个 span 各自从第 0 列起算。
    ///
    /// 逐 span 调 `expand_tabs` 会把 `["ab", "\tX"]` 的 tab 当成行首 tab 展成 4 个空格
    /// （`"ab    X"`），正确结果是补到下一个制表位、即 2 个空格（`"ab  X"`）。
    /// 列位一错，后续 span 的列号与账本记的显示宽度全跟着错。单 span 的
    /// `"a\tb"` 用例抓不到这个 bug。
    #[test]
    fn tabs_expand_by_column_across_span_boundaries() {
        struct MultiSpan;
        impl Component for MultiSpan {
            fn id(&self) -> ComponentId {
                ComponentId(7)
            }

            fn revision(&self) -> u64 {
                0
            }

            fn render(&self, _width: u16) -> Vec<Line<'static>> {
                vec![
                    // tab 落在第 3 列（"ab" 占 0..2）→ 补到制表位 4 → 2 个空格。
                    Line::from(vec![Span::raw("ab"), Span::raw("\tX")]),
                    // 连续两个跨 span 的 tab：col 1 → 4（3 空格），col 5 → 8（3 空格）。
                    Line::from(vec![Span::raw("a"), Span::raw("\tb"), Span::raw("\tc")]),
                    // tab 在行首：整 4 个空格。
                    Line::from(vec![Span::raw("\t"), Span::raw("z")]),
                ]
            }
        }

        let mut composer = Composer::new();
        let component = MultiSpan;
        let components: Vec<&dyn Component> = vec![&component];
        composer.compose(&components, 40);

        let text: Vec<String> = composer
            .frame()
            .iter()
            .map(|line| line.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        assert_eq!(text[0], "ab  X", "tab 应补到下一个制表位而不是整 4 格");
        assert_eq!(text[1], "a   b   c", "连续跨 span 的 tab 各自按当前列补齐");
        assert_eq!(text[2], "    z", "行首 tab 补满一个制表位");

        // 展开后的行宽必须与逐 span 求和一致——账本就是按这个宽度推进的。
        for (line, joined) in composer.frame().iter().zip(&text) {
            let per_span: usize = line
                .spans
                .iter()
                .map(|s| zcode_text::width::visible_width(&s.content))
                .sum();
            assert_eq!(
                per_span,
                zcode_text::width::visible_width(joined),
                "整行宽度与逐 span 求和必须一致: {joined:?}"
            );
            assert!(!joined.contains('\t'), "展开后不许残留制表符: {joined:?}");
        }
    }

    #[test]
    fn frame_equals_concatenated_segment_lines() {
        let a = Fake::new(1, 1, &["a1", "a2"]);
        let b = Fake::new(2, 1, &["b1"]);
        let components: Vec<&dyn Component> = vec![&a, &b];
        let mut composer = Composer::new();
        composer.compose(&components, 80);

        let rendered: Vec<String> = composer.frame().iter().map(line_text).collect();
        assert_eq!(
            rendered,
            vec!["a1".to_string(), "a2".to_string(), "b1".to_string()]
        );
    }
}
