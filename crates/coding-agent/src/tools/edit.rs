//! `edit` 工具：对已存在文件做一次精确的字符串替换。
//!
//! # 多级 replacer 链
//!
//! 模型给出的 `old_string` 经常带着与磁盘文件的细微出入——行尾多一个空格、缩进整体错位、
//! 或者中间某一行在模型读取之后被别的调用改过。逐级放宽匹配严格度是标准做法（移植自
//! opencode `packages/opencode/src/tool/edit.ts:244-644` 的 9 级 replacer 链），但本文件
//! **不是**逐级照搬：读完那 9 级后发现其中 `IndentationFlexibleReplacer`
//! （`edit.ts:471-497`）在它自己的顺序里几乎是死代码——它的匹配条件（去掉公共缩进后逐行
//! *完全*相等，含行尾空白）严格蕴含 `LineTrimmedReplacer`（`edit.ts:248-286`，逐行两端
//! trim 后比较）的匹配条件，而 `LineTrimmedReplacer` 排在它前面且优先级更高，于是
//! `IndentationFlexibleReplacer` 能匹配到的窗口集合永远是前者的子集，正常路径下不可能被
//! 命中。继续照抄等于埋一段测不出问题、也没人会删的死代码。
//!
//! 本文件改用四级，**严格单调放宽**（第 N 级能匹配到的位置集合是第 N+1 级的子集），
//! 每级恰好新增一个维度的容忍度，不会互相吞掉：
//!
//! 1. [`exact_matches`] —— 字节精确匹配。
//! 2. [`trailing_whitespace_agnostic_matches`] —— 只忽略**行尾**空白（空格/制表符/`\r`），
//!    每行的前导缩进仍要求逐字节相等。对应"模型漏抄了一个行尾空格"这类最常见的抄写误差。
//! 3. [`indent_normalized_matches`] —— 在②的基础上再允许整个块有一个**统一**的公共缩进
//!    偏移（相对结构不变，只是绝对层级不同）。对应"模型复制了一段代码但漏带外层缩进"。
//! 4. [`block_anchor_matches`] —— 只要求首尾两行（trim 后）精确命中，中间行只需过半数
//!    trim 后相等。对应"块的边界很清楚，但中间某一行在模型读取之后被后续调用改过"。
//!
//! 没有移植 opencode 用 Levenshtein 连续相似度 + `0.65` 阈值做候选打分的
//! `BlockAnchorReplacer`（`edit.ts:220-221,329-407`）——那个阈值上游自己也没给出derivation，
//! 抄一个自己说不清道理的模糊匹配比匹配不上更危险（任务书原话）。④用的"过半数行匹配"是
//! 离散、可审计的多数表决，不是连续相似度打分。
//!
//! 出于同样的理由，也没有移植 `EscapeNormalizedReplacer`（转义序列反转义猜测）——它会在
//! `old_string` 恰好包含 `\n`/`\t` 等字面反斜杠转义时改写匹配语义，容易把"模型字符串里就是
//! 写了两个字符 `\` `n`"这种情况误判为"模型想表达一个换行"，而这两种意图在源码里是完全不同
//! 的字节序列。
//!
//! # 解析策略：命中即止，不做全链条搜索
//!
//! 每一级返回该级下**全部**候选位置；只要某一级有候选（不论一个还是多个），就在该级做出
//! 最终判定，不再往下试更宽松的级别。因为链条是严格单调放宽的，第 N 级的候选集合已经是
//! 第 N+1 级候选集合的子集——继续往下找不会漏掉真正唯一的匹配，只会把本该报"多处命中，
//! 请收紧上下文"的情况，用更宽松的判据悄悄猜出一个可能是错的位置。
//!
//! # 零变更防护
//!
//! `old_string == new_string` 直接拒绝，且**不是**"锚点没找对，再多给点上下文"这一类提示。
//! oh-my-pi 在这条防护补齐前吃过真实教训：同一次任务被同一个字节级相同的 no-op payload
//! 反复调用 182/205 次，直到用户手动打断（`edit/hashline/execute.ts:51-63`、
//! `noop-loop-guard.ts:39`，issue #2081）。模型看到"没找到"类措辞会倾向于扩大 payload 重试，
//! 而这次真正的问题恰恰是 payload 本身没有变化——所以错误文案必须直接点名"这次调用没有产生
//! 任何变化"，而不是让模型误读成匹配失败。

