//! 工具输出的统一截断：行 / 字节 / 列三个维度，外加一个跨 chunk 保持状态的流式 sink。
//!
//! 语义移植自 oh-my-pi `packages/coding-agent/src/session/streaming-output.ts`，但不移植它
//! 为了避免整串 `Buffer` 分配而做的 code-unit 快速拒绝技巧（`:328-347`）——`&str` 本身就是
//! UTF-8，`s.len()` 即字节数，行迭代用 `memchr`，直白实现即可。

use std::collections::VecDeque;
use std::fmt::Write as _;

use crate::width::{truncate_to_width, visible_width, visible_width_up_to};

/// 工具输出的统一截断上限：行数、字节数、单行显示列数各自独立生效。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TruncateLimits {
    /// 最多保留的行数。
    pub max_lines: usize,
    /// 最多保留的字节数。
    pub max_bytes: usize,
    /// 单行最多保留的显示列数（经 `unicode-width` 计算，不是字节数或字符数）。
    pub max_columns: usize,
}

impl Default for TruncateLimits {
    /// 3000 行 / 50 000 字节 / 512 列——移植自 oh-my-pi `streaming-output.ts:10-12` 的
    /// 上游实测取值；其中 512 列这个数字上游本身也没有出处（已核实：既无 benchmark 也无
    /// 关联 issue 说明依据），本仓沿用同一个数值，但必须如实标注"前提未知"，不能假装它
    /// 有实验支撑。
    fn default() -> Self {
        Self {
            max_lines: 3000,
            max_bytes: 50_000,
            max_columns: 512,
        }
    }
}

/// 截断操作的结果：保留下来的文本，以及描述丢弃了多少内容的元数据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Truncated {
    /// 截断后保留的文本。
    pub text: String,
    /// 被丢弃的完整行数（不含只是被列宽裁短、但仍然可见的行）。
    pub dropped_lines: usize,
    /// 被丢弃的字节数。
    pub dropped_bytes: usize,
    /// 仅 [`truncate_head`] 会置位：第一行本身就超出预算，因此 `text` 为空
    /// （而不是该行的部分内容——头部截断绝不返回半行）。
    pub first_line_exceeds_limit: bool,
}

/// 按行切分，保留每行末尾的 `\n`（若存在）；最后一行若没有换行符则不补。
fn lines_with_terminator(input: &str) -> impl Iterator<Item = &str> {
    let mut rest = input;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        if let Some(i) = memchr::memchr(b'\n', rest.as_bytes()) {
            // `i` 是 `rest` 内 '\n' 的字节位置；'\n' 是单字节字符，i + 1 必然落在合法的
            // UTF-8 字符边界上，`split_at` 不会 panic。
            let (line, tail) = rest.split_at(i + 1);
            rest = tail;
            Some(line)
        } else {
            let line = rest;
            rest = "";
            Some(line)
        }
    })
}

/// 统计 `input` 的"行数"：以 `\n` 分隔，末尾若有换行符不额外计一个空行。
fn count_lines(input: &str) -> usize {
    if input.is_empty() {
        return 0;
    }
    let newline_count = memchr::memchr_iter(b'\n', input.as_bytes()).count();
    if input.as_bytes().last() == Some(&b'\n') {
        newline_count
    } else {
        newline_count + 1
    }
}

/// 取 `s` 的最长前缀，字节数不超过 `budget`，并对齐到合法的字符边界。
fn take_prefix_within(s: &str, budget: usize) -> &str {
    if s.len() <= budget {
        return s;
    }
    let mut end = budget;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s.get(..end).unwrap_or("")
}

/// 取 `s` 的最长后缀，字节数不超过 `budget`，并对齐到合法的字符边界。
fn take_suffix_within(s: &str, budget: usize) -> &str {
    if s.len() <= budget {
        return s;
    }
    let mut start = s.len() - budget;
    while start < s.len() && !s.is_char_boundary(start) {
        start += 1;
    }
    s.get(start..).unwrap_or("")
}

/// 把一行"带终止符"的切片拆成 `(内容, 终止符)`；终止符是 `"\n"` 或 `""`。
fn split_line_terminator(line: &str) -> (&str, &str) {
    match line.strip_suffix('\n') {
        Some(content) => (content, "\n"),
        None => (line, ""),
    }
}

