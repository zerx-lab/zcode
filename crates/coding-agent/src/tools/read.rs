//! `read` 工具：读文件、读目录（浅层摘要）、读图片，`path` 参数里内嵌的冒号选择器负责分页
//! 与显示模式。
//!
//! # 为什么 schema 只有一个 `path` 字段
//!
//! 分页信息编进路径的选择器后缀（`path:N-M`），而不是额外的 `offset`/`limit` 参数：模型只需
//! 填一个字段，且工具的续读提示能直接给出下一步调用的**字面量**（`用 :123 继续`），模型复制
//! 粘贴即可用，不用自己拼两个数字。代价是语法要自己解析，且 `grep`（按同一选择器过滤命中行）
//! 必须跟这里保持一致——[`parse_selector`] 与 [`split_path_and_selector`] 因此是 `pub(crate)`，
//! 供 `crate::tools::grep`/`crate::tools::write`/`crate::tools::edit` 复用，全仓只此一份解析器。
//! 移植自 oh-my-pi `packages/coding-agent/src/tools/path-utils.ts:204-330`（`LineRange` /
//! `parseLineRanges` / `splitPathAndSel`）与 `packages/coding-agent/src/tools/read.ts:773-822`
//! （`parseSel` 的复合选择器判定）。
//!
//! # 字面文件名压过选择器
//!
//! 磁盘上真实存在名为 `test:1-2` 的文件时，字面路径解释胜出，选择器解释让路
//! （oh-my-pi issue #4618）。[`split_path_and_selector`] 只做语法拆分，不摸文件系统；
//! "字面路径是否存在"的探测在 [`ReadTool::execute`] 里用
//! `tokio::fs::symlink_metadata`（lstat 语义，悬空符号链接也算"存在"）完成。
//!
//! # 二进制文件判定
//!
//! 用"前 4096 字节是否含 `\0`"作为文本/二进制的判定依据，这是 git/ripgrep/GNU grep -I 共用的
//! 行业惯例，不是本仓发明；极少数不含早期 `\0` 字节的小型二进制（例如某些尺寸凑巧的 GIF）
//! 会被误判为文本，随后在按行解码时因非法 UTF-8 报错退出——错误信息仍然可读，只是没有走到
//! 更友好的"这是一张图片"分支，本仓认为这个边界情形的正确性代价可以接受。

use std::fmt::Write as _;
use std::path::Path;
use std::sync::{Arc, LazyLock};

use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use regex::Regex;
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;
use zcode_agent::{
    ApprovalDecision, Concurrency, StoredImage, Tier, Tool, ToolContext, ToolError, ToolOutput,
};
use zcode_text::{ResizeOptions, TruncateLimits, probe_dimensions, process_image};

use crate::config::ToolsConfig;
use crate::tools::{ls, output};
use crate::workspace::{PathError, Workspace};

/// 显式区间起点之前补的上下文行数。
///
/// 前提：只有"显式给了起点"时才补（比如 `:50-100`），补 1 行是为了让模型看到目标区间
/// 紧邻的上一行，方便判断锚点/缩进层级，不需要更多。
/// 移植自 oh-my-pi `packages/coding-agent/src/tools/read.ts:420`。
const RANGE_LEADING_CONTEXT_LINES: usize = 1;

/// 显式区间终点之后补的上下文行数。
///
/// 前提：补 3 行是为了让模型看到目标区间之后的收尾（比如一个函数体结束后的空行与下一个
/// 声明），3 是 oh-my-pi 实测的经验值，上游同样没有给出更细的依据。
/// 移植自 oh-my-pi `packages/coding-agent/src/tools/read.ts:421`。
const RANGE_TRAILING_CONTEXT_LINES: usize = 3;

/// 按"待收集行数"估算读取字节预算时假设的平均单行字节数。
///
/// 前提：不缩放字节预算的话，配置了 3000 行的读取请求会在默认 50 000 字节处被硬顶砍断，
/// 配置形同虚设。512 字节/行是 oh-my-pi 的经验假设，上游没有给出更细的统计依据。
/// 移植自 oh-my-pi `packages/coding-agent/src/tools/read.ts:2699-2701`。
const BYTES_PER_LINE_ESTIMATE: usize = 512;

/// 探测文本/二进制时读取的样本字节数。
///
/// 移植自 opencode `packages/opencode/src/tool/read.ts:18`（`SAMPLE_BYTES`）。
const SNIFF_SAMPLE_BYTES: usize = 4096;

/// read 目录分支的浅层摘要参数：最大深度。
///
/// 移植自 oh-my-pi `packages/coding-agent/src/tools/read.ts:3397`
/// （`READ_DIRECTORY_MAX_DEPTH`）。完整目录树请改用 `ls`。
const READ_DIR_MAX_DEPTH: usize = 2;

/// read 目录分支的浅层摘要参数：每目录最多展开的子项数。
///
/// 移植自 oh-my-pi `packages/coding-agent/src/tools/read.ts:3398`
/// （`READ_DIRECTORY_CHILD_LIMIT`）。
const READ_DIR_PER_DIR_LIMIT: usize = 12;