use std::fmt::Write as _;
use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use zcode_agent::{ApprovalDecision, Concurrency, Tier, Tool, ToolContext, ToolError, ToolOutput};

use crate::config::ToolsConfig;
use crate::tools::output;
use crate::tools::write::{symlink_note, write_atomic};
use crate::workspace::Workspace;

/// 上下文行数：diff 里变化区块上下各展示多少行未改动的邻近行。
///
/// 任务书 §edit 明确要求"上下各 3 行上下文"；这也是 `git diff -U3` 的默认值，
/// 不需要另找出处。
pub(crate) const DIFF_CONTEXT_LINES: usize = 3;

/// `edit` 的参数：定位一段确切文本并替换。
#[derive(Debug, Deserialize)]
struct EditArgs {
    /// 目标文件路径。
    path: String,
    /// 要替换的确切文本。
    old_string: String,
    /// 替换后的文本。
    new_string: String,
    /// `true` 时替换所有匹配处；默认只允许唯一匹配，否则报错。
    #[serde(default)]
    replace_all: bool,
}

/// 一处候选匹配的字节区间（半开区间 `[start, end)`，对原始 `content` 而言）。
#[derive(Debug, Clone)]
struct Match {
    start: usize,
    end: usize,
    /// 只有③级"缩进归一"命中会填充：`(search 的公共前导缩进, 命中块在文件里的真实公共
    /// 前导缩进)`。①②级要求缩进逐字节相等，不存在这个落差；④级锚点匹配的块结构本来就
    /// 不完全一致，不做二次缩进猜测。应用替换时把 `new_string` 每一行开头的前者替换成
    /// 后者——否则直接拿掉 `old_string` 那部分（含它在文件里的真实缩进）、换成 `new_string`
    /// （沿用的是 `old_string` 自己那个更浅的缩进基线），会把命中块的绝对缩进层级冲掉。
    reindent: Option<(String, String)>,
}

/// 定位阶段的结论。
enum LocateOutcome {
    /// 没有任何一级命中。
    NotFound,
    /// 某一级恰好命中一处。
    Unique(Match),
    /// 某一级命中多处（`!replace_all` 时是错误，`replace_all` 时是待替换集合）。
    Ambiguous(Vec<Match>),
}

/// 替换阶段的结论。
enum ReplaceOutcome {
    /// 四级 replacer 均未命中。
    NotFound,
    /// 命中多处但调用方没有传 `replace_all: true`。
    Ambiguous(Vec<Match>),
    /// 成功应用，`match_count` 是实际替换的处数（`replace_all` 下可能 > 1）。
    Applied {
        new_content: String,
        match_count: usize,
    },
}

/// `edit` 工具：定位一段确切文本并替换，支持渐进放宽的匹配与多处替换。
#[derive(Debug)]
pub(crate) struct EditTool {
    workspace: Arc<Workspace>,
}

impl EditTool {
    /// 装配期构造。`config` 目前没有 edit 专属选项，只是为了保持八个内置工具统一的
    /// 构造签名（`ToolsRegistry` 的既定约定）。
    pub(crate) fn new(workspace: Arc<Workspace>, _config: &ToolsConfig) -> Self {
        Self { workspace }
    }
}