/// 保留头部。首行本身就超预算时 `text` 为空、`first_line_exceeds_limit = true`
/// （**不是**该行的部分内容）；否则只保留完整的行，绝不在行中间切断。
#[must_use]
pub fn truncate_head(input: &str, limits: &TruncateLimits) -> Truncated {
    if input.is_empty() {
        return Truncated {
            text: String::new(),
            dropped_lines: 0,
            dropped_bytes: 0,
            first_line_exceeds_limit: false,
        };
    }
    let total_bytes = input.len();
    let mut kept: Vec<&str> = Vec::new();
    let mut kept_bytes = 0usize;

    for line in lines_with_terminator(input) {
        if kept.len() >= limits.max_lines || kept_bytes + line.len() > limits.max_bytes {
            if kept.is_empty() {
                return Truncated {
                    text: String::new(),
                    dropped_lines: count_lines(input),
                    dropped_bytes: total_bytes,
                    first_line_exceeds_limit: true,
                };
            }
            let total_lines = count_lines(input);
            return Truncated {
                text: kept.concat(),
                dropped_lines: total_lines - kept.len(),
                dropped_bytes: total_bytes - kept_bytes,
                first_line_exceeds_limit: false,
            };
        }
        kept_bytes += line.len();
        kept.push(line);
    }

    Truncated {
        text: input.to_owned(),
        dropped_lines: 0,
        dropped_bytes: 0,
        first_line_exceeds_limit: false,
    }
}

/// 保留尾部。与 [`truncate_head`] **不对称**：当字节预算比行预算更紧时，尾部会从候选行的
/// 前端再裁一刀，允许输出的第一行是某一行的部分内容（而不是像头部那样整行丢弃）。
pub fn truncate_tail(input: &str, limits: &TruncateLimits) -> Truncated {
    if input.is_empty() {
        return Truncated {
            text: String::new(),
            dropped_lines: 0,
            dropped_bytes: 0,
            first_line_exceeds_limit: false,
        };
    }
    let total_bytes = input.len();
    let lines: Vec<&str> = lines_with_terminator(input).collect();
    let total_lines = lines.len();

    // 行预算：只保留最后 max_lines 行（max_lines == 0 时保留 0 行）。
    let keep_from_line = total_lines.saturating_sub(limits.max_lines);
    let candidate: Vec<&str> = lines
        .get(keep_from_line..)
        .map(<[&str]>::to_vec)
        .unwrap_or_default();
    let candidate_text: String = candidate.concat();

    if candidate_text.len() <= limits.max_bytes {
        let dropped_bytes = total_bytes - candidate_text.len();
        return Truncated {
            text: candidate_text,
            dropped_lines: keep_from_line,
            dropped_bytes,
            first_line_exceeds_limit: false,
        };
    }

    // 字节预算比行预算更紧：从候选文本前端再裁掉一截，允许切在某一行中间——
    // 这正是 tail 与 head 不对称的地方：head 遇到超限整行直接丢弃，tail 保留该行的尾部片段。
    let trimmed = take_suffix_within(&candidate_text, limits.max_bytes).to_owned();
    let offset = candidate_text.len() - trimmed.len();

    let mut consumed = 0usize;
    let mut fully_dropped_in_candidate = 0usize;
    for line in &candidate {
        if consumed + line.len() <= offset {
            consumed += line.len();
            fully_dropped_in_candidate += 1;
        } else {
            break;
        }
    }

    Truncated {
        dropped_bytes: total_bytes - trimmed.len(),
        dropped_lines: keep_from_line + fully_dropped_in_candidate,
        text: trimmed,
        first_line_exceeds_limit: false,
    }
}

