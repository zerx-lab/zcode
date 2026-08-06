//! ANSI 感知的显示宽度计算、按列截断/换行、制表符展开与控制字符清洗。
//!
//! 宽度一律经 `unicode-width` 计算，绝不用 `str::len()`。多码点 grapheme 簇
//! （ZWJ 序列、VS16 emoji 表现选择符、keycap 等）必须整簇求宽度，不能逐字符累加，
//! 否则会把 `👨‍👩‍👧‍👦`（正确宽度 2）算成 8 列。详见 [`grapheme_width`]。

use std::borrow::Cow;
use std::ops::Range;

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// 默认制表符宽度（列数）。
///
/// 上游实现用 3，但核实后既无 bench 也无 issue 或注释说明理由。本仓取 4：
/// 与绝大多数编辑器、`git diff`/终端默认一致，代码片段粘进终端时缩进层级不会错位。
pub const DEFAULT_TAB_WIDTH: usize = 4;

const ESC: char = '\u{1B}';

/// 是否是 C1 控制码（`U+0080..=U+009F`）——7-bit `ESC` 序列的单字符等价形式，
/// 一些遗留终端/协议会直接发送这个而不是 `ESC` 前缀形式。
fn is_c1(c: char) -> bool {
    ('\u{80}'..='\u{9F}').contains(&c)
}

/// 扫描过程中识别出的一个视觉 token；区间均为原字符串上的合法 UTF-8 边界。
#[derive(Debug)]
enum Token {
    /// ANSI/C1 控制序列：显示宽度恒为 0。
    Control(Range<usize>),
    /// 制表符（单字节 `\t`）：展开宽度取决于当前列，由调用方按 `tab_width` 计算。
    Tab(usize),
    /// 换行符（单字节 `\n`）：宽度 0，列计数器归零。
    Newline(usize),
    /// 一个内容簇：单字符或非 ASCII 的完整 grapheme 簇，宽度已算好。
    Content { range: Range<usize>, width: usize },
}

/// 把字符串分解成 [`Token`] 流的惰性扫描器，是本模块除纯宽度求和之外
/// 全部公开函数共用的核心：每个 token 恰好是一个不可再分的视觉单元，
/// 保证 truncate/wrap/`display_col_to_byte` 等按位置操作的函数总能在
/// 任意 token 边界停下来，不会切碎宽字符、tab 或转义序列。
struct Tokenizer<'a> {
    s: &'a str,
    pos: usize,
}

impl<'a> Tokenizer<'a> {
    fn new(s: &'a str) -> Self {
        Self { s, pos: 0 }
    }
}

impl Iterator for Tokenizer<'_> {
    type Item = Token;

    fn next(&mut self) -> Option<Token> {
        let rest = self.s.get(self.pos..)?;
        let ch = rest.chars().next()?;

        if ch == ESC || is_c1(ch) {
            let len = scan_control_sequence(rest, ch);
            let start = self.pos;
            self.pos += len;
            return Some(Token::Control(start..self.pos));
        }
        if ch == '\t' {
            let pos = self.pos;
            self.pos += ch.len_utf8();
            return Some(Token::Tab(pos));
        }
        if ch == '\n' {
            let pos = self.pos;
            self.pos += ch.len_utf8();
            return Some(Token::Newline(pos));
        }
        if ch == '\r' {
            // 单独处理：CR 自身宽度为 0，且绝不能被 grapheme 分段和随后的 LF
            // 合并成一簇（unicode-width 把裸的 "\r\n" 当一簇、宽度算 1，
            // 这会让后续 Newline token 的列归零逻辑失效）。
            let start = self.pos;
            let end = start + ch.len_utf8();
            self.pos = end;
            return Some(Token::Content {
                range: start..end,
                width: 0,
            });
        }

        // 逐簇分段：每个 token 恰好是一个 grapheme 簇，不能只看首字符是不是
        // ASCII 就下结论切在哪（keycap 是"数字 + VS16 + 组合封闭符"跨 ASCII/
        // 非 ASCII 边界成簇的典型例子）。批量合并多个簇成一个 token 的
        // "ASCII 快路径"只用于纯宽度求和场景，见 [`scan_ascii_width_with_limit`]；
        // 这里绝不能做同样的合并，否则一整段 ASCII 会变成不可再分的原子 token，
        // 宽度超限时截断/换行只能整段丢弃或整段保留。
        let g = rest.graphemes(true).next()?;
        let start = self.pos;
        let end = start + g.len();
        self.pos = end;
        let width = grapheme_width(g);
        Some(Token::Content {
            range: start..end,
            width,
        })
    }
}