#[async_trait]
impl Tool for EditTool {
    fn name(&self) -> &'static str {
        "edit"
    }

    fn description(&self) -> &'static str {
        include_str!("./prompts/edit.md")
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to edit (relative to the workspace root, or absolute)."
                },
                "old_string": {
                    "type": "string",
                    "description": "The exact text to replace. Must carry enough surrounding context (a full statement or block, not a bare token) to match exactly one location in the file — a short or ambiguous snippet that matches more than once is rejected."
                },
                "new_string": {
                    "type": "string",
                    "description": "The text to replace old_string with."
                },
                "replace_all": {
                    "type": "boolean",
                    "description": "Replace every occurrence of old_string instead of requiring a single unique match. Defaults to false."
                }
            },
            "required": ["path", "old_string", "new_string"],
            "additionalProperties": false
        })
    }

    fn approval(&self, _args: &Value) -> ApprovalDecision {
        ApprovalDecision::tier(Tier::Write)
    }

    fn concurrency(&self, _args: &Value) -> Concurrency {
        Concurrency::Exclusive
    }

    /// 单次 fs 读 + 内存替换 + tmp/rename，耗时在毫秒级，来不及也没必要响应软/硬取消。
    async fn execute(&self, args: Value, _ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        let args: EditArgs = serde_json::from_value(args)
            .map_err(|err| output::error(format!("参数解析失败：{err}")))?;

        if args.old_string == args.new_string {
            return Err(output::error(
                "old_string 与 new_string 完全相同——这次调用本身没有产生任何变化，不是「锚点没找对，\
再多给点上下文」的情况。反复用同一份 payload 重试解决不了问题：请重新核对 old_string 是否真的圈住了\
要改的那部分，或者确认 new_string 是不是漏改了。（真实教训：某次任务里同一份字节级相同的 no-op \
payload 被连续调用了 182/205 次才被用户手动打断。）",
            ));
        }
        if args.old_string.is_empty() {
            return Err(output::error(
                "old_string 不能为空——编辑已存在文件必须给出要替换的确切文本；如果是想整份重写这个\
文件，请改用 write 工具。",
            ));
        }

        let resolved = self
            .workspace
            .resolve(&args.path)
            .map_err(|err| output::error(format!("路径解析失败：{err}")))?;
        let display = self.workspace.display(&resolved.path);

        let content_bytes = tokio::fs::read(&resolved.path).await.map_err(|err| {
            if err.kind() == std::io::ErrorKind::NotFound {
                output::error(format!(
                    "{display} 不存在，无法编辑；先用 write 创建它，或确认路径是否写错。"
                ))
            } else {
                output::error(format!("读取 {display} 失败：{err}"))
            }
        })?;
        let content = String::from_utf8(content_bytes).map_err(|_| {
            output::error(format!("{display} 不是合法 UTF-8 文本，无法按文本编辑。"))
        })?;

        match apply_replace(
            &content,
            &args.old_string,
            &args.new_string,
            args.replace_all,
        ) {
            ReplaceOutcome::NotFound => {
                let hint = nearest_candidate(&content, &args.old_string).map_or_else(
                    || "文件里没有找到形态相近的内容。".to_owned(),
                    |(line_no, text)| format!("文件里最接近的一处在第 {line_no} 行：{text}"),
                );
                Err(output::error(format!(
                    "在 {display} 中找不到 old_string：已依次尝试精确匹配、行尾空白无关匹配、缩进归一\
匹配、块首尾锚点匹配，均未命中。{hint}\n建议：先用 read 重新读取该区域，直接复制真实文本作为 \
old_string，而不是凭记忆重写。",
                )))
            }
            ReplaceOutcome::Ambiguous(matches) => {
                let count = matches.len();
                let locations = matches
                    .iter()
                    .map(|m| describe_match(&content, m))
                    .collect::<Vec<_>>()
                    .join("；");
                Err(output::error(format!(
                    "old_string 在 {display} 中匹配到 {count} 处：{locations}。未传 replace_all: true \
时只允许唯一匹配。请把 old_string 的上下文扩大到能唯一定位这一处（带上更多相邻行），或者显式传 \
replace_all: true 一次性替换全部 {count} 处。",
                )))
            }
            ReplaceOutcome::Applied {
                new_content,
                match_count,
            } => {
                let resolution = crate::tools::write::resolve_symlink_target(&resolved.path).await;
                write_atomic(&resolution.path, new_content.as_bytes())
                    .await
                    .map_err(|err| {
                        output::error(format!(
                            "写入 {} 失败：{err}",
                            self.workspace.display(&resolution.path)
                        ))
                    })?;

                let mut body = String::new();
                if let Some(note) = symlink_note(&display, &resolution, &self.workspace) {
                    let _ = writeln!(body, "{note}\n");
                }
                let _ = writeln!(body, "已在 {display} 完成 {match_count} 处替换。");
                let diff = compact_diff(&content, &new_content, DIFF_CONTEXT_LINES);
                if !diff.is_empty() {
                    body.push('\n');
                    body.push_str(&diff);
                }
                Ok(output::finish(body, format!("edit {display}")))
            }
        }
    }
}

/// 依次尝试四级 replacer，返回第一个产出候选的级别的判定。
fn locate(content: &str, old: &str) -> LocateOutcome {
    let levels: [fn(&str, &str) -> Vec<Match>; 4] = [
        exact_matches,
        trailing_whitespace_agnostic_matches,
        indent_normalized_matches,
        block_anchor_matches,
    ];
    for level in levels {
        let mut matches = level(content, old);
        match matches.len() {
            0 => {}
            1 => {
                if let Some(m) = matches.pop() {
                    return LocateOutcome::Unique(m);
                }
            }
            _ => return LocateOutcome::Ambiguous(matches),
        }
    }
    LocateOutcome::NotFound
}