/// 单段选择器的整体形状：`N`、`N-M`、`N+K`、`N-`（开放到 EOF）。
const RANGE_CHUNK_SHAPE: &str = r"\d+(?:[-+]\d+|-)?";

/// 编译一个硬编码的正则字面量。调用方保证输入永远是编译期已知合法的正则；失败即实现自身的
/// 缺陷（内部不变量），这是本文件唯一集中的 `expect` 落点，避免在每个 `LazyLock` 初始化闭包
/// 里重复放行同一个 lint。
#[allow(clippy::expect_used)]
fn compile_regex(pattern: &str) -> Regex {
    Regex::new(pattern).expect("硬编码正则字面量语法已知合法")
}

/// 选择器整体（单段或逗号多段，或字面 `raw`）的形状校验；只判语法形状，不做数值语义校验
/// （语义校验在 [`parse_line_range_chunk`]）。
static SELECTOR_SHAPE_RE: LazyLock<Regex> = LazyLock::new(|| {
    compile_regex(&format!(
        r"(?i)^(?:{RANGE_CHUNK_SHAPE}(?:,{RANGE_CHUNK_SHAPE})*|raw)$"
    ))
});

/// 纯行区间列表（不含 `raw`）的形状校验，用于复合选择器 `raw:N-M` 的成分判定。
static RANGE_LIST_SHAPE_RE: LazyLock<Regex> = LazyLock::new(|| {
    compile_regex(&format!(
        r"(?i)^{RANGE_CHUNK_SHAPE}(?:,{RANGE_CHUNK_SHAPE})*$"
    ))
});

/// 字面 `raw`（大小写不敏感）的形状校验。
static RAW_ONLY_RE: LazyLock<Regex> = LazyLock::new(|| compile_regex(r"(?i)^raw$"));

/// 形似"负数起点"选择器（如 `-100`）的手误形状：本身不合法，但要被识别为"选择器尝试"从而
/// 触发报错，而不是被静默当成字面路径的一部分。
static NEGATIVE_CHUNK_RE: LazyLock<Regex> = LazyLock::new(|| compile_regex(r"^-\d+(?:[-+]\d+)?$"));

/// 单段行区间的捕获式解析：`(起始行)(?:(分隔符)(结束数字)?)?`。
static LINE_RANGE_CHUNK_RE: LazyLock<Regex> =
    LazyLock::new(|| compile_regex(r"^(\d+)(?:([-+])(\d+)?)?$"));

/// 1-indexed、含两端的行区间；`end = None` 表示开放到文件末尾。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LineRange {
    /// 起始行号（含）。
    pub(crate) start: usize,
    /// 结束行号（含）；`None` 表示开放到 EOF。
    pub(crate) end: Option<usize>,
}

/// 解析后的选择器语义。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Selector {
    /// 无选择器后缀。
    None,
    /// `:raw`，逐字节输出，不加行号前缀、不补上下文。
    Raw,
    /// 一个或多个行区间（已按起点升序排序、重叠/相邻区间已合并）；`raw` 叠加 `:raw`。
    Lines {
        /// 请求的区间列表。
        ranges: Vec<LineRange>,
        /// 是否叠加 `raw`（逐字节输出，不补上下文）。
        raw: bool,
    },
}

/// 选择器语法错误。文本面向模型，可直接喂进 [`ToolError::Failed`]。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub(crate) struct SelectorError(pub(crate) String);

/// 从形如 `path:sel` 的原始参数里剥离选择器后缀（纯字符串语法层面，不摸文件系统）。
///
/// 只有当冒号之后的内容整体符合选择器形状（`raw`、行区间、或"行区间+raw"复合）时才会剥离；
/// 否则原样返回整段 `raw`（`sel = None`），这自然保护了 Windows 盘符路径
/// （`C:\foo\bar.txt` 的最后一个冒号候选是整段剩余路径，不符合选择器形状）。
/// 调用方在采用剥离结果前必须先确认字面全路径在磁盘上不存在
/// （见模块文档"字面文件名压过选择器"）——本函数不做这个判断。
#[must_use]
pub(crate) fn split_path_and_selector(raw: &str) -> (&str, Option<&str>) {
    let colon = match raw.rfind(':') {
        Some(0) | None => return (raw, None),
        Some(index) => index,
    };
    let Some(candidate) = raw.get(colon + 1..) else {
        return (raw, None);
    };
    if candidate.is_empty() || !SELECTOR_SHAPE_RE.is_match(candidate) {
        return (raw, None);
    }
    let Some(base_path) = raw.get(..colon) else {
        return (raw, None);
    };

    // 复合选择器：`path:raw:1-50` 或 `path:1-50:raw`。两段必须恰好是一个行区间加一个字面
    // `raw`（顺序任意），否则不合并，只剥离最外层这一段。
    if let Some(inner_colon) = base_path.rfind(':')
        && inner_colon > 0
        && let Some(inner_candidate) = base_path.get(inner_colon + 1..)
    {
        let inner_is_raw = RAW_ONLY_RE.is_match(inner_candidate);
        let outer_is_raw = RAW_ONLY_RE.is_match(candidate);
        let inner_is_range = RANGE_LIST_SHAPE_RE.is_match(inner_candidate);
        let outer_is_range = RANGE_LIST_SHAPE_RE.is_match(candidate);
        let is_compound = (inner_is_raw && outer_is_range) || (inner_is_range && outer_is_raw);
        if is_compound
            && let (Some(sel), Some(merged_base)) =
                (raw.get(inner_colon + 1..), raw.get(..inner_colon))
        {
            return (merged_base, Some(sel));
        }
    }

    (base_path, Some(candidate))
}