/// 中段省略：头部保留一半预算、尾部保留另一半，中间用一行省略标记连接。
/// `≤ 1` 行时无法做"头 N 行 + 尾 M 行"的省略，退化为按字节的内联封顶文案
/// （见 [`enforce_inline_byte_cap`]）。
#[must_use]
pub fn truncate_middle(input: &str, limits: &TruncateLimits) -> Truncated {
    if input.is_empty() {
        return Truncated {
            text: String::new(),
            dropped_lines: 0,
            dropped_bytes: 0,
            first_line_exceeds_limit: false,
        };
    }
    let total_bytes = input.len();
    let total_lines = count_lines(input);

    if total_lines <= 1 {
        let text = enforce_inline_byte_cap(input, limits.max_bytes);
        let dropped_bytes = total_bytes.saturating_sub(text.len());
        return Truncated {
            text,
            dropped_lines: 0,
            dropped_bytes,
            first_line_exceeds_limit: false,
        };
    }

    if total_bytes <= limits.max_bytes && total_lines <= limits.max_lines {
        return Truncated {
            text: input.to_owned(),
            dropped_lines: 0,
            dropped_bytes: 0,
            first_line_exceeds_limit: false,
        };
    }

    // 头尾各分一半预算；奇数时头部少拿、尾部多拿（保证尾部至少有一半，呼应 OutputSink 头窗的钳位）。
    let head_limits = TruncateLimits {
        max_lines: limits.max_lines / 2,
        max_bytes: limits.max_bytes / 2,
        max_columns: limits.max_columns,
    };
    let tail_limits = TruncateLimits {
        max_lines: limits.max_lines - head_limits.max_lines,
        max_bytes: limits.max_bytes - head_limits.max_bytes,
        max_columns: limits.max_columns,
    };

    let head = truncate_head(input, &head_limits);
    let tail = truncate_tail(input, &tail_limits);

    let head_kept_lines = count_lines(&head.text);
    let tail_kept_lines = count_lines(&tail.text);

    // 防御：极端预算下头尾窗口可能覆盖了全部内容——此时没什么可省略的，退化为纯头部截断。
    if head_kept_lines + tail_kept_lines >= total_lines
        || head.text.len() + tail.text.len() >= total_bytes
    {
        return truncate_head(input, limits);
    }

    let dropped_lines = total_lines - head_kept_lines - tail_kept_lines;
    let dropped_bytes = total_bytes - head.text.len() - tail.text.len();

    let mut text = String::with_capacity(head.text.len() + tail.text.len() + 32);
    text.push_str(head.text.trim_end_matches('\n'));
    write!(
        &mut text,
        "\n… 省略 {dropped_lines} 行 / {dropped_bytes} 字节 …\n"
    )
    .ok();
    text.push_str(tail.text.trim_start_matches('\n'));

    Truncated {
        text,
        dropped_lines,
        dropped_bytes,
        first_line_exceeds_limit: false,
    }
}

/// 内联字节封顶：把 `input` 收窄到最多 `max_bytes` 字节，中段用省略标记连接头尾。
/// 预算规划为头部 60% / 尾部 25% / 省略标记（含被丢弃字节数的文案）最多 15%，但这只是
/// 规划比例——最终拼接结果会被显式校验，必要时退化为纯头部硬截断，因此调用方可以依赖
/// "结果字节数绝不超过 `max_bytes`"这一不变量，无论输入长度或 `max_bytes` 取值多极端。
#[must_use]
pub fn enforce_inline_byte_cap(input: &str, max_bytes: usize) -> String {
    if input.len() <= max_bytes {
        return input.to_owned();
    }
    if max_bytes == 0 {
        return String::new();
    }

    let head_budget = max_bytes.saturating_mul(3) / 5; // 60%
    let tail_budget = max_bytes / 4; // 25%
    let head = take_prefix_within(input, head_budget);
    let tail = take_suffix_within(input, tail_budget);
    let dropped = input.len().saturating_sub(head.len() + tail.len());

    let mut result = String::with_capacity(head.len() + tail.len() + 32);
    result.push_str(head);
    write!(&mut result, "…(省略 {dropped} 字节)…").ok();
    result.push_str(tail);

    if result.len() > max_bytes {
        // 省略标记文案的长度会随字节数的位数浮动，极小的 max_bytes 可能装不下它；
        // 此时放弃省略标记，退化为纯粹的头部硬截断，保证不变量始终成立。
        return take_prefix_within(input, max_bytes).to_owned();
    }
    result
}