/// 单个 char 的显示宽度；无定义宽度的控制字符按 0 处理。
fn char_width(c: char) -> usize {
    UnicodeWidthChar::width(c).unwrap_or(0)
}

/// 一个 grapheme 簇的显示宽度。单码点簇走 [`char_width`]（更快、且语义等价）；
/// 多码点簇必须把整簇一起交给 `UnicodeWidthStr::width`——反例：拆开逐字符累加会把
/// ZWJ 家庭 emoji（`👨‍👩‍👧‍👦`，4 个 emoji + 3 个 ZWJ）算成 8 列（4 个组件各 2 列之和），
/// 而它作为一簇整体的正确显示宽度是 2 列；keycap（`1️⃣`）同理，拆开算是 1 列，整簇算是 2 列。
fn grapheme_width(g: &str) -> usize {
    let mut chars = g.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => char_width(c),
        _ => UnicodeWidthStr::width(g),
    }
}

/// 识别从 `rest`（首字符已确认是 `lead`，即 `ESC` 或 C1 引导符）开始的一个完整控制序列，
/// 返回其字节长度。覆盖 CSI（含 SGR）、OSC（含 OSC8 超链接）、DCS/SOS/PM/APC、
/// 字符集指定（SCS）、通用双字符转义，以及裸 C1 控制码。
/// 序列在字符串末尾被截断时，吞掉剩余全部字节，保证调用方总能前进、不会死循环。
fn scan_control_sequence(rest: &str, lead: char) -> usize {
    let lead_len = lead.len_utf8();

    if lead == ESC {
        let Some(after_esc) = rest.get(lead_len..) else {
            return rest.len();
        };
        let Some(second) = after_esc.chars().next() else {
            return rest.len(); // 末尾裸 ESC：整体吞掉
        };
        let from = lead_len + second.len_utf8();
        return match second {
            '[' => scan_after_csi_intro(rest, from),
            ']' | 'P' | 'X' | '^' | '_' => scan_string_terminated(rest, from),
            '(' | ')' | '*' | '+' | '-' | '.' | '/' => scan_fixed_len(rest, from, 1),
            _ => from, // 通用两字符转义，如 ESC c（RIS）、ESC 7/8
        };
    }

    // 8-bit 形式：C1 控制码本身即引导符，无需 ESC 前缀。
    match lead {
        '\u{9B}' => scan_after_csi_intro(rest, lead_len), // CSI
        '\u{9D}' | '\u{90}' | '\u{98}' | '\u{9E}' | '\u{9F}' => {
            scan_string_terminated(rest, lead_len) // OSC/DCS/SOS/PM/APC
        }
        _ => lead_len, // 其余裸 C1 控制码，单字符即完整序列
    }
}

/// 从 CSI 引导符之后开始，消费参数字节（`0x30..=0x3F`）、中间字节（`0x20..=0x2F`），
/// 再消费一个最终字节（`0x40..=0x7E`），返回整段序列（从 `rest` 起点算）的字节长度。
/// 这些字节按 ECMA-48 定义恒为 ASCII，逐字节扫描不会破坏 UTF-8 边界。
fn scan_after_csi_intro(rest: &str, from: usize) -> usize {
    let bytes = rest.as_bytes();
    let mut i = from;
    while let Some(&b) = bytes.get(i) {
        if (0x30..=0x3F).contains(&b) {
            i += 1;
        } else {
            break;
        }
    }
    while let Some(&b) = bytes.get(i) {
        if (0x20..=0x2F).contains(&b) {
            i += 1;
        } else {
            break;
        }
    }
    match bytes.get(i) {
        Some(&b) if (0x40..=0x7E).contains(&b) => i + 1,
        _ => rest.len(), // 截断/畸形：吞掉剩余部分
    }
}