/// 解析选择器原文。空字符串返回 [`Selector::None`]。
///
/// 复合选择器（含冒号）只接受"一个行区间 + 字面 `raw`"这一种组合；其余每个 chunk 都"形似
/// 选择器"却不在接受集合内时报错（而非静默退化为字面路径）——这样 `:raw:raw`、
/// `:5-10:20-30` 这类拼写错误会被明确指出，而不是悄悄丢弃后半段。
/// 移植自 oh-my-pi `packages/coding-agent/src/tools/read.ts:786-822`（`parseSel`）。
pub(crate) fn parse_selector(sel: &str) -> Result<Selector, SelectorError> {
    if sel.is_empty() {
        return Ok(Selector::None);
    }

    if sel.contains(':') {
        let chunks: Vec<&str> = sel.split(':').collect();
        if chunks.len() == 2 {
            let a = chunks.first().copied().unwrap_or_default();
            let b = chunks.get(1).copied().unwrap_or_default();
            let a_is_raw = RAW_ONLY_RE.is_match(a);
            let b_is_raw = RAW_ONLY_RE.is_match(b);
            let range_chunk = if a_is_raw {
                Some(b)
            } else if b_is_raw {
                Some(a)
            } else {
                None
            };
            if let Some(range_chunk) = range_chunk
                && RANGE_LIST_SHAPE_RE.is_match(range_chunk)
            {
                let ranges = parse_line_ranges(range_chunk)?;
                return Ok(Selector::Lines { ranges, raw: true });
            }
        }
        if chunks.iter().all(|chunk| selector_chunk_looks_like(chunk)) {
            return Err(invalid_selector(sel));
        }
        // 本仓不支持 sqlite/archive/url 各自的冒号语法；识别不出的复合形式当字面路径处理。
        return Ok(Selector::None);
    }

    if RAW_ONLY_RE.is_match(sel) {
        return Ok(Selector::Raw);
    }
    if RANGE_LIST_SHAPE_RE.is_match(sel) {
        let ranges = parse_line_ranges(sel)?;
        return Ok(Selector::Lines { ranges, raw: false });
    }
    Ok(Selector::None)
}

/// 判断一个复合选择器的 chunk 是否"形似一次选择器尝试"（用于决定报错还是当字面路径）。
fn selector_chunk_looks_like(chunk: &str) -> bool {
    RAW_ONLY_RE.is_match(chunk)
        || NEGATIVE_CHUNK_RE.is_match(chunk)
        || RANGE_LIST_SHAPE_RE.is_match(chunk)
}

fn invalid_selector(sel: &str) -> SelectorError {
    SelectorError(format!(
        "选择器 ':{sel}' 无效。可用形式：:N、:N-M、:N+K、:N-（开放到文件末尾）、\
         逗号分隔的多段（如 :5-16,960-973）、:raw，或行区间叠加 raw（如 :raw:50-100 / :50-100:raw）。"
    ))
}

/// 解析逗号分隔的多段行区间：按起点升序排序，重叠/相邻区间合并。
fn parse_line_ranges(sel: &str) -> Result<Vec<LineRange>, SelectorError> {
    let mut ranges = sel
        .split(',')
        .map(parse_line_range_chunk)
        .collect::<Result<Vec<_>, _>>()?;
    ranges.sort_by_key(|range| range.start);
    Ok(merge_ranges(ranges))
}

/// 合并已按起点排序的区间列表：起点落在上一段（含相邻的 +1）内的区间被吸收；开放到 EOF 的
/// 区间吸收其后全部区间。
fn merge_ranges(ranges: Vec<LineRange>) -> Vec<LineRange> {
    let mut merged: Vec<LineRange> = Vec::with_capacity(ranges.len());
    for range in ranges {
        let Some(last) = merged.last_mut() else {
            merged.push(range);
            continue;
        };
        let Some(last_end) = last.end else {
            // 上一段已开放到 EOF，后续区间全部被吸收。
            continue;
        };
        if range.start <= last_end + 1 {
            if range.end.is_none() || range.end > last.end {
                last.end = range.end;
            }
        } else {
            merged.push(range);
        }
    }
    merged
}