/// 按显示宽度给每行加上一个列数上限：触顶的行只补一个 `…`（不是硬切掉超出部分的原始字符）。
///
/// 必须调用 [`crate::width::truncate_to_width`]，不自己按字节或字符数截断——上游对同一个
/// 512 列预算，native 侧按字节截（`grep.rs:325-334`），JS 侧按字符截
/// （`streaming-output.ts:266-270`），CJK 内容下两者的截断位置能差出三倍。本仓只保留
/// 一套、按显示宽度的实现。
#[must_use]
pub fn cap_columns(input: &str, max_columns: usize) -> String {
    let mut out = String::with_capacity(input.len());
    for line in lines_with_terminator(input) {
        let (content, terminator) = split_line_terminator(line);
        let capped = truncate_to_width(content, max_columns, "…");
        out.push_str(&capped);
        out.push_str(terminator);
    }
    out
}

/// 流式输出缓冲：头窗（固定前缀）+ 滚动尾窗（按行淘汰的环形缓冲），跨 `push` 调用保持
/// 每行的列宽裁剪状态，使得同一行无论被拆成多少次 `push` 写入，触顶时也只补一个省略号。
///
/// 移植自 oh-my-pi `OutputSink`（`streaming-output.ts:733-1337`）。列宽相关的可见宽度计算
/// 全部委托给 [`crate::width`]（`visible_width_up_to` 早退检测 + `truncate_to_width` 取实际
/// 截断内容），本模块不自己重新实现 grapheme/宽度扫描。已知限制：`visible_width_up_to` /
/// `truncate_to_width` 按每次调用传入的子串独立扫描 ANSI 状态机、不维护跨调用状态——如果
/// 一个 `ESC[...m` 转义序列恰好被切在两次 `push` 调用之间，裸露的半截序列会被误判宽度。
/// 这是任何不做转义序列前瞻缓冲的流式实现的固有限制，不影响不含颜色码的纯文本输出。
#[derive(Debug)]
pub struct OutputSink {
    limits: TruncateLimits,
    // "…" 的显示宽度，构造时算一次，避免每次 push 都重算。
    ellipsis_width: usize,

    // 头窗：一旦装满就不再变化，钳到 max_bytes / 2（保证尾窗至少拿一半）。
    head_capacity_lines: usize,
    head_capacity_bytes: usize,
    head_text: String,
    head_lines: usize,
    head_full: bool,

    // 滚动尾窗：按行组织的双端队列，超出容量就从队首整行淘汰；只剩一行仍超限时对那一行
    // 做字节级裁剪（呼应 truncate_tail 的"允许部分首行"）。
    tail_capacity_lines: usize,
    tail_capacity_bytes: usize,
    tail_lines: VecDeque<String>,
    tail_bytes: usize,

    // 当前正在累积、尚未遇到换行符的行（已按列宽裁剪）。
    pending_line: String,
    // 当前行已消费的显示宽度（跨 chunk 累计）。
    pending_width: usize,
    // 当前行是否已经补过省略号；补过之后同一行的后续字节只计入 column_dropped_bytes。
    pending_capped: bool,

    total_bytes: usize,
    total_lines: usize,
    // 因列宽封顶而丢弃的字节数——这部分不计入"中段省略"的统计。
    column_dropped_bytes: usize,
}

impl OutputSink {
    /// 用给定上限创建一个新的流式截断缓冲。
    #[must_use]
    pub fn new(limits: TruncateLimits) -> Self {
        let head_capacity_bytes = limits.max_bytes / 2;
        let head_capacity_lines = limits.max_lines / 2;
        let tail_capacity_bytes = limits.max_bytes - head_capacity_bytes;
        let tail_capacity_lines = limits.max_lines - head_capacity_lines;
        let ellipsis_width = visible_width("…");
        Self {
            limits,
            ellipsis_width,
            head_capacity_lines,
            head_capacity_bytes,
            head_text: String::new(),
            head_lines: 0,
            head_full: false,
            tail_capacity_lines,
            tail_capacity_bytes,
            tail_lines: VecDeque::new(),
            tail_bytes: 0,
            pending_line: String::new(),
            pending_width: 0,
            pending_capped: false,
            total_bytes: 0,
            total_lines: 0,
            column_dropped_bytes: 0,
        }
    }