/// 消费一段"字符串型"控制序列（OSC/DCS/SOS/PM/APC）直到终止符：`BEL`（`\x07`）、
/// C1 `ST`（`U+009C`），或 7-bit `ST`（`ESC \\`）。OSC 的规范终止符是 `ST`，但
/// 实践中大量终端也接受 `BEL`（尤其是 OSC8 超链接），这里统一按更宽容的规则处理。
fn scan_string_terminated(rest: &str, from: usize) -> usize {
    let Some(tail) = rest.get(from..) else {
        return rest.len();
    };
    let mut iter = tail.char_indices();
    while let Some((offset, c)) = iter.next() {
        match c {
            '\u{07}' | '\u{9C}' => return from + offset + c.len_utf8(),
            _ if c == ESC => {
                let mut lookahead = iter.clone();
                if let Some((_, next)) = lookahead.next()
                    && next == '\\'
                {
                    let end_offset = lookahead.next().map_or(tail.len(), |(o, _)| o);
                    return from + end_offset;
                }
            }
            _ => {}
        }
    }
    rest.len() // 未找到终止符：吞掉剩余全部
}

/// 消费 `from` 之后固定 `n_more_chars` 个字符（用于字符集指定 SCS 的终结字节）。
fn scan_fixed_len(rest: &str, from: usize, n_more_chars: usize) -> usize {
    let Some(tail) = rest.get(from..) else {
        return rest.len();
    };
    let mut end = from;
    let mut chars = tail.chars();
    for _ in 0..n_more_chars {
        match chars.next() {
            Some(c) => end += c.len_utf8(),
            None => return rest.len(),
        }
    }
    end
}

/// 给定当前列，计算一个制表符展开到下一个 `tab_width` 整数倍列所需的空格数。
/// `tab_width == 0` 时没有对齐目标，制表符不产生任何空格（宽度 0）。
fn tab_stop_width(column: usize, tab_width: usize) -> usize {
    if tab_width == 0 {
        return 0;
    }
    let rem = column % tab_width;
    if rem == 0 { tab_width } else { tab_width - rem }
}

/// 宽度求和的核心扫描：返回 `(累计宽度, 是否在达到 limit 后提前退出)`。
/// `limit` 为 `None` 时扫描整个字符串；为 `Some(n)` 时一旦累计宽度超过 `n` 立即停止。
///
/// `s` 全是 ASCII 时走 [`scan_ascii_width_with_limit`] 快路径：ASCII 字符永远是独立的
/// 单码点簇，不可能出现 ZWJ/VS16/组合标记那样的跨字符簇，逐字节查宽度即可，不需要
/// （更贵的）grapheme 分段。这是纯粹的宽度计算优化，[`Tokenizer`] 本身绝不做同样的合并
/// ——那样会让 truncate/wrap 之类按位置操作的函数没法在合并出来的大 token 内部断开。
/// 代价：宽度逻辑因此劈成两条路径（这里 vs. 基于 `Tokenizer` 的通用路径），
/// 改任一条都必须检查另一条是否要同步改。
fn scan_width_with_limit(s: &str, tab_width: usize, limit: Option<usize>) -> (usize, bool) {
    if s.is_ascii() {
        return scan_ascii_width_with_limit(s, tab_width, limit);
    }
    let mut total = 0usize;
    let mut column = 0usize;
    for token in Tokenizer::new(s) {
        match token {
            Token::Control(_) => {}
            Token::Tab(_) => {
                let w = tab_stop_width(column, tab_width);
                total += w;
                column += w;
            }
            Token::Newline(_) => {
                column = 0;
            }
            Token::Content { width, .. } => {
                total += width;
                column += width;
            }
        }
        if let Some(limit) = limit
            && total > limit
        {
            return (total, true);
        }
    }
    (total, false)
}

/// [`scan_width_with_limit`] 的纯 ASCII 快路径：字节即字符即簇，逐字节判定，
/// 复用 [`scan_control_sequence`] 处理内嵌的 `ESC` 转义序列（纯 ASCII 输入里
/// 引导符必为 `ESC`，C1 引导符本身就是非 ASCII 字节，不会出现在这条路径）。
fn scan_ascii_width_with_limit(s: &str, tab_width: usize, limit: Option<usize>) -> (usize, bool) {
    let bytes = s.as_bytes();
    let mut i = 0usize;
    let mut total = 0usize;
    let mut column = 0usize;
    while let Some(&b) = bytes.get(i) {
        match b {
            0x1B => {
                let Some(rest) = s.get(i..) else { break };
                i += scan_control_sequence(rest, ESC);
            }
            b'\t' => {
                let w = tab_stop_width(column, tab_width);
                total += w;
                column += w;
                i += 1;
            }
            b'\n' => {
                column = 0;
                i += 1;
            }
            _ => {
                let w = char_width(char::from(b));
                total += w;
                column += w;
                i += 1;
            }
        }
        if let Some(limit) = limit
            && total > limit
        {
            return (total, true);
        }
    }
    (total, false)
}