/// 解析单段行区间 chunk：`N`、`N-M`、`N+K`、`N-`。
fn parse_line_range_chunk(chunk: &str) -> Result<LineRange, SelectorError> {
    let caps = LINE_RANGE_CHUNK_RE
        .captures(chunk)
        .ok_or_else(|| invalid_selector(chunk))?;
    let start_str = caps.get(1).map_or("", |m| m.as_str());
    let start: usize = start_str
        .parse()
        .map_err(|_| SelectorError(format!("行号 '{start_str}' 超出可处理范围。")))?;
    if start < 1 {
        return Err(SelectorError(
            "行选择器 0 无效；行号从 1 开始，用 :1。".to_owned(),
        ));
    }

    let sep = caps.get(2).map(|m| m.as_str());
    let rhs_str = caps.get(3).map(|m| m.as_str());
    let end = match sep {
        Some("+") => {
            let rhs_str = rhs_str.ok_or_else(|| {
                SelectorError(format!("区间 '{chunk}' 无效：+ 后必须跟一个 >= 1 的行数。"))
            })?;
            let count: usize = rhs_str
                .parse()
                .map_err(|_| SelectorError(format!("行数 '{rhs_str}' 超出可处理范围。")))?;
            if count < 1 {
                return Err(SelectorError(format!(
                    "区间 '{chunk}' 无效：行数必须 >= 1。"
                )));
            }
            let end = start
                .checked_add(count)
                .and_then(|value| value.checked_sub(1))
                .ok_or_else(|| SelectorError(format!("区间 '{chunk}' 计算溢出。")))?;
            Some(end)
        }
        Some("-") => match rhs_str {
            Some(rhs_str) => {
                let end: usize = rhs_str
                    .parse()
                    .map_err(|_| SelectorError(format!("行号 '{rhs_str}' 超出可处理范围。")))?;
                if end < start {
                    return Err(SelectorError(format!(
                        "区间 '{chunk}' 无效：结束行必须 >= 起始行。"
                    )));
                }
                Some(end)
            }
            None => None,
        },
        _ => None,
    };
    Ok(LineRange { start, end })
}

/// 把 [`PathError`] 翻译成面向模型的 [`ToolError`]；`ls` 复用同一份转换。
pub(crate) fn path_error_to_tool_error(err: &PathError) -> ToolError {
    match err {
        PathError::Empty => output::error("path 不能为空。"),
        PathError::NotUtf8 => output::error("路径包含无法用合法 UTF-8 表示的字节。"),
    }
}

/// 行号越界时的面向模型说明：给方向而不是空报错。
///
/// 移植自 oh-my-pi `packages/coding-agent/src/tools/read.ts:2724-2734`。
fn out_of_bounds_message(requested_line: usize, total_lines: usize) -> String {
    let suggestion = if total_lines == 0 {
        "该文件为空。".to_owned()
    } else {
        format!("用 :1 从头读，或 :{total_lines} 读最后一行。")
    };
    format!("第 {requested_line} 行超出文件末尾（共 {total_lines} 行）。{suggestion}")
}

/// 续读提示：末尾追加 `[还有 N 行。用 :<next> 继续]`。
///
/// 移植自 oh-my-pi `packages/coding-agent/src/tools/read.ts:2900-2902`。
fn continuation_hint(next_offset: usize, remaining_lines: usize) -> String {
    format!("[还有 {remaining_lines} 行。用 :{next_offset} 继续]")
}

/// `read` 工具。
#[derive(Debug)]
pub(crate) struct ReadTool {
    workspace: Arc<Workspace>,
    /// 无选择器/无显式结束行时的默认页大小；来自 `config.tools.read_max_lines`，
    /// 钳在 `[1, 硬顶行数]` 之间。
    default_limit: usize,
}

impl ReadTool {
    /// 构造 `read` 工具。
    pub(crate) fn new(workspace: Arc<Workspace>, config: &ToolsConfig) -> Self {
        let hard_cap = TruncateLimits::default().max_lines;
        Self {
            workspace,
            default_limit: config.read_max_lines.clamp(1, hard_cap),
        }
    }
}