    /// 清空所有状态，恢复为刚创建时的样子（复用同一份 `limits`）。
    pub fn reset(&mut self) {
        self.head_text.clear();
        self.head_lines = 0;
        self.head_full = false;
        self.tail_lines.clear();
        self.tail_bytes = 0;
        self.pending_line.clear();
        self.pending_width = 0;
        self.pending_capped = false;
        self.total_bytes = 0;
        self.total_lines = 0;
        self.column_dropped_bytes = 0;
    }

    /// 写入一段新到达的输出。可以在任意字节边界处被拆成多次调用——即使一行内容横跨多个
    /// `push`，列宽封顶状态也会跨调用保持，触顶只补一个省略号。
    pub fn push(&mut self, chunk: &str) {
        self.total_bytes += chunk.len();
        let mut rest = chunk;

        while !rest.is_empty() {
            // 先切出这一行剩余部分的边界：下一个 '\n'，或者本次 chunk 的末尾。
            let (frag, has_newline, tail) = match memchr::memchr(b'\n', rest.as_bytes()) {
                Some(i) => (
                    rest.get(..i).unwrap_or(""),
                    true,
                    rest.get(i + 1..).unwrap_or(""),
                ),
                None => (rest, false, ""),
            };

            if self.pending_capped {
                // 本行已经补过省略号：frag 整段都只是要被丢弃、计数的字节。
                self.column_dropped_bytes += frag.len();
            } else if self.limits.max_columns == 0 {
                // 列宽预算为 0：整行都不展示，与 `truncate_to_width` 的 max_width == 0
                // 语义一致——连省略号都不放。
                self.pending_capped = true;
                self.column_dropped_bytes += frag.len();
            } else {
                let budget = self.limits.max_columns.saturating_sub(self.ellipsis_width);
                let remaining = budget.saturating_sub(self.pending_width);
                let (added_width, exceeded) = visible_width_up_to(frag, remaining);
                if exceeded {
                    // frag 会让这一行触顶：向 width.rs 要实际能塞进剩余预算的前缀
                    // （ellipsis 传空串，因为省略号由这里统一追加一次）。
                    let fitting = truncate_to_width(frag, remaining, "");
                    self.column_dropped_bytes += frag.len() - fitting.len();
                    self.pending_line.push_str(&fitting);
                    self.pending_line.push('…');
                    self.pending_capped = true;
                } else {
                    self.pending_line.push_str(frag);
                    self.pending_width += added_width;
                }
            }

            if has_newline {
                self.close_current_line(true);
            }
            rest = tail;
        }
    }

    /// 结束流式写入，汇总头窗 + 省略标记（如有丢弃）+ 尾窗为最终结果。
    #[must_use]
    pub fn finish(mut self) -> Truncated {
        if !self.pending_line.is_empty() || self.pending_capped {
            self.close_current_line(false);
        }

        // 列宽裁剪丢掉的字节不算"中段省略"：用扣除掉它们之后的有效总字节数对账。
        let effective_total_bytes = self.total_bytes.saturating_sub(self.column_dropped_bytes);
        let kept_lines = self.head_lines + self.tail_lines.len();
        let kept_bytes = self.head_text.len() + self.tail_bytes;
        let dropped_lines = self.total_lines.saturating_sub(kept_lines);
        let dropped_bytes = effective_total_bytes.saturating_sub(kept_bytes);

        let mut text = self.head_text;
        if dropped_lines > 0 || dropped_bytes > 0 {
            write!(
                &mut text,
                "\n… 省略 {dropped_lines} 行 / {dropped_bytes} 字节 …\n"
            )
            .ok();
        }
        for line in self.tail_lines {
            text.push_str(&line);
        }

        Truncated {
            text,
            dropped_lines,
            dropped_bytes,
            first_line_exceeds_limit: false,
        }
    }

    /// 结束当前正在累积的行：`terminated` 为 `true` 时补一个 `'\n'`（真正遇到换行符），
    /// 为 `false` 时不补（`finish()` 收尾一段没有换行符的残留内容）。
    fn close_current_line(&mut self, terminated: bool) {
        self.total_lines += 1;
        let mut line = std::mem::take(&mut self.pending_line);
        if terminated {
            line.push('\n');
        }
        self.pending_width = 0;
        self.pending_capped = false;
        self.route_line(line);
    }