/// 计算 `s` 的可见显示宽度（列数），制表符按 [`DEFAULT_TAB_WIDTH`] 对齐。
/// ANSI/C1 控制序列不计宽度但不影响后续内容的位置。
#[must_use]
pub fn visible_width(s: &str) -> usize {
    visible_width_with_tab(s, DEFAULT_TAB_WIDTH)
}

/// [`visible_width`] 的可定制制表符宽度版本。
#[must_use]
pub fn visible_width_with_tab(s: &str, tab_width: usize) -> usize {
    scan_width_with_limit(s, tab_width, None).0
}

/// 带早退的宽度计算：一旦累计宽度超过 `limit` 立即停止扫描。
/// 返回 `(已累计宽度, 是否超限)`；未超限时累计宽度就是 `s` 的完整可见宽度。
#[must_use]
pub fn visible_width_up_to(s: &str, limit: usize) -> (usize, bool) {
    visible_width_up_to_with_tab(s, limit, DEFAULT_TAB_WIDTH)
}

/// [`visible_width_up_to`] 的可定制制表符宽度版本。
#[must_use]
pub fn visible_width_up_to_with_tab(s: &str, limit: usize, tab_width: usize) -> (usize, bool) {
    scan_width_with_limit(s, tab_width, Some(limit))
}

/// 判断一段截取到的控制序列文本是不是 SGR（`ESC[...m` 或等价 C1 CSI 形式）。
/// 只有真正拷贝过 SGR 才需要在截断结尾补 `ESC[0m`，无条件补会污染纯文本行的输出。
fn is_sgr(control_text: &str) -> bool {
    control_text.ends_with('m')
        && (control_text.starts_with("\x1b[") || control_text.starts_with('\u{9B}'))
}

/// 按显示宽度截断字符串，超限部分替换为 `ellipsis`。
/// 不超限时返回 `Cow::Borrowed`（零分配）；`max_width == 0` 时返回空串；
/// `ellipsis` 本身宽度超过 `max_width` 时改为按 grapheme 边界截断 `ellipsis` 自身。
#[must_use]
pub fn truncate_to_width<'a>(s: &'a str, max_width: usize, ellipsis: &str) -> Cow<'a, str> {
    truncate_to_width_with_tab(s, max_width, ellipsis, DEFAULT_TAB_WIDTH)
}

/// [`truncate_to_width`] 的可定制制表符宽度版本。
#[must_use]
pub fn truncate_to_width_with_tab<'a>(
    s: &'a str,
    max_width: usize,
    ellipsis: &str,
    tab_width: usize,
) -> Cow<'a, str> {
    if max_width == 0 {
        return Cow::Owned(String::new());
    }

    let (_, exceeded) = visible_width_up_to_with_tab(s, max_width, tab_width);
    if !exceeded {
        return Cow::Borrowed(s);
    }

    let ellipsis_width = visible_width_with_tab(ellipsis, tab_width);
    if ellipsis_width > max_width {
        return Cow::Owned(truncate_plain_to_width(ellipsis, max_width, tab_width));
    }
    let budget = max_width - ellipsis_width;

    let mut out = String::with_capacity(s.len().min(1024));
    let mut column = 0usize;
    let mut copied_sgr = false;
    for token in Tokenizer::new(s) {
        match token {
            Token::Control(range) => {
                if let Some(text) = s.get(range) {
                    out.push_str(text);
                    if is_sgr(text) {
                        copied_sgr = true;
                    }
                }
            }
            Token::Tab(_) => {
                let w = tab_stop_width(column, tab_width);
                if column + w > budget {
                    break;
                }
                for _ in 0..w {
                    out.push(' ');
                }
                column += w;
            }
            Token::Newline(_) => break, // 截断只在单行预算内进行
            Token::Content { range, width } => {
                if column + width > budget {
                    break;
                }
                if let Some(text) = s.get(range) {
                    out.push_str(text);
                }
                column += width;
            }
        }
    }
    out.push_str(ellipsis);
    if copied_sgr {
        out.push_str("\x1b[0m");
    }
    Cow::Owned(out)
}