#[async_trait]
impl Tool for ReadTool {
    fn name(&self) -> &'static str {
        "read"
    }

    fn description(&self) -> &'static str {
        include_str!("./prompts/read.md")
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "本地路径，可选带一个冒号选择器后缀（如 :10-20、:raw）控制分页与显示模式；完整语法见工具说明。"
                }
            },
            "required": ["path"],
            "additionalProperties": false
        })
    }

    fn approval(&self, args: &Value) -> ApprovalDecision {
        let Some(raw) = args.get("path").and_then(Value::as_str) else {
            return ApprovalDecision::tier(Tier::Read);
        };
        let (path_str, _sel) = split_path_and_selector(raw);
        match self.workspace.resolve(path_str) {
            // 落在 workspace 外时抬高一档（Read -> Write）：读取任意系统路径的爆炸半径比读
            // 工作区内文件更大，交给用户/策略多看一眼，而不是和区内读取一样静默放行。
            Ok(resolved) if resolved.outside_root => ApprovalDecision::tier(Tier::Write),
            _ => ApprovalDecision::tier(Tier::Read),
        }
    }

    fn concurrency(&self, _args: &Value) -> Concurrency {
        Concurrency::Shared
    }

    async fn execute(&self, args: Value, _ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        let raw_path = args
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| output::error("path 参数缺失或不是字符串。"))?;

        let (mut path_str, sel_text) = split_path_and_selector(raw_path);
        let mut selector = Selector::None;
        if let Some(sel_text) = sel_text {
            // 字面文件名压过选择器：lstat 而不是 stat，悬空符号链接也算"存在"
            // （oh-my-pi issue #4618）。
            let literal = self
                .workspace
                .resolve(raw_path)
                .map_err(|err| path_error_to_tool_error(&err))?;
            let literal_exists = tokio::fs::symlink_metadata(&literal.path).await.is_ok();
            if literal_exists {
                path_str = raw_path;
            } else {
                selector = parse_selector(sel_text).map_err(|err| output::error(err.0))?;
            }
        }

        let resolved = self
            .workspace
            .resolve(path_str)
            .map_err(|err| path_error_to_tool_error(&err))?;
        let display_path = self.workspace.display(&resolved.path);

        let metadata = tokio::fs::metadata(&resolved.path)
            .await
            .map_err(|err| output::error(format!("无法访问 '{display_path}': {err}")))?;

        if metadata.is_dir() {
            return read_directory(&resolved.path, &display_path).await;
        }

        read_file(&resolved.path, &display_path, selector, self.default_limit).await
    }
}

/// read 的目录分支：浅层摘要，深度与每目录条目数都很小，完整树留给 `ls`。
async fn read_directory(path: &Path, display_path: &str) -> Result<ToolOutput, ToolError> {
    let root = path.to_path_buf();
    let result = tokio::task::spawn_blocking(move || {
        ls::walk(
            &root,
            ls::WalkOptions {
                max_depth: READ_DIR_MAX_DEPTH,
                per_dir_limit: READ_DIR_PER_DIR_LIMIT,
                max_entries: usize::MAX,
                respect_gitignore: true,
            },
        )
    })
    .await
    .map_err(|err| output::error(format!("目录遍历任务失败: {err}")))?;

    let mut body = ls::render(display_path, &result);
    body.push_str("\n\n需要完整目录树请改用 ls 工具。");
    Ok(output::finish(body, format!("目录 {display_path}")))
}

/// 探测文件是否为文本，并按选择器分派到单区间/多区间/整文件读取；判定为二进制时转图片
/// 流水线或报错。
async fn read_file(
    path: &Path,
    display_path: &str,
    selector: Selector,
    default_limit: usize,
) -> Result<ToolOutput, ToolError> {
    let sample = read_sample(path).await?;
    if sample.contains(&0) {
        return read_binary_or_image(path, display_path).await;
    }

    match selector {
        Selector::None => single_range(path, display_path, 1, None, false, default_limit).await,
        Selector::Raw => single_range(path, display_path, 1, None, true, default_limit).await,
        Selector::Lines { ranges, raw } if ranges.len() == 1 => {
            let head = ranges
                .first()
                .copied()
                .ok_or_else(|| output::error("内部错误：选择器解析出的行区间列表为空。"))?;
            single_range(path, display_path, head.start, head.end, raw, default_limit).await
        }
        Selector::Lines { ranges, raw } => {
            multi_range(path, display_path, &ranges, raw, default_limit).await
        }
    }
}

/// 读取文件开头一小段样本，用于文本/二进制判定。
async fn read_sample(path: &Path) -> Result<Vec<u8>, ToolError> {
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|err| output::error(format!("无法打开文件: {err}")))?;
    let mut buf = vec![0_u8; SNIFF_SAMPLE_BYTES];
    let read = file
        .read(&mut buf)
        .await
        .map_err(|err| output::error(format!("无法读取文件: {err}")))?;
    buf.truncate(read);
    Ok(buf)
}

/// 判定为二进制后的分支：能识别为受支持格式的图片就走图像流水线，否则报错说明原因。
async fn read_binary_or_image(path: &Path, display_path: &str) -> Result<ToolOutput, ToolError> {
    let bytes = tokio::fs::read(path)
        .await
        .map_err(|err| output::error(format!("无法读取文件: {err}")))?;

    match probe_dimensions(&bytes) {
        Ok(_) => {
            let processed = process_image(&bytes, &ResizeOptions::default())
                .map_err(|err| output::error(format!("图片处理失败: {err}")))?;
            let data = BASE64.encode(&processed.bytes);
            let image = StoredImage {
                media_type: processed.mime.as_str().to_owned(),
                data,
            };
            let title = match processed.dimension_note() {
                Some(note) => format!("{display_path}（{note}）"),
                None => display_path.to_owned(),
            };
            Ok(ToolOutput::image(image).with_title(title))
        }
        Err(err) => Err(output::error(format!(
            "'{display_path}' 是二进制文件，无法按文本读取，也不是受支持的图片格式（{err}）。"
        ))),
    }
}