/// 定位并应用替换。`replace_all` 时把命中集合按起始位置从后往前逐个替换，
/// 避免早替换的位置偏移让后面的字节区间失效。
fn apply_replace(content: &str, old: &str, new: &str, replace_all: bool) -> ReplaceOutcome {
    let matches = match locate(content, old) {
        LocateOutcome::NotFound => return ReplaceOutcome::NotFound,
        LocateOutcome::Unique(m) => vec![m],
        LocateOutcome::Ambiguous(ms) => {
            if !replace_all {
                return ReplaceOutcome::Ambiguous(ms);
            }
            ms
        }
    };

    let mut sorted = matches;
    sorted.sort_by_key(|m| m.start);
    let mut result = content.to_owned();
    for m in sorted.iter().rev() {
        let replacement = match &m.reindent {
            Some((from, to)) => reindent_text(new, from, to),
            None => new.to_owned(),
        };
        let mut next = String::with_capacity(result.len());
        if let Some(pre) = result.get(..m.start) {
            next.push_str(pre);
        }
        next.push_str(&replacement);
        if let Some(post) = result.get(m.end..) {
            next.push_str(post);
        }
        result = next;
    }
    ReplaceOutcome::Applied {
        new_content: result,
        match_count: sorted.len(),
    }
}

/// 把 `text` 每一行开头字面匹配 `from` 的前导缩进换成 `to`；不以 `from` 开头的行
/// （通常是空行，或缩进比 `from` 还浅的行）原样保留，不强行凑。
fn reindent_text(text: &str, from: &str, to: &str) -> String {
    if from == to {
        return text.to_owned();
    }
    text.split('\n')
        .map(|line| match line.strip_prefix(from) {
            Some(rest) => format!("{to}{rest}"),
            None => line.to_owned(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 第①级：字节精确子串匹配。
fn exact_matches(content: &str, old: &str) -> Vec<Match> {
    if old.is_empty() {
        return Vec::new();
    }
    content
        .match_indices(old)
        .map(|(start, matched)| Match {
            start,
            end: start + matched.len(),
            reindent: None,
        })
        .collect()
}

/// 把内容按 `\n` 切成若干行的字节区间（不含 `\n` 本身），语义与 `content.split('\n')`
/// 一一对应，只是额外保留每行在原字符串里的字节偏移，供构造 [`Match`] 用。
fn line_spans(content: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut start = 0usize;
    for (idx, ch) in content.char_indices() {
        if ch == '\n' {
            spans.push((start, idx));
            start = idx + 1;
        }
    }
    spans.push((start, content.len()));
    spans
}

/// `old_string` 按行切分；模型给出的多行文本经常带一个尾随的空行（末尾多打了个换行），
/// 那不是内容的一部分，丢弃它以免行数对不上。
fn search_lines(old: &str) -> Vec<&str> {
    let mut lines: Vec<&str> = old.split('\n').collect();
    if lines.last() == Some(&"") {
        lines.pop();
    }
    lines
}

/// 第②级：忽略每行**行尾**空白（空格/制表符/`\r`），前导缩进仍要求逐字节相等。
fn trailing_whitespace_agnostic_matches(content: &str, old: &str) -> Vec<Match> {
    let spans = line_spans(content);
    let search = search_lines(old);
    let n = search.len();
    if n == 0 || spans.len() < n {
        return Vec::new();
    }
    let mut out = Vec::new();
    for i in 0..=(spans.len() - n) {
        let all_match = (0..n).all(|j| {
            let Some(&(s, e)) = spans.get(i + j) else {
                return false;
            };
            let Some(line) = content.get(s..e) else {
                return false;
            };
            let Some(&want) = search.get(j) else {
                return false;
            };
            line.trim_end() == want.trim_end()
        });
        if all_match
            && let (Some(&(start, _)), Some(&(_, end))) = (spans.get(i), spans.get(i + n - 1))
        {
            out.push(Match {
                start,
                end,
                reindent: None,
            });
        }
    }
    out
}

/// 一个多行块所有非空行共有的最小前导空白字节数。
fn min_indent_len(lines: &[&str]) -> usize {
    lines
        .iter()
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.len() - line.trim_start().len())
        .min()
        .unwrap_or(0)
}

/// [`min_indent_len`] 对应的字面前导空白子串，供 [`Match::reindent`] 拼接使用。
fn common_indent_prefix(lines: &[&str]) -> String {
    let min_indent = min_indent_len(lines);
    lines
        .iter()
        .find(|line| !line.trim().is_empty())
        .and_then(|line| line.get(..min_indent))
        .unwrap_or("")
        .to_owned()
}

/// 去掉一个多行块里所有非空行共有的最小前导空白，保留相对缩进结构；空行原样保留。
fn strip_common_indent(lines: &[&str]) -> Vec<String> {
    let min_indent = min_indent_len(lines);
    lines
        .iter()
        .map(|line| {
            if line.trim().is_empty() {
                (*line).to_owned()
            } else {
                line.get(min_indent.min(line.len())..)
                    .unwrap_or(line)
                    .to_owned()
            }
        })
        .collect()
}

/// 第③级：在②的基础上再容忍整块统一的缩进偏移——相对结构必须一致，只是绝对层级不同。
fn indent_normalized_matches(content: &str, old: &str) -> Vec<Match> {
    let spans = line_spans(content);
    let search = search_lines(old);
    let n = search.len();
    if n == 0 || spans.len() < n {
        return Vec::new();
    }
    let search_prefix = common_indent_prefix(&search);
    let normalized_search = strip_common_indent(&search);
    let mut out = Vec::new();
    for i in 0..=(spans.len() - n) {
        let block: Option<Vec<&str>> = (0..n)
            .map(|j| spans.get(i + j).and_then(|&(s, e)| content.get(s..e)))
            .collect();
        let Some(block) = block else { continue };
        let normalized_block = strip_common_indent(&block);
        let equal = normalized_block.len() == normalized_search.len()
            && normalized_block
                .iter()
                .zip(normalized_search.iter())
                .all(|(a, b)| a.trim_end() == b.trim_end());
        if equal && let (Some(&(start, _)), Some(&(_, end))) = (spans.get(i), spans.get(i + n - 1))
        {
            let block_prefix = common_indent_prefix(&block);
            let reindent = if block_prefix == search_prefix {
                None
            } else {
                Some((search_prefix.clone(), block_prefix))
            };
            out.push(Match {
                start,
                end,
                reindent,
            });
        }
    }
    out
}

/// 第④级：只要求首尾两行（trim 后）精确命中，中间行至少过半数 trim 后相等。
/// 少于 3 行时没有"首尾之间"的中段，锚点没有意义，直接不产出候选。
fn block_anchor_matches(content: &str, old: &str) -> Vec<Match> {
    let spans = line_spans(content);
    let search = search_lines(old);
    let n = search.len();
    if n < 3 || spans.len() < n {
        return Vec::new();
    }
    let first_want = search.first().map(|s| s.trim()).unwrap_or_default();
    let last_want = search.last().map(|s| s.trim()).unwrap_or_default();

    let mut out = Vec::new();
    for i in 0..=(spans.len() - n) {
        let Some(&(s0, e0)) = spans.get(i) else {
            continue;
        };
        let Some(first_line) = content.get(s0..e0) else {
            continue;
        };
        if first_line.trim() != first_want {
            continue;
        }
        let Some(&(sl, el)) = spans.get(i + n - 1) else {
            continue;
        };
        let Some(last_line) = content.get(sl..el) else {
            continue;
        };
        if last_line.trim() != last_want {
            continue;
        }

        let mut matching = 0usize;
        let mut total = 0usize;
        for j in 1..n.saturating_sub(1) {
            let Some(&(bs, be)) = spans.get(i + j) else {
                continue;
            };
            let Some(block_line) = content.get(bs..be) else {
                continue;
            };
            let Some(&want) = search.get(j) else { continue };
            let (got, want) = (block_line.trim(), want.trim());
            if got.is_empty() && want.is_empty() {
                continue;
            }
            total += 1;
            if got == want {
                matching += 1;
            }
        }
        if total == 0 || matching * 2 >= total {
            out.push(Match {
                start: s0,
                end: el,
                reindent: None,
            });
        }
    }
    out
}

/// 定位到原始内容里某个字节区间对应的起止行号，供错误消息展示。
fn describe_match(content: &str, m: &Match) -> String {
    let start_line = line_number_at(content, m.start);
    let end_line = if m.end > m.start {
        line_number_at(content, m.end.saturating_sub(1))
    } else {
        start_line
    };
    if end_line > start_line {
        format!("第 {start_line}-{end_line} 行")
    } else {
        format!("第 {start_line} 行")
    }
}

/// 字节偏移之前有多少个 `\n`，加一即为 1-based 行号；偏移不落在字符边界上时退化返回 1
/// （只影响提示文案的精确度，不影响替换本身的正确性）。
fn line_number_at(content: &str, byte_offset: usize) -> usize {
    content
        .get(..byte_offset)
        .map_or(1, |s| s.matches('\n').count() + 1)
}

/// 找不到任何匹配时，给出文件里"长得最像" `old_string` 第一行非空内容的那一行，
/// 帮模型判断是记错了内容还是记错了文件。纯诊断用途，不参与任何实际替换决策，
/// 所以这里可以用连续的 Levenshtein 距离——它给出的是"最接近"的排序提示，不是拿来直接
/// 应用的匹配结果，错了也只是提示不够精准，不会误改文件。
fn nearest_candidate(content: &str, old: &str) -> Option<(usize, String)> {
    let probe = old.lines().find(|l| !l.trim().is_empty())?.trim();
    if probe.is_empty() {
        return None;
    }
    let mut best: Option<(usize, usize, &str)> = None;
    for (idx, line) in content.lines().enumerate() {
        let dist = levenshtein(probe, line.trim());
        let better = best.is_none_or(|(_, best_dist, _)| dist < best_dist);
        if better {
            best = Some((idx + 1, dist, line));
        }
    }
    best.map(|(line_no, _, text)| (line_no, text.to_owned()))
}

/// 标准 Levenshtein 编辑距离，滚动双行数组实现（`O(len(a))` 空间）。
fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr = vec![0usize; b.len() + 1];
    for (i, &ca) in a.iter().enumerate() {
        if let Some(first) = curr.get_mut(0) {
            *first = i + 1;
        }
        for (j, &cb) in b.iter().enumerate() {
            let del = prev.get(j + 1).copied().unwrap_or(0) + 1;
            let ins = curr.get(j).copied().unwrap_or(0) + 1;
            let sub = prev.get(j).copied().unwrap_or(0) + usize::from(ca != cb);
            let best = del.min(ins).min(sub);
            if let Some(slot) = curr.get_mut(j + 1) {
                *slot = best;
            }
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev.last().copied().unwrap_or(0)
}

/// 生成 `old` → `new` 的紧凑行级 diff：跳过公共前后缀，只展示发生变化的中段，
/// 外加两侧各 `context` 行未改动的邻近行。
///
/// 用的是公共前缀/后缀裁剪，**不是**全文件最长公共子序列。交互式编辑工具的真实输入
/// 绝大多数只改动一处连续区块，前后缀裁剪已经覆盖这个场景；真遇到文件里零散多处都有
/// 改动，会把整个跨度当成一次性替换展示——代价是那种输入下 diff 不够精细，换来的是一个
/// 确定性、无阈值可调、不会给出"看着像对但其实对错了行"的算法。`write`（整份覆写）与
/// `edit`（定点替换）共用同一份实现，保证两个工具的成功输出形态一致。
pub(crate) fn compact_diff(old: &str, new: &str, context: usize) -> String {
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();

    let prefix = old_lines
        .iter()
        .zip(new_lines.iter())
        .take_while(|(a, b)| a == b)
        .count();

    let max_suffix = old_lines
        .len()
        .saturating_sub(prefix)
        .min(new_lines.len().saturating_sub(prefix));
    let suffix = old_lines
        .iter()
        .rev()
        .zip(new_lines.iter().rev())
        .take(max_suffix)
        .take_while(|(a, b)| a == b)
        .count();

    let old_mid_start = prefix;
    let old_mid_end = old_lines.len().saturating_sub(suffix);
    let new_mid_start = prefix;
    let new_mid_end = new_lines.len().saturating_sub(suffix);

    if old_mid_start >= old_mid_end && new_mid_start >= new_mid_end {
        return String::new();
    }

    let mut out = String::new();

    let before: Vec<&str> = old_lines
        .iter()
        .copied()
        .take(prefix)
        .skip(prefix.saturating_sub(context))
        .collect();
    let before_start = prefix.saturating_sub(before.len());
    for (offset, line) in before.iter().enumerate() {
        let _ = writeln!(out, "{:>6}   {line}", before_start + offset + 1);
    }
    for (offset, line) in old_lines
        .iter()
        .copied()
        .skip(old_mid_start)
        .take(old_mid_end.saturating_sub(old_mid_start))
        .enumerate()
    {
        let _ = writeln!(out, "{:>6} - {line}", old_mid_start + offset + 1);
    }
    for (offset, line) in new_lines
        .iter()
        .copied()
        .skip(new_mid_start)
        .take(new_mid_end.saturating_sub(new_mid_start))
        .enumerate()
    {
        let _ = writeln!(out, "{:>6} + {line}", new_mid_start + offset + 1);
    }
    let after: Vec<&str> = new_lines
        .iter()
        .copied()
        .skip(new_mid_end)
        .take(context)
        .collect();
    for (offset, line) in after.iter().enumerate() {
        let _ = writeln!(out, "{:>6}   {line}", new_mid_end + offset + 1);
    }

    out.trim_end_matches('\n').to_owned()
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;
    use tokio::sync::mpsc;
    use zcode_agent::{EntryId, InterruptSignal, SessionId};

    use super::*;
    use crate::config::ToolsConfig;

    fn ctx() -> ToolContext {
        let (progress, _rx) = mpsc::unbounded_channel();
        ToolContext {
            session_id: SessionId::generate(),
            entry_id: EntryId::generate(),
            call_id: "call_1".to_owned(),
            cwd: std::env::temp_dir(),
            cancel: InterruptSignal::new(),
            steering: InterruptSignal::new(),
            progress,
        }
    }

    fn tools_config() -> ToolsConfig {
        ToolsConfig {
            disabled: Vec::new(),
            bash_timeout_secs: 120,
            read_max_lines: 2000,
        }
    }

    fn tool_in(dir: &std::path::Path) -> EditTool {
        let workspace = Arc::new(Workspace::new(dir.to_path_buf()));
        EditTool::new(workspace, &tools_config())
    }

    #[tokio::test]
    async fn exact_match_replaces_unique_occurrence() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("a.txt");
        tokio::fs::write(&path, "fn foo() {\n    1\n}\n")
            .await
            .expect("write fixture");
        let tool = tool_in(dir.path());

        let args = json!({ "path": "a.txt", "old_string": "    1\n", "new_string": "    2\n" });
        let result = tool
            .execute(args, ctx())
            .await
            .expect("edit should succeed");
        let text = result_text(&result);
        assert!(text.contains('1'), "标题应带替换处数");

        let on_disk = tokio::fs::read_to_string(&path).await.expect("read back");
        assert_eq!(on_disk, "fn foo() {\n    2\n}\n");
    }

    #[tokio::test]
    async fn line_trimmed_level_tolerates_trailing_whitespace_only() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("a.txt");
        // 内容每行都带一个多余的行尾空格；old_string 没有——精确匹配必然失败。
        tokio::fs::write(&path, "foo();  \nbar();  \n")
            .await
            .expect("write fixture");
        let tool = tool_in(dir.path());

        let args =
            json!({ "path": "a.txt", "old_string": "foo();\nbar();", "new_string": "baz();" });
        tool.execute(args, ctx())
            .await
            .expect("行尾空白无关匹配应命中");

        let on_disk = tokio::fs::read_to_string(&path).await.expect("read back");
        assert_eq!(on_disk, "baz();\n");
    }

    #[tokio::test]
    async fn indent_normalized_level_tolerates_uniform_offset() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("a.py");
        // 内容比 old_string 整体多缩进 4 格（嵌在 class 里），相对结构一致。
        // 行尾没有多余空白，所以②（只忽略行尾）在前导缩进不等时必然失败，
        // 只有③（缩进归一）能匹配到。
        tokio::fs::write(&path, "class Foo:\n    def bar(self):\n        return 1\n")
            .await
            .expect("write fixture");
        let tool = tool_in(dir.path());

        let args = json!({
            "path": "a.py",
            "old_string": "def bar(self):\n    return 1",
            "new_string": "def bar(self):\n    return 2",
        });
        tool.execute(args, ctx()).await.expect("缩进归一匹配应命中");

        let on_disk = tokio::fs::read_to_string(&path).await.expect("read back");
        assert_eq!(
            on_disk,
            "class Foo:\n    def bar(self):\n        return 2\n"
        );
    }

    #[tokio::test]
    async fn block_anchor_level_tolerates_a_drifted_middle_line() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("a.js");
        tokio::fs::write(
            &path,
            "function calc() {\n    // step one\n    const a = compute();\n    return a;\n}\n",
        )
        .await
        .expect("write fixture");
        let tool = tool_in(dir.path());

        // 中间的注释行在 old_string 里是"过时"的措辞——精确/行尾/缩进三级都会因这一行
        // 不相等而整体失败；只有首尾锚点 + 过半数中段匹配的第④级能命中。
        let args = json!({
            "path": "a.js",
            "old_string": "function calc() {\n    // outdated comment\n    const a = compute();\n    return a;\n}",
            "new_string": "function calc() {\n    // refreshed\n    const a = compute();\n    return a;\n}",
        });
        tool.execute(args, ctx())
            .await
            .expect("块首尾锚点匹配应命中");

        let on_disk = tokio::fs::read_to_string(&path).await.expect("read back");
        assert!(on_disk.contains("// refreshed"));
    }

    #[tokio::test]
    async fn multiple_matches_without_replace_all_reports_every_line() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("a.txt");
        tokio::fs::write(&path, "foo\nfoo\n")
            .await
            .expect("write fixture");
        let tool = tool_in(dir.path());

        let args = json!({ "path": "a.txt", "old_string": "foo", "new_string": "bar" });
        let err = tool
            .execute(args, ctx())
            .await
            .expect_err("多处命中且未 replace_all 必须报错");
        let message = err.to_string();
        assert!(message.contains("2 处"), "消息应说明命中处数：{message}");
        assert!(message.contains("第 1 行"), "消息应带第一处行号：{message}");
        assert!(message.contains("第 2 行"), "消息应带第二处行号：{message}");
    }

    #[tokio::test]
    async fn replace_all_rewrites_every_occurrence() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("a.txt");
        tokio::fs::write(&path, "foo\nfoo\nfoo\n")
            .await
            .expect("write fixture");
        let tool = tool_in(dir.path());

        let args = json!({ "path": "a.txt", "old_string": "foo", "new_string": "bar", "replace_all": true });
        tool.execute(args, ctx()).await.expect("replace_all 应成功");

        let on_disk = tokio::fs::read_to_string(&path).await.expect("read back");
        assert_eq!(on_disk, "bar\nbar\nbar\n");
    }

    #[tokio::test]
    async fn not_found_reports_nearest_candidate() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("a.txt");
        tokio::fs::write(&path, "const total = compute();\n")
            .await
            .expect("write fixture");
        let tool = tool_in(dir.path());

        let args = json!({ "path": "a.txt", "old_string": "const totall = compute();", "new_string": "x" });
        let err = tool
            .execute(args, ctx())
            .await
            .expect_err("找不到匹配必须报错");
        let message = err.to_string();
        assert!(
            message.contains("最接近"),
            "应给出最接近候选提示：{message}"
        );
        assert!(
            message.contains("compute();"),
            "候选提示应带上那一行的原文：{message}"
        );
    }

    #[tokio::test]
    async fn identical_old_and_new_is_rejected_as_noop() {
        let dir = tempdir().expect("tempdir");
        let tool = tool_in(dir.path());

        let args = json!({ "path": "does-not-matter.txt", "old_string": "x", "new_string": "x" });
        let err = tool
            .execute(args, ctx())
            .await
            .expect_err("old==new 必须直接拒绝");
        assert!(
            err.to_string().contains("没有产生任何变化"),
            "文案必须点名这是无变化而非没找到"
        );
    }

    #[test]
    fn compact_diff_only_shows_changed_span_with_context() {
        let old = "a\nb\nc\nd\ne\nf\ng\n";
        let new = "a\nb\nc\nX\ne\nf\ng\n";
        let diff = compact_diff(old, new, 3);
        assert!(diff.contains("- d"));
        assert!(diff.contains("+ X"));
        assert!(diff.contains('a'), "应带前置上下文");
        assert!(diff.contains('g'), "应带后置上下文");
    }

    fn result_text(output: &ToolOutput) -> String {
        output
            .content
            .iter()
            .map(|block| match block {
                zcode_agent::StoredToolResultContent::Text { text } => text.clone(),
                zcode_agent::StoredToolResultContent::Image { .. } => String::new(),
            })
            .collect()
    }
}