/// 不带省略号、纯按显示宽度截断（用于 ellipsis 自身超宽时的兜底）。
fn truncate_plain_to_width(s: &str, max_width: usize, tab_width: usize) -> String {
    let mut out = String::new();
    let mut column = 0usize;
    for token in Tokenizer::new(s) {
        match token {
            Token::Control(range) => {
                if let Some(text) = s.get(range) {
                    out.push_str(text);
                }
            }
            Token::Tab(_) => {
                let w = tab_stop_width(column, tab_width);
                if column + w > max_width {
                    break;
                }
                for _ in 0..w {
                    out.push(' ');
                }
                column += w;
            }
            Token::Newline(_) => break,
            Token::Content { range, width } => {
                if column + width > max_width {
                    break;
                }
                if let Some(text) = s.get(range) {
                    out.push_str(text);
                }
                column += width;
            }
        }
    }
    out
}

/// 是 SGR 就返回其参数子串（`ESC[` 与结尾 `m` 之间的部分），否则 `None`。
fn sgr_params(text: &str) -> Option<&str> {
    let inner = text
        .strip_prefix("\x1b[")
        .or_else(|| text.strip_prefix('\u{9B}'))?;
    inner.strip_suffix('m')
}

/// 是 OSC8 超链接序列就返回其 URI 部分（第二个 `;` 之后、终止符之前），否则 `None`。
fn osc8_uri(text: &str) -> Option<&str> {
    let inner = text
        .strip_prefix("\x1b]")
        .or_else(|| text.strip_prefix('\u{9D}'))?;
    let inner = inner.strip_prefix("8;")?;
    let semi = inner.find(';')?;
    let after = inner.get(semi + 1..)?;
    let uri = after
        .strip_suffix('\u{07}')
        .or_else(|| after.strip_suffix("\x1b\\"))
        .or_else(|| after.strip_suffix('\u{9C}'))
        .unwrap_or(after);
    Some(uri)
}

/// 根据一段刚扫描到的控制序列文本，维护换行时需要重放的 SGR/OSC8 超链接状态：
/// - SGR：纯 reset（参数为空或 `"0"`）清空 `sgr_prologue`；`"0;…"` 形式（复位后再设置）
///   同样先清空再记录这条序列；其余 SGR 追加进去（真实终端里样式是叠加生效的，
///   比如粗体和颜色是两条独立转义序列，必须都保留才能在新行开头正确重放）。
/// - OSC8：URI 为空表示关闭链接。
/// - 其它控制序列不改变跨行状态。
fn update_ansi_carry_state(text: &str, sgr_prologue: &mut String, hyperlink: &mut Option<String>) {
    if let Some(params) = sgr_params(text) {
        if params.is_empty() || params == "0" {
            sgr_prologue.clear();
        } else if params.starts_with("0;") {
            sgr_prologue.clear();
            sgr_prologue.push_str(text);
        } else {
            sgr_prologue.push_str(text);
        }
        return;
    }
    if let Some(uri) = osc8_uri(text) {
        *hyperlink = if uri.is_empty() {
            None
        } else {
            Some(text.to_owned())
        };
    }
}

/// 换行前重置样式（避免 underline/strike 等属性渗到行尾空白单元格，即终端的
/// BCE 渗色问题）、把当前行推入结果，再在新行开头重放跨行状态。
fn flush_wrap_line(
    lines: &mut Vec<String>,
    current: &mut String,
    column: &mut usize,
    sgr_prologue: &str,
    hyperlink: Option<&String>,
) {
    if !sgr_prologue.is_empty() {
        current.push_str("\x1b[0m");
    }
    lines.push(std::mem::take(current));
    *column = 0;
    if !sgr_prologue.is_empty() {
        current.push_str(sgr_prologue);
    }
    if let Some(link) = hyperlink {
        current.push_str(link);
    }
}

/// 按显示宽度硬换行；`width == 0` 无可用列，返回空集合。
/// 输入中已有的换行符也会作为强制断行点。SGR 与 OSC8 超链接状态跨行携带
/// （见 [`update_ansi_carry_state`] 与 [`flush_wrap_line`]）。
#[must_use]
pub fn wrap_to_width(s: &str, width: usize) -> Vec<String> {
    wrap_to_width_with_tab(s, width, DEFAULT_TAB_WIDTH)
}