/// 单个显式区间（或无选择器/`raw`）的读取路径：负责上下文扩展、字节预算缩放、越界与
/// 续读提示。
#[allow(clippy::too_many_arguments)]
async fn single_range(
    path: &Path,
    display_path: &str,
    start: usize,
    end: Option<usize>,
    raw: bool,
    default_limit: usize,
) -> Result<ToolOutput, ToolError> {
    let requested_start = start.max(1);

    // 上下文扩展仅在存在显式区间信息时生效；raw 一律不补——没有行号时补出来的行与请求
    // 内容无法区分。移植自 oh-my-pi read.ts:2686-2691。
    let expand_start = !raw && requested_start > 1;
    let expand_end = !raw && end.is_some();
    let leading = if expand_start {
        RANGE_LEADING_CONTEXT_LINES.min(requested_start - 1)
    } else {
        0
    };
    let trailing = if expand_end {
        RANGE_TRAILING_CONTEXT_LINES
    } else {
        0
    };
    let window_start = requested_start - leading;

    let requested_len = end.map_or(default_limit, |value| {
        value.saturating_sub(requested_start).saturating_add(1)
    });

    let default_max_lines = TruncateLimits::default().max_lines;
    let default_max_bytes = TruncateLimits::default().max_bytes;
    let max_lines_to_collect = requested_len
        .saturating_add(leading)
        .saturating_add(trailing)
        .min(default_max_lines);
    // 字节预算随行数缩放，否则配置了较大行数上限也会在默认字节封顶处被砍断。
    let max_bytes = max_lines_to_collect
        .saturating_mul(BYTES_PER_LINE_ESTIMATE)
        .max(default_max_bytes);

    let target = LineRange {
        start: window_start,
        end: window_start.checked_add(max_lines_to_collect.saturating_sub(1)),
    };

    let owned_path = path.to_path_buf();
    let scan = tokio::task::spawn_blocking(move || {
        scan_ranges(
            &owned_path,
            &[target],
            &[max_lines_to_collect],
            max_lines_to_collect,
            max_bytes,
        )
    })
    .await
    .map_err(|err| output::error(format!("读取任务失败: {err}")))?
    .map_err(|err| output::error(format!("无法读取 '{display_path}': {err}")))?;
    let (mut windows, total_lines) = scan;

    if requested_start > total_lines {
        return Err(output::error(out_of_bounds_message(
            requested_start,
            total_lines,
        )));
    }

    let lines = windows.pop().unwrap_or_default();
    let collected_count = lines.len();

    if collected_count == 0 {
        // 有效起点但一行都没收集到：唯一的成因是首行本身就超过字节预算。
        return Err(output::error(format!(
            "第 {window_start} 行超过 {max_bytes} 字节的读取预算，无法完整返回该行。"
        )));
    }

    let mut body = if raw {
        lines.join("\n")
    } else {
        lines
            .iter()
            .enumerate()
            .map(|(offset, line)| format!("{}:{line}", window_start + offset))
            .collect::<Vec<_>>()
            .join("\n")
    };

    let last_line = window_start + collected_count - 1;
    if last_line < total_lines {
        body.push_str("\n\n");
        body.push_str(&continuation_hint(last_line + 1, total_lines - last_line));
    }

    Ok(output::finish(body, display_path.to_owned()))
}