    /// 把一整行（已按列宽裁剪好）路由进头窗或尾窗。
    fn route_line(&mut self, line: String) {
        if !self.head_full {
            let would_bytes = self.head_text.len() + line.len();
            let would_lines = self.head_lines + 1;
            if would_bytes <= self.head_capacity_bytes && would_lines <= self.head_capacity_lines {
                self.head_text.push_str(&line);
                self.head_lines = would_lines;
                return;
            }
            self.head_full = true;
        }

        if self.tail_capacity_lines == 0 || self.tail_capacity_bytes == 0 {
            // 尾窗预算为 0：这一行整体都进不了输出，直接丢弃（总字节/总行数已在别处计数）。
            return;
        }

        self.tail_bytes += line.len();
        self.tail_lines.push_back(line);
        self.evict_tail_overflow();
    }

    /// 让尾窗回到容量以内：优先整行淘汰队首；只剩一行仍超限（该行本身比尾窗预算还大）
    /// 时，直接对它做字节级裁剪，只留下能塞进预算的尾部片段。
    fn evict_tail_overflow(&mut self) {
        loop {
            if self.tail_bytes <= self.tail_capacity_bytes
                && self.tail_lines.len() <= self.tail_capacity_lines
            {
                return;
            }
            if self.tail_lines.len() > 1 {
                if let Some(evicted) = self.tail_lines.pop_front() {
                    self.tail_bytes -= evicted.len();
                }
                continue;
            }
            if let Some(only) = self.tail_lines.pop_front() {
                let trimmed = take_suffix_within(&only, self.tail_capacity_bytes).to_owned();
                self.tail_bytes = trimmed.len();
                if !trimmed.is_empty() {
                    self.tail_lines.push_back(trimmed);
                }
            }
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits(max_lines: usize, max_bytes: usize, max_columns: usize) -> TruncateLimits {
        TruncateLimits {
            max_lines,
            max_bytes,
            max_columns,
        }
    }

    #[test]
    fn default_limits_match_upstream_constants() {
        let l = TruncateLimits::default();
        assert_eq!(l.max_lines, 3000);
        assert_eq!(l.max_bytes, 50_000);
        assert_eq!(l.max_columns, 512);
    }

    // ---- truncate_head ----

    #[test]
    fn truncate_head_keeps_everything_under_budget() {
        let input = "a\nb\nc\n";
        let result = truncate_head(input, &limits(10, 100, 80));
        assert_eq!(result.text, input);
        assert_eq!(result.dropped_lines, 0);
        assert_eq!(result.dropped_bytes, 0);
        assert!(!result.first_line_exceeds_limit);
    }

    #[test]
    fn truncate_head_first_line_exceeds_budget_returns_empty_not_partial() {
        let input = "a".repeat(1000); // 单行，没有换行符
        let result = truncate_head(&input, &limits(100, 100, 80));
        assert_eq!(result.text, "");
        assert!(result.first_line_exceeds_limit);
        assert_eq!(result.dropped_lines, 1);
        assert_eq!(result.dropped_bytes, 1000);
    }

    #[test]
    fn truncate_head_drops_whole_line_when_it_does_not_fit() {
        // "aa\n"(3) + "bb\n"(3) 都能塞进 5 字节预算的前两行不行——预算刚好只够第一行。
        let input = "aa\nbbbbbbbb\ncc\n";
        let result = truncate_head(input, &limits(10, 3, 80));
        assert_eq!(result.text, "aa\n");
        assert!(!result.first_line_exceeds_limit);
        assert_eq!(result.dropped_lines, 2);
        assert_eq!(result.dropped_bytes, input.len() - 3);
    }

    #[test]
    fn truncate_head_respects_max_lines() {
        let input = "1\n2\n3\n4\n5\n";
        let result = truncate_head(input, &limits(2, 1000, 80));
        assert_eq!(result.text, "1\n2\n");
        assert_eq!(result.dropped_lines, 3);
    }

    // ---- truncate_tail ----

    #[test]
    fn truncate_tail_keeps_everything_under_budget() {
        let input = "a\nb\nc\n";
        let result = truncate_tail(input, &limits(10, 100, 80));
        assert_eq!(result.text, input);
        assert_eq!(result.dropped_lines, 0);
        assert_eq!(result.dropped_bytes, 0);
    }

    #[test]
    fn truncate_tail_returns_partial_first_line_unlike_head() {
        // 与 truncate_head_first_line_exceeds_budget_returns_empty_not_partial 用同样的输入：
        // head 返回空 + 标记位；tail 必须返回非空的部分内容，体现不对称。
        let input = "a".repeat(1000);
        let head = truncate_head(&input, &limits(100, 100, 80));
        let tail = truncate_tail(&input, &limits(100, 100, 80));

        assert_eq!(head.text, "");
        assert!(head.first_line_exceeds_limit);

        assert_eq!(tail.text.len(), 100);
        assert!(tail.text.chars().all(|c| c == 'a'));
        assert!(!tail.first_line_exceeds_limit);
        assert_eq!(tail.dropped_bytes, 900);
    }

    #[test]
    fn truncate_tail_respects_max_lines() {
        let input = "1\n2\n3\n4\n5\n";
        let result = truncate_tail(input, &limits(2, 1000, 80));
        assert_eq!(result.text, "4\n5\n");
        assert_eq!(result.dropped_lines, 3);
    }

    #[test]
    fn truncate_tail_zero_byte_budget_is_empty() {
        let input = "line1\nline2\n";
        let result = truncate_tail(input, &limits(100, 0, 80));
        assert_eq!(result.text, "");
        assert_eq!(result.dropped_bytes, input.len());
    }

    // ---- truncate_middle ----

    #[test]
    fn truncate_middle_degenerates_to_byte_caption_for_single_line() {
        let input = "x".repeat(500);
        let result = truncate_middle(&input, &limits(3000, 200, 80));
        assert!(result.text.len() <= 200);
        assert!(result.text.contains("省略"));
        assert_eq!(result.dropped_lines, 0);
    }

    #[test]
    fn truncate_middle_keeps_short_single_line_unchanged() {
        let input = "short line, no newline";
        let result = truncate_middle(input, &limits(3000, 50_000, 80));
        assert_eq!(result.text, input);
        assert_eq!(result.dropped_bytes, 0);
    }

    #[test]
    fn truncate_middle_general_case_has_head_marker_and_tail() {
        let mut input = String::new();
        for i in 0..100 {
            writeln!(&mut input, "line{i}").ok();
        }
        let result = truncate_middle(&input, &limits(10, 1_000_000, 80));
        assert!(result.text.starts_with("line0\n"));
        assert!(result.text.trim_end().ends_with("line99"));
        assert!(result.text.contains('…'));
        assert!(result.dropped_lines > 0);
    }

    // ---- enforce_inline_byte_cap ----

    #[test]
    fn enforce_inline_byte_cap_short_input_is_unchanged() {
        let input = "hello";
        assert_eq!(enforce_inline_byte_cap(input, 100), input);
    }

    #[test]
    fn enforce_inline_byte_cap_exact_length_is_unchanged() {
        let input = "hello";
        assert_eq!(enforce_inline_byte_cap(input, input.len()), input);
    }

    #[test]
    fn enforce_inline_byte_cap_never_exceeds_budget() {
        let inputs = ["", "x", "short", &"y".repeat(1000), &"z".repeat(1_000_000)];
        let budgets = [0usize, 1, 2, 3, 5, 10, 50, 200];
        for input in inputs {
            for &budget in &budgets {
                let capped = enforce_inline_byte_cap(input, budget);
                assert!(
                    capped.len() <= budget,
                    "input_len={} budget={budget} capped_len={}",
                    input.len(),
                    capped.len()
                );
            }
        }
    }

    // ---- cap_columns ----

    #[test]
    fn cap_columns_truncates_cjk_line_by_display_width_not_bytes() {
        // 50 个中文字符，每个显示宽度 2、字节长度 3 —— 总宽度 100、总字节 150。
        let input = "中".repeat(50);
        let max_columns = 20;
        let result = cap_columns(&input, max_columns);
        let visible_width_of_result: usize = visible_width(&result);
        assert!(
            visible_width_of_result <= max_columns,
            "visible_width={visible_width_of_result}"
        );
        // 真正发生了截断：不是原样返回。
        assert_ne!(result, input);
        // 按字节暴力截到 20 字节会切在字符中间产生非法 UTF-8；我们的实现绝不会这样，
        // 用字节数远小于按字符数截断的结果来间接证明是按“列”而不是按“字节”截的。
        assert!(result.len() < input.len());
    }

    #[test]
    fn cap_columns_leaves_short_ascii_line_untouched() {
        let input = "hello\nworld\n";
        assert_eq!(cap_columns(input, 80), input);
    }

    // ---- OutputSink ----

    #[test]
    fn output_sink_single_ellipsis_across_chunk_boundary() {
        let max_columns = 5;
        let ellipsis_width = visible_width("…");
        let budget = max_columns - ellipsis_width;
        let source = "abcdefgh";
        let expected_prefix = take_prefix_within(source, budget);

        let mut sink = OutputSink::new(limits(100, 10_000, max_columns));
        sink.push(source); // 触顶发生在这个 chunk 内部
        sink.push("ijkl\nmore"); // 换行符也跨 chunk 到达；"more" 是新的一行
        let result = sink.finish();

        assert_eq!(
            result.text.matches('…').count(),
            1,
            "text={:?}",
            result.text
        );
        assert!(
            result.text.starts_with(&format!("{expected_prefix}…\n")),
            "text={:?} expected_prefix={expected_prefix:?}",
            result.text
        );
        assert!(result.text.ends_with("more"));
    }

    #[test]
    fn output_sink_ellipsis_count_independent_of_chunking() {
        // 同样的内容，用一次性 push 和逐字节 push 两种方式喂给 sink，结果必须完全一致。
        let content = "0123456789 more text here\nsecond line\n";
        let l = limits(100, 10_000, 8);

        let mut whole = OutputSink::new(l);
        whole.push(content);
        let whole_result = whole.finish();

        let mut byte_by_byte = OutputSink::new(l);
        for byte in content.as_bytes() {
            // 全 ASCII 输入，每个字节本身就是合法的 UTF-8 子串。
            let s = std::str::from_utf8(std::slice::from_ref(byte)).unwrap_or("");
            byte_by_byte.push(s);
        }
        let byte_result = byte_by_byte.finish();

        assert_eq!(whole_result.text, byte_result.text);
        assert_eq!(whole_result.dropped_lines, byte_result.dropped_lines);
        assert_eq!(whole_result.dropped_bytes, byte_result.dropped_bytes);
    }

    #[test]
    fn output_sink_head_and_tail_windows_with_single_large_chunk() {
        let mut chunk = String::new();
        for i in 0..10 {
            writeln!(&mut chunk, "line{i}").ok();
        }
        let mut sink = OutputSink::new(limits(3, 30, 1000));
        sink.push(&chunk);
        let result = sink.finish();

        assert!(result.text.starts_with("line0\n"));
        assert!(result.text.ends_with("line8\nline9\n"));
        assert_eq!(result.dropped_lines, 7);
        assert_eq!(result.dropped_bytes, 42);
    }

    #[test]
    fn output_sink_reset_clears_all_state() {
        let mut sink = OutputSink::new(limits(100, 10_000, 80));
        sink.push("first line one\nsecond line\n");
        sink.reset();
        sink.push("second\n");
        let result = sink.finish();

        assert_eq!(result.text, "second\n");
        assert_eq!(result.dropped_lines, 0);
        assert_eq!(result.dropped_bytes, 0);
    }

    #[test]
    fn output_sink_empty_input_produces_empty_result() {
        let sink = OutputSink::new(TruncateLimits::default());
        let result = sink.finish();
        assert_eq!(result.text, "");
        assert_eq!(result.dropped_lines, 0);
        assert_eq!(result.dropped_bytes, 0);
    }

    #[test]
    fn output_sink_zero_columns_hides_content_without_ellipsis() {
        let mut sink = OutputSink::new(limits(100, 10_000, 0));
        sink.push("hello\n");
        let result = sink.finish();
        assert_eq!(result.text, "\n");
        assert!(!result.text.contains('…'));
    }
}