/// [`wrap_to_width`] 的可定制制表符宽度版本。
#[must_use]
pub fn wrap_to_width_with_tab(s: &str, width: usize, tab_width: usize) -> Vec<String> {
    if width == 0 {
        return Vec::new();
    }
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut column = 0usize;
    let mut sgr_prologue = String::new();
    let mut hyperlink: Option<String> = None;

    for token in Tokenizer::new(s) {
        match token {
            Token::Control(range) => {
                if let Some(text) = s.get(range) {
                    update_ansi_carry_state(text, &mut sgr_prologue, &mut hyperlink);
                    current.push_str(text);
                }
            }
            Token::Tab(_) => {
                let mut w = tab_stop_width(column, tab_width);
                if column + w > width && column > 0 {
                    flush_wrap_line(
                        &mut lines,
                        &mut current,
                        &mut column,
                        &sgr_prologue,
                        hyperlink.as_ref(),
                    );
                    w = tab_stop_width(column, tab_width);
                }
                for _ in 0..w {
                    current.push(' ');
                }
                column += w;
            }
            Token::Newline(_) => {
                flush_wrap_line(
                    &mut lines,
                    &mut current,
                    &mut column,
                    &sgr_prologue,
                    hyperlink.as_ref(),
                );
            }
            Token::Content { range, width: cw } => {
                if column + cw > width && column > 0 {
                    flush_wrap_line(
                        &mut lines,
                        &mut current,
                        &mut column,
                        &sgr_prologue,
                        hyperlink.as_ref(),
                    );
                }
                if let Some(text) = s.get(range) {
                    current.push_str(text);
                }
                column += cw;
            }
        }
    }
    lines.push(current);
    lines
}

/// 把显示列映射到字节偏移。落在宽字符或制表符展开的空白中间时往前退一格，
/// 永远返回合法 UTF-8 边界；`col` 超出全部内容时返回 `s.len()`。
#[must_use]
pub fn display_col_to_byte(s: &str, col: usize) -> usize {
    display_col_to_byte_with_tab(s, col, DEFAULT_TAB_WIDTH)
}

/// [`display_col_to_byte`] 的可定制制表符宽度版本。
#[must_use]
pub fn display_col_to_byte_with_tab(s: &str, col: usize, tab_width: usize) -> usize {
    let mut column = 0usize;
    for token in Tokenizer::new(s) {
        match token {
            Token::Control(_) => {}
            Token::Tab(pos) => {
                let w = tab_stop_width(column, tab_width);
                if col < column + w {
                    return pos;
                }
                column += w;
            }
            Token::Newline(pos) => return pos,
            Token::Content { range, width } => {
                if col < column + width {
                    return range.start;
                }
                column += width;
            }
        }
    }
    s.len()
}

/// 把制表符按当前列展开成对齐用的空格（不是固定替换）：等宽网格里 tab 若原样保留
/// 会造成视觉空洞，必须按 [`DEFAULT_TAB_WIDTH`] 对齐展开。
#[must_use]
pub fn expand_tabs(s: &str) -> Cow<'_, str> {
    expand_tabs_with_tab(s, DEFAULT_TAB_WIDTH)
}

/// [`expand_tabs`] 的可定制制表符宽度版本。
#[must_use]
pub fn expand_tabs_with_tab(s: &str, tab_width: usize) -> Cow<'_, str> {
    if !s.contains('\t') {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    let mut column = 0usize;
    for token in Tokenizer::new(s) {
        match token {
            Token::Control(range) => {
                if let Some(text) = s.get(range) {
                    out.push_str(text);
                }
            }
            Token::Tab(_) => {
                let w = tab_stop_width(column, tab_width);
                for _ in 0..w {
                    out.push(' ');
                }
                column += w;
            }
            Token::Newline(pos) => {
                if let Some(text) = s.get(pos..pos + 1) {
                    out.push_str(text);
                }
                column = 0;
            }
            Token::Content { range, width } => {
                if let Some(text) = s.get(range) {
                    out.push_str(text);
                }
                column += width;
            }
        }
    }
    Cow::Owned(out)
}