/// 逗号多段选择器的读取路径：不做上下文扩展（每段各自独立、互不相邻），每段渲染为一个带
/// 行号范围标题的区块。
async fn multi_range(
    path: &Path,
    display_path: &str,
    ranges: &[LineRange],
    raw: bool,
    default_limit: usize,
) -> Result<ToolOutput, ToolError> {
    let default_max_lines = TruncateLimits::default().max_lines;
    let default_max_bytes = TruncateLimits::default().max_bytes;

    let per_range_caps: Vec<usize> = ranges
        .iter()
        .map(|range| {
            range.end.map_or(default_limit, |end| {
                end.saturating_sub(range.start).saturating_add(1)
            })
        })
        .collect();
    let lines_to_collect = per_range_caps.iter().sum::<usize>().min(default_max_lines);
    let max_bytes = lines_to_collect
        .saturating_mul(BYTES_PER_LINE_ESTIMATE)
        .max(default_max_bytes);

    let owned_path = path.to_path_buf();
    let owned_ranges = ranges.to_vec();
    let owned_caps = per_range_caps;
    let (windows, total_lines) = tokio::task::spawn_blocking(move || {
        scan_ranges(
            &owned_path,
            &owned_ranges,
            &owned_caps,
            lines_to_collect,
            max_bytes,
        )
    })
    .await
    .map_err(|err| output::error(format!("读取任务失败: {err}")))?
    .map_err(|err| output::error(format!("无法读取 '{display_path}': {err}")))?;

    let smallest_start = ranges.iter().map(|range| range.start).min().unwrap_or(1);
    if smallest_start > total_lines {
        return Err(output::error(out_of_bounds_message(
            smallest_start,
            total_lines,
        )));
    }

    let mut body = String::new();
    for (range, lines) in ranges.iter().zip(windows.iter()) {
        if !body.is_empty() {
            body.push_str("\n\n");
        }
        let end_label = range
            .end
            .map_or_else(|| "EOF".to_owned(), |end| end.to_string());
        if lines.is_empty() {
            let _ = write!(
                body,
                "--- 第 {}-{end_label} 行（超出文件末尾，共 {total_lines} 行）---",
                range.start
            );
            continue;
        }
        if !raw {
            let _ = writeln!(body, "--- 第 {}-{end_label} 行 ---", range.start);
        }
        let rendered = lines
            .iter()
            .enumerate()
            .map(|(offset, line)| {
                if raw {
                    line.clone()
                } else {
                    format!("{}:{line}", range.start + offset)
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        body.push_str(&rendered);
    }

    Ok(output::finish(body, display_path.to_owned()))
}

/// 单次前向扫描收集若干个（已排序、互不重叠的）行区间，返回每段收集到的行以及文件总行数。
///
/// 每段独立受 `per_range_caps` 限制，全局共享 `global_max_lines`/`global_max_bytes` 预算；
/// 为了拿到准确的 `total_lines`（越界提示与续读提示都需要），无论预算是否已耗尽都会扫描
/// 到文件末尾——这是有意的简化：本工具面向典型代码文件，接受一次线性扫描的成本，不像
/// oh-my-pi 那样为巨型文件维护单独的"不扫到 EOF"快照路径。
fn scan_ranges(
    path: &Path,
    targets: &[LineRange],
    per_range_caps: &[usize],
    global_max_lines: usize,
    global_max_bytes: usize,
) -> std::io::Result<(Vec<Vec<String>>, usize)> {
    use std::io::BufRead as _;

    let file = std::fs::File::open(path)?;
    let mut reader = std::io::BufReader::new(file);
    let mut windows: Vec<Vec<String>> = targets.iter().map(|_| Vec::new()).collect();
    let mut range_idx = 0_usize;
    let mut total_lines = 0_usize;
    let mut collected_lines_total = 0_usize;
    let mut collected_bytes_total = 0_usize;
    let mut buf = String::new();

    loop {
        buf.clear();
        let read = reader.read_line(&mut buf)?;
        if read == 0 {
            break;
        }
        total_lines += 1;

        while let Some(target) = targets.get(range_idx) {
            let past_end = target.end.is_some_and(|end| total_lines > end);
            if past_end {
                range_idx += 1;
                continue;
            }
            break;
        }

        let Some(target) = targets.get(range_idx) else {
            continue;
        };
        if total_lines < target.start {
            continue;
        }
        if collected_lines_total >= global_max_lines || collected_bytes_total >= global_max_bytes {
            continue;
        }
        let cap = per_range_caps.get(range_idx).copied().unwrap_or(0);
        let Some(window) = windows.get_mut(range_idx) else {
            continue;
        };
        if window.len() >= cap {
            continue;
        }

        let line = strip_line_ending(&buf);
        collected_bytes_total += line.len() + 1;
        collected_lines_total += 1;
        window.push(line.to_owned());
    }

    Ok((windows, total_lines))
}

/// 去掉一行末尾的 `\n`/`\r\n`（不改变其余内容，字节截断只发生在结尾）。
fn strip_line_ending(line: &str) -> &str {
    match line.strip_suffix('\n') {
        Some(rest) => rest.strip_suffix('\r').unwrap_or(rest),
        None => line,
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn parses_bare_line_number() {
        assert_eq!(
            parse_selector("42").unwrap(),
            Selector::Lines {
                ranges: vec![LineRange {
                    start: 42,
                    end: None
                }],
                raw: false,
            }
        );
    }

    #[test]
    fn parses_inclusive_range() {
        assert_eq!(
            parse_selector("5-16").unwrap(),
            Selector::Lines {
                ranges: vec![LineRange {
                    start: 5,
                    end: Some(16)
                }],
                raw: false,
            }
        );
    }

    #[test]
    fn parses_count_range() {
        assert_eq!(
            parse_selector("5+10").unwrap(),
            Selector::Lines {
                ranges: vec![LineRange {
                    start: 5,
                    end: Some(14)
                }],
                raw: false,
            }
        );
    }

    #[test]
    fn parses_open_ended_range() {
        assert_eq!(
            parse_selector("301-").unwrap(),
            Selector::Lines {
                ranges: vec![LineRange {
                    start: 301,
                    end: None
                }],
                raw: false,
            }
        );
    }

    #[test]
    fn parses_comma_separated_multi_range() {
        assert_eq!(
            parse_selector("5-16,960-973").unwrap(),
            Selector::Lines {
                ranges: vec![
                    LineRange {
                        start: 5,
                        end: Some(16)
                    },
                    LineRange {
                        start: 960,
                        end: Some(973)
                    },
                ],
                raw: false,
            }
        );
    }

    #[test]
    fn parses_raw_alone() {
        assert_eq!(parse_selector("raw").unwrap(), Selector::Raw);
        assert_eq!(parse_selector("RAW").unwrap(), Selector::Raw);
    }

    #[test]
    fn parses_raw_with_range_either_order() {
        let expected = Selector::Lines {
            ranges: vec![LineRange {
                start: 2,
                end: Some(4),
            }],
            raw: true,
        };
        assert_eq!(parse_selector("raw:2-4").unwrap(), expected);
        assert_eq!(parse_selector("2-4:raw").unwrap(), expected);
    }

    #[test]
    fn merges_overlapping_ranges() {
        assert_eq!(
            parse_selector("5-16,10-20").unwrap(),
            Selector::Lines {
                ranges: vec![LineRange {
                    start: 5,
                    end: Some(20)
                }],
                raw: false,
            }
        );
    }

    #[test]
    fn rejects_invalid_compound_selector() {
        assert!(parse_selector("raw:raw").is_err());
        assert!(parse_selector("5-10:20-30").is_err());
    }

    #[test]
    fn rejects_zero_line_selector() {
        assert!(parse_selector("0").is_err());
    }

    #[test]
    fn rejects_descending_range() {
        assert!(parse_selector("10-5").is_err());
    }

    #[test]
    fn split_ignores_windows_drive_letters() {
        let (path, sel) = split_path_and_selector(r"C:\Users\zero\file.txt");
        assert_eq!(path, r"C:\Users\zero\file.txt");
        assert_eq!(sel, None);
    }

    #[test]
    fn split_peels_trailing_selector() {
        let (path, sel) = split_path_and_selector("src/lib.rs:10-20");
        assert_eq!(path, "src/lib.rs");
        assert_eq!(sel, Some("10-20"));
    }

    #[test]
    fn split_peels_compound_selector() {
        let (path, sel) = split_path_and_selector("src/lib.rs:raw:10-20");
        assert_eq!(path, "src/lib.rs");
        assert_eq!(sel, Some("raw:10-20"));
    }

    #[test]
    fn out_of_bounds_message_gives_actionable_suggestion() {
        let msg = out_of_bounds_message(50, 10);
        assert!(msg.contains(":1"));
        assert!(msg.contains(":10"));
        assert!(msg.contains("10 行"));
    }

    #[test]
    fn out_of_bounds_message_notes_empty_file() {
        let msg = out_of_bounds_message(1, 0);
        assert!(msg.contains("该文件为空"));
    }

    #[test]
    fn continuation_hint_uses_next_offset() {
        let hint = continuation_hint(21, 5);
        assert_eq!(hint, "[还有 5 行。用 :21 继续]");
    }

    #[tokio::test]
    async fn read_beyond_eof_reports_actionable_bounds_and_next_offset() {
        let dir = tempdir().expect("tempdir 创建失败");
        let path = dir.path().join("small.txt");
        std::fs::write(&path, "a\nb\nc\n").expect("写入测试文件失败");

        let out_of_range = single_range(&path, "small.txt", 10, None, false, 300)
            .await
            .expect_err("越界读取应当返回错误");
        assert!(matches!(out_of_range, ToolError::Failed(_)));
        if let ToolError::Failed(msg) = out_of_range {
            assert!(msg.contains("3 行"));
        }

        let output = single_range(&path, "small.txt", 1, None, false, 2)
            .await
            .expect("在界内的读取不应失败");
        let text = extract_text(&output);
        assert!(text.contains("[还有 1 行。用 :3 继续]"));
    }

    #[tokio::test]
    async fn raw_selector_skips_context_expansion_and_line_numbers() {
        let dir = tempdir().expect("tempdir 创建失败");
        let path = dir.path().join("lines.txt");
        // 恰好 3 行：请求区间 3-3 命中文件末尾，续读提示不会介入，纯粹隔离
        // "raw 不补上下文、不加行号前缀" 这一件事。
        std::fs::write(&path, "l1\nl2\nl3\n").expect("写入测试文件失败");

        let output = single_range(&path, "lines.txt", 3, Some(3), true, 300)
            .await
            .expect("raw 读取不应失败");
        let text = extract_text(&output);
        // raw 模式：既不补上下文（应当只有 l3 一行），也不加行号前缀。
        assert_eq!(text, "l3");
    }

    #[tokio::test]
    async fn explicit_range_expands_context() {
        let dir = tempdir().expect("tempdir 创建失败");
        let path = dir.path().join("lines.txt");
        std::fs::write(&path, "l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\n").expect("写入测试文件失败");

        let output = single_range(&path, "lines.txt", 3, Some(3), false, 300)
            .await
            .expect("读取不应失败");
        let text = extract_text(&output);
        // 显式区间 3-3：起点前补 1 行（第 2 行）、终点后补 3 行（第 4-6 行）。
        assert!(text.contains("2:l2"));
        assert!(text.contains("3:l3"));
        assert!(text.contains("6:l6"));
        assert!(!text.contains("7:l7"));
    }

    fn extract_text(output: &ToolOutput) -> String {
        output
            .content
            .iter()
            .filter_map(|block| match block {
                zcode_agent::StoredToolResultContent::Text { text } => Some(text.clone()),
                zcode_agent::StoredToolResultContent::Image { .. } => None,
            })
            .collect::<String>()
    }
}