/// 剥离字符串中的全部 ANSI/C1 控制序列（CSI/OSC/DCS/SOS/PM/APC 及裸 C1 控制码），
/// 保留其余内容原样。没有可剥离内容时返回 `Cow::Borrowed`（零分配）。
#[must_use]
pub fn strip_ansi(s: &str) -> Cow<'_, str> {
    match strip_ansi_owned(s) {
        Some(owned) => Cow::Owned(owned),
        None => Cow::Borrowed(s),
    }
}

fn strip_ansi_owned(s: &str) -> Option<String> {
    let mut out = String::new();
    let mut changed = false;
    let mut copied_to = 0usize;
    for token in Tokenizer::new(s) {
        if let Token::Control(range) = token {
            if let Some(text) = s.get(copied_to..range.start) {
                out.push_str(text);
            }
            changed = true;
            copied_to = range.end;
        }
    }
    if !changed {
        return None;
    }
    if let Some(text) = s.get(copied_to..) {
        out.push_str(text);
    }
    Some(out)
}

/// 第一步要清掉的字符：C0（保留 `\t` `\n`）、DEL、C1。`ESC` 单独保留给
/// [`strip_ansi`] 按完整序列处理——在这里把它当普通 C0 删掉，会把 `ESC[31m`
/// 拆成裸露的 `[31m` 留在文本里，比完全不清洗还难看。
fn is_removable_control(c: char) -> bool {
    matches!(
        c,
        '\u{00}'..='\u{08}' | '\u{0B}'..='\u{1A}' | '\u{1C}'..='\u{1F}' | '\u{7F}' | '\u{80}'..='\u{9F}'
    )
}

fn strip_controls_owned(s: &str) -> Option<String> {
    if !s.chars().any(is_removable_control) {
        return None;
    }
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if !is_removable_control(c) {
            out.push(c);
        }
    }
    Some(out)
}

/// 折叠连续 `>= 2` 个 `U+FFFD`（Unicode 替换字符）为单个：这类连续替换字符
/// 几乎总是上游把被截断的多字节 UTF-8（例如流式输出在字符中途被切断）做
/// 无损解码（`String::from_utf8_lossy`）时自己产生的噪声，而不是文本里
/// 本来就有的、有意义的单个替换标记；折叠掉能避免向用户展示一串 `"���"`。
fn collapse_fffd_runs_owned(s: &str) -> Option<String> {
    const FFFD: char = '\u{FFFD}';
    if s.matches(FFFD).count() < 2 {
        return None;
    }
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    let mut changed = false;
    while let Some(c) = chars.next() {
        out.push(c);
        if c == FFFD {
            let mut run = 1usize;
            while chars.peek() == Some(&FFFD) {
                chars.next();
                run += 1;
            }
            if run > 1 {
                changed = true;
            }
        }
    }
    if changed { Some(out) } else { None }
}

/// 清洗文本：先剥 C0（保留 `\t` `\n`）、DEL、C1，再折叠自产的连续 `U+FFFD`，
/// 最后只有清洗后仍含 `ESC` 时才走（更昂贵的）[`strip_ansi`] 状态机。
///
/// 三步顺序不能换：
/// 1. 必须先剥控制字符，否则 `ESC` 之外的杂散 C0/C1 会原样留在输出里；
/// 2. 必须在剥控制字符之后再折叠 `U+FFFD`，因为折叠的是"上游解码噪声"而不是
///    控制字符本身产生的东西，顺序颠倒不影响正确性但语义上"先清子噪声再清大噪声"更自然；
/// 3. 必须最后才判断是否含 `ESC`——如果先判断，C0 清洗可能已经把裸露的 `ESC`
///    过滤掉（`ESC` 也在 C0 范围内，但本函数特意排除了它），提前判断没有意义，
///    且会让判断依据落在"清洗前"的原始文本而不是即将返回的文本上，二者可能不一致。
#[must_use]
pub fn sanitize_text(s: &str) -> Cow<'_, str> {
    let mut owned: Option<String> = strip_controls_owned(s);
    let view: &str = owned.as_deref().unwrap_or(s);

    if let Some(collapsed) = collapse_fffd_runs_owned(view) {
        owned = Some(collapsed);
    }
    let view: &str = owned.as_deref().unwrap_or(s);

    if view.contains(ESC)
        && let Some(stripped) = strip_ansi_owned(view)
    {
        owned = Some(stripped);
    }

    match owned {
        Some(o) => Cow::Owned(o),
        None => Cow::Borrowed(s),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cjk_is_double_width() {
        assert_eq!(visible_width("中文"), 4);
    }

    #[test]
    fn zwj_family_emoji_is_two_columns() {
        assert_eq!(
            visible_width("\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}"),
            2
        );
    }

    #[test]
    fn vs16_presentation_selector_is_two_columns() {
        assert_eq!(visible_width("\u{2764}\u{FE0F}"), 2);
    }

    #[test]
    fn keycap_sequence_is_two_columns() {
        assert_eq!(visible_width("1\u{FE0F}\u{20E3}"), 2);
    }

    #[test]
    fn ansi_sgr_is_zero_width_but_preserved_on_truncate() {
        let s = "\x1b[31mred\x1b[0m";
        assert_eq!(visible_width(s), 3);
        let truncated = truncate_to_width(s, 10, "…");
        match truncated {
            Cow::Borrowed(b) => assert_eq!(b, s),
            Cow::Owned(_) => panic!("width 3 <= max 10 must borrow"),
        }
    }

    #[test]
    fn boundary_max_width_zero_returns_empty() {
        assert_eq!(truncate_to_width("hello", 0, "…"), "");
    }

    #[test]
    fn boundary_ellipsis_itself_too_wide_is_grapheme_truncated() {
        let out = truncate_to_width("hello world", 2, "....");
        assert_eq!(out, "..");
    }

    #[test]
    fn boundary_reset_only_appended_when_sgr_actually_copied() {
        let plain = truncate_to_width("hello world", 5, "…");
        assert!(!plain.contains("\x1b[0m"));

        let colored = truncate_to_width("\x1b[31mhello world", 8, "…");
        assert!(colored.contains("\x1b[0m"));
    }

    #[test]
    fn boundary_wrap_resets_before_break_and_replays_after() {
        let s = "\x1b[4maaaaabbbbb";
        let lines = wrap_to_width(s, 5);
        assert_eq!(lines.len(), 2);
        assert!(lines[0].ends_with("\x1b[0m"));
        assert!(lines[1].starts_with("\x1b[4m"));
    }

    #[test]
    fn boundary_display_col_to_byte_retreats_out_of_wide_char() {
        assert_eq!(display_col_to_byte("a\u{1F642}b", 2), 1);
    }

    #[test]
    fn boundary_sanitize_text_order() {
        // C1 (U+0090) 与 DEL 先被剥掉；接着两个相邻的自产 U+FFFD 被折叠成一个；
        // 结果不含 ESC，strip_ansi 完全不参与。
        let input = "a\u{0090}b\u{FFFD}\u{FFFD}c\u{7F}d";
        assert_eq!(sanitize_text(input), "ab\u{FFFD}cd");

        // 含 ESC 时才触发 strip_ansi；C0 清洗特意不动 ESC 本身，交给状态机整体处理。
        let with_ansi = "x\x1b[31my\x1b[0mz";
        assert_eq!(sanitize_text(with_ansi), "xyz");
    }

    #[test]
    fn truncate_to_width_borrows_when_not_exceeding() {
        let s = "hello";
        match truncate_to_width(s, 10, "…") {
            Cow::Borrowed(b) => assert_eq!(b, s),
            Cow::Owned(_) => panic!("must not allocate when not truncating"),
        }
    }

    #[test]
    fn strip_ansi_removes_csi_osc_dcs() {
        let s = "\x1b[31mred\x1b[0m \x1b]8;;http://x\x07link\x1b]8;;\x07 \x1bP+q436e\x1b\\end";
        assert_eq!(strip_ansi(s), "red link end");
    }

    #[test]
    fn expand_tabs_aligns_to_stops() {
        assert_eq!(expand_tabs_with_tab("a\tb", 4), "a   b");
        assert_eq!(expand_tabs_with_tab("ab\tc", 4), "ab  c");
    }

    #[test]
    fn tab_width_default_is_four() {
        assert_eq!(DEFAULT_TAB_WIDTH, 4);
    }

    #[test]
    fn wrap_to_width_breaks_on_width_and_existing_newline() {
        let lines = wrap_to_width("hello\nworld!!", 5);
        assert_eq!(lines, vec!["hello", "world", "!!"]);
    }

    #[test]
    fn wrap_to_width_zero_returns_empty_vec() {
        assert!(wrap_to_width("anything", 0).is_empty());
    }
}
