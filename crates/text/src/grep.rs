//! 进程内 ripgrep 引擎：遍历、匹配、分页、取消。
//!
//! 设计依据（调研已定，见任务说明）：进程内嵌入 `grep-*` 系列 crate 而不是 fork `rg` 子进程，
//! 换来零进程启动开销、无需解析 stdout、且取消信号能真正打断正在进行的搜索。目录遍历统一交给
//! `ignore::WalkBuilder`，不自行实现遍历器。
//!
//! [`grep`] 是**同步阻塞函数**：它会占用调用线程直到搜索完成、超时或被取消。异步调用方必须自行
//! 通过 `tokio::task::spawn_blocking`（或等价机制）派发，本 crate 不引入 `tokio` 依赖。

use std::{
    io::{self, Read},
    path::{Path, PathBuf},
    sync::{
        OnceLock,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use grep_searcher::{BinaryDetection, Searcher, SearcherBuilder, Sink, SinkMatch};
use ignore::{
    WalkBuilder,
    overrides::{Override, OverrideBuilder},
};
use rayon::iter::{IntoParallelRefIterator, ParallelBridge, ParallelIterator};
use thiserror::Error;
use tracing::{debug, warn};

use crate::width::truncate_to_width;

/// 大小写匹配模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaseMode {
    /// 严格区分大小写。
    Sensitive,
    /// 忽略大小写。
    Insensitive,
    /// “智能大小写”：模式中的字面字符全部小写时忽略大小写，否则区分（ripgrep `--smart-case` 语义）。
    Smart,
}

/// 实际生效的匹配器种类，回报给调用方以弥补“静默降级”的信息缺口。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatcherKind {
    /// `pattern` 被当作正则表达式编译成功。
    Regex,
    /// `pattern` 编译为正则失败，已退化为按字面量（`regex::escape` 转义后）匹配。
    Literal,
}

/// 一次 grep 调用的资源上限。
///
/// 各字段默认值及其成立前提见 [`Default`] 实现上的注释；调用方可按需覆盖。
#[derive(Debug, Clone, Copy)]
pub struct GrepLimits {
    /// 结果中最多包含的命中文件数。该配额只被**有至少一条匹配的文件**占用；零命中的文件
    /// 不消耗配额，会继续被搜索（规模由 `timeout` 与 `cancel` 兜底），只是不出现在结果里。
    pub max_files: usize,
    /// 每个文件最多保留的匹配行数，超出部分丢弃并将该文件标记为 `truncated`。
    pub max_matches_per_file: usize,
    /// 本次调用最多保留的匹配行总数（跨文件累计）。
    pub max_total_matches: usize,
    /// 单个文件参与“整篇搜索”的字节数上限；超过该大小的文件只搜索开头这么多字节。
    pub max_file_bytes: u64,
    /// 整次调用的墙钟超时；到期后立即停止接纳新文件（已开始的单次文件搜索不会被打断）。
    pub timeout: Duration,
    /// 单条匹配行按显示宽度（而非字节或字符数）截断的上限列数。
    pub max_line_columns: usize,
}

impl Default for GrepLimits {
    fn default() -> Self {
        Self {
            // oh-my-pi `grep.ts:91-126` 实测取值；无 optimizer 脚本支撑，数字本身“前提待验证”。
            // 注意：这里限制的是“结果里的命中文件数”，不是“扫描过的文件数”——零命中文件不占配额。
            max_files: 20,
            // 同上。
            max_matches_per_file: 20,
            // 需覆盖 20×20=400 的满打满算，再留出分页余量，使调用方仍能看清准确的文件总数。
            max_total_matches: 2000,
            // 超过则只读文件开头这么大的窗口；具体数值的定量依据未知，先取常见的“大文件”阈值。
            max_file_bytes: 4 * 1024 * 1024,
            // 异步调用方即便放弃等待，线程池仍会持续搜索（巨型目录树、网络挂载）烧 CPU，
            // 必须有墙钟兜底。
            timeout: Duration::from_secs(30),
            // 单行展示上限；前提未知。必须配合 `crate::width::truncate_to_width` 按显示宽度截断。
            max_line_columns: 512,
        }
    }
}

/// 一次 grep 调用的完整请求参数。
#[derive(Debug)]
pub struct GrepRequest<'a> {
    /// 搜索模式；为空时 [`grep`] 返回 [`GrepError::EmptyPattern`]。
    pub pattern: &'a str,
    /// 搜索的根目录（或文件）列表，可以有多个。
    pub roots: &'a [PathBuf],
    /// 只搜索匹配这些 glob 的路径；为空表示不限制。
    pub include_globs: &'a [String],
    /// 排除匹配这些 glob 的路径；总是优先于 `include_globs`。
    pub exclude_globs: &'a [String],
    /// 大小写匹配模式。
    pub case: CaseMode,
    /// 是否允许匹配跨越多行。
    ///
    /// **不是需要手动打开的开关**：[`grep`] 会在此基础上再检查 `pattern` 本身——只要模式里出现
    /// 真实换行符（`\n` 字节）或字面的反斜杠加 `n`（用户在正则里写 `\n` 想匹配换行），就会自动启用
    /// 多行搜索，即便这里传 `false`。调用方通常直接传 `false`；只有希望在没有换行字面量时也强制
    /// 跨行搜索（例如配合 dot-matches-newline 场景）才需要显式传 `true`。
    pub multiline: bool,
    /// 是否遵守 `.gitignore` / `.ignore` / 全局 git 排除规则。
    pub respect_gitignore: bool,
    /// 是否包含点前缀的隐藏文件。工具层调用时恒为 `true`：agent 需要看到 `.github/`、
    /// `.env.example` 等文件。
    pub include_hidden: bool,
    /// 资源上限。
    pub limits: GrepLimits,
    /// 外部取消信号；每次进入一个新文件前检查一次，为 `true` 时立即停止接纳新工作。
    pub cancel: Option<&'a AtomicBool>,
}

/// 单条匹配行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineMatch {
    /// 匹配所在的行号（从 1 开始）。
    pub line_number: u64,
    /// 匹配行的文本，已去除行终止符并按 [`GrepLimits::max_line_columns`] 截断显示宽度。
    pub line: String,
    /// 匹配起始位置相对文件开头的绝对字节偏移。
    pub byte_offset: u64,
}

/// 单个文件内的搜索结果。
#[derive(Debug, Clone)]
pub struct FileMatches {
    /// 文件路径。
    pub path: PathBuf,
    /// 该文件中保留下来的匹配行，按出现顺序排列。
    pub matches: Vec<LineMatch>,
    /// 是否因命中 [`GrepLimits::max_matches_per_file`] 或全局匹配上限而被截断。
    pub truncated: bool,
}

/// 一次 grep 调用的汇总结果。
#[derive(Debug)]
pub struct GrepOutcome {
    /// 命中匹配的文件，按路径字典序排列。
    pub files: Vec<FileMatches>,
    /// 实际被搜索（尝试打开并交给搜索器）的文件数，不含因 `NotFound`/`PermissionDenied` 而跳过的
    /// 路径；不受 `max_files` 配额限制，可能远大于 `files.len()`——零命中文件也计入这里。
    pub files_searched: usize,
    /// 保留下来的匹配行总数；一旦触顶（`truncated == true`），该数字只是下界。
    pub total_matches: usize,
    /// 本次结果是否因任何资源上限、超时或取消而不完整。
    pub truncated: bool,
    /// 因超出 [`GrepLimits::max_file_bytes`] 且第二遍预算已耗尽而被完全跳过（未读取一个字节）的文件数。
    pub skipped_oversized: usize,
    /// 因命中二进制检测（首个 NUL 字节处截断）而提前结束读取的文件数。
    pub stopped_on_binary: usize,
    /// 是否因达到 [`GrepLimits::timeout`] 而提前结束。
    pub timed_out: bool,
    /// 实际生效的匹配器种类。
    pub matcher: MatcherKind,
}

/// grep 过程中可能失败的原因。
#[derive(Debug, Error)]
pub enum GrepError {
    /// 搜索模式为空字符串。
    #[error("搜索模式不能为空")]
    EmptyPattern,
    /// `include_globs` 或 `exclude_globs` 中的某一项不是合法的 glob。
    #[error("glob 模式 `{glob}` 无效")]
    InvalidGlob {
        /// 出问题的原始 glob 文本。
        glob: String,
        /// 底层的 glob/gitignore 解析错误。
        #[source]
        source: ignore::Error,
    },
    /// 目录遍历过程中发生了非“路径不可访问”类的错误。
    #[error("遍历目录失败")]
    Walk {
        /// 底层遍历错误。
        #[source]
        source: ignore::Error,
    },
    /// 打开或读取某个文件时发生了非 `NotFound`/`PermissionDenied` 的 IO 错误。
    #[error("读取文件 `{}` 失败", path.display())]
    Io {
        /// 出问题的文件路径。
        path: PathBuf,
        /// 底层 IO 错误。
        #[source]
        source: io::Error,
    },
}

/// 执行一次进程内 grep 搜索。
///
/// 这是**同步阻塞函数**：调用线程会被占用直到搜索完成、超时或被取消。在异步上下文中调用时，
/// 调用方必须自行用 `tokio::task::spawn_blocking`（或等价机制）派发；本 crate 不依赖 `tokio`。
///
/// # 错误
///
/// 仅在模式为空、glob 非法、目录遍历遇到非预期错误、或文件 IO 遇到非 `NotFound`/`PermissionDenied`
/// 的错误时返回 `Err`。正则编译失败**不会**导致整次调用失败——见 [`MatcherKind::Literal`]。
pub fn grep(request: &GrepRequest<'_>) -> Result<GrepOutcome, GrepError> {
    if request.pattern.is_empty() {
        return Err(GrepError::EmptyPattern);
    }

    let effective_multiline =
        request.multiline || request.pattern.contains('\n') || request.pattern.contains("\\n");
    let (matcher, matcher_kind) = build_matcher(request.pattern, request.case, effective_multiline);

    let base = request
        .roots
        .first()
        .map_or_else(|| Path::new("."), PathBuf::as_path);
    let overrides = build_overrides(base, request.include_globs, request.exclude_globs)?;

    let mut walk_builder = match request.roots.split_first() {
        Some((first, rest)) => {
            let mut builder = WalkBuilder::new(first);
            for root in rest {
                builder.add(root);
            }
            builder
        }
        None => WalkBuilder::empty(),
    };
    walk_builder
        .hidden(!request.include_hidden)
        .parents(request.respect_gitignore)
        .ignore(request.respect_gitignore)
        .git_ignore(request.respect_gitignore)
        .git_global(request.respect_gitignore)
        .git_exclude(request.respect_gitignore)
        // `.gitignore` 应当在任何目录树里生效，不要求存在真实的 `.git`（否则从压缩包/worktree
        // 拷贝出来的项目会静默失去 gitignore 支持）。
        .require_git(false)
        .overrides(overrides);
    let walker = walk_builder.build();

    let deadline = Instant::now() + request.limits.timeout;
    let ctx = SearchContext {
        matcher: &matcher,
        limits: &request.limits,
        cancel: request.cancel,
        deadline,
    };
    let state = SharedState::default();

    let make_searcher = move || {
        let mut builder = SearcherBuilder::new();
        builder.binary_detection(BinaryDetection::quit(0));
        builder.line_number(true);
        builder.multi_line(effective_multiline);
        builder.build()
    };

    // 第一遍：遍历整棵树。常规大小的文件直接整篇搜索；超限文件推迟到第二遍处理。
    let pass1: Vec<PassOutcome> = walker
        .take_while(|_| !should_stop(&state, &ctx))
        .par_bridge()
        .map_init(make_searcher, |searcher, entry_result| {
            process_entry(entry_result, searcher, &ctx, &state)
        })
        .filter_map(std::convert::identity)
        .collect();

    let mut files = Vec::with_capacity(pass1.len());
    let mut oversized = Vec::new();
    for outcome in pass1 {
        match outcome {
            PassOutcome::Matched(file_matches) => files.push(file_matches),
            PassOutcome::Deferred(path) => oversized.push(path),
        }
    }

    // 第二遍：处理被推迟的超大文件——仅当预算仍有空间时才进行，且只读取开头窗口。
    if !oversized.is_empty() {
        if should_stop(&state, &ctx) {
            state
                .skipped_oversized
                .fetch_add(oversized.len(), Ordering::Relaxed);
        } else {
            let window = request.limits.max_file_bytes;
            let pass2: Vec<Option<FileMatches>> = oversized
                .par_iter()
                .map_init(make_searcher, |searcher, path| {
                    process_deferred(path, window, searcher, &ctx, &state)
                })
                .collect();
            files.extend(pass2.into_iter().flatten());
        }
    }

    if let Some(err) = state.fatal_error.into_inner() {
        return Err(err);
    }

    files.sort_by(|a, b| a.path.cmp(&b.path));

    let truncated = files.iter().any(|file| file.truncated)
        || state.files_capped.load(Ordering::Relaxed)
        || state.matches_capped.load(Ordering::Relaxed)
        || state.timed_out.load(Ordering::Relaxed)
        || state.cancelled.load(Ordering::Relaxed)
        || state.skipped_oversized.load(Ordering::Relaxed) > 0;

    Ok(GrepOutcome {
        files,
        files_searched: state.files_searched.load(Ordering::Relaxed),
        total_matches: state.total_matches.load(Ordering::Relaxed),
        truncated,
        skipped_oversized: state.skipped_oversized.load(Ordering::Relaxed),
        stopped_on_binary: state.stopped_on_binary.load(Ordering::Relaxed),
        timed_out: state.timed_out.load(Ordering::Relaxed),
        matcher: matcher_kind,
    })
}

/// 搜索过程中跨线程共享、贯穿整次调用的只读上下文。
struct SearchContext<'a> {
    matcher: &'a RegexMatcher,
    limits: &'a GrepLimits,
    cancel: Option<&'a AtomicBool>,
    deadline: Instant,
}

/// 跨线程共享的可变状态；除 `fatal_error` 外一律是无锁原子量。
#[derive(Default)]
struct SharedState {
    /// 实际被搜索（成功打开并交给搜索器）的文件总数；不受 `max_files` 约束，只被
    /// `should_stop` 里的超时/取消条件间接限制。
    files_searched: AtomicUsize,
    /// 结果中已收录的命中文件数；这是 `max_files` 真正约束的对象（零命中文件不占用它）。
    result_files: AtomicUsize,
    total_matches: AtomicUsize,
    skipped_oversized: AtomicUsize,
    stopped_on_binary: AtomicUsize,
    timed_out: AtomicBool,
    cancelled: AtomicBool,
    files_capped: AtomicBool,
    matches_capped: AtomicBool,
    /// 遍历或 IO 过程中遇到的第一个致命错误；一旦写入，整次调用最终返回 `Err`。
    fatal_error: OnceLock<GrepError>,
}

/// 遍历第一遍（未推迟的常规文件）的处理结果。
enum PassOutcome {
    /// 已完成搜索的文件。
    Matched(FileMatches),
    /// 因超过 [`GrepLimits::max_file_bytes`] 而推迟到第二遍的路径。
    Deferred(PathBuf),
}

/// 打开文件失败时的分类。
enum OpenError {
    /// `NotFound` / `PermissionDenied`：跳过，不计入 `files_searched`。
    Skip,
    /// 其他 IO 错误：致命，整次调用应以 `Err` 结束。
    Fatal(io::Error),
}

/// 单个文件的搜索结果。
struct SearchResult {
    matches: Vec<LineMatch>,
    truncated: bool,
    binary_detected: bool,
}

/// 早停判据：致命错误、取消信号、超时、文件数或匹配总数已达上限。
///
/// 调用方在**进入每个文件之前**调用一次；命中的具体原因会写回 `state` 对应的原子标志，
/// 供最终 [`GrepOutcome`] 组装时使用。
fn should_stop(state: &SharedState, ctx: &SearchContext<'_>) -> bool {
    if state.fatal_error.get().is_some() {
        return true;
    }
    if ctx.cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
        state.cancelled.store(true, Ordering::Relaxed);
        return true;
    }
    if Instant::now() >= ctx.deadline {
        state.timed_out.store(true, Ordering::Relaxed);
        return true;
    }
    if state.result_files.load(Ordering::Relaxed) >= ctx.limits.max_files {
        state.files_capped.store(true, Ordering::Relaxed);
        return true;
    }
    if state.total_matches.load(Ordering::Relaxed) >= ctx.limits.max_total_matches {
        state.matches_capped.store(true, Ordering::Relaxed);
        return true;
    }
    false
}

/// 尝试为 `counter` 占用一个名额：未达 `limit` 时原子地 +1 并返回 `true`，否则原样返回 `false`。
fn try_reserve(counter: &AtomicUsize, limit: usize) -> bool {
    let mut current = counter.load(Ordering::Relaxed);
    loop {
        if current >= limit {
            return false;
        }
        match counter.compare_exchange_weak(
            current,
            current + 1,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => return true,
            Err(observed) => current = observed,
        }
    }
}

/// 构建正则匹配器；编译失败时退化为字面量匹配，永不让整次搜索因非法正则而失败。
fn build_matcher(pattern: &str, case: CaseMode, multiline: bool) -> (RegexMatcher, MatcherKind) {
    let mut builder = RegexMatcherBuilder::new();
    builder.multi_line(multiline);
    match case {
        CaseMode::Sensitive => {}
        CaseMode::Insensitive => {
            builder.case_insensitive(true);
        }
        CaseMode::Smart => {
            builder.case_smart(true);
        }
    }

    match builder.build(pattern) {
        Ok(matcher) => (matcher, MatcherKind::Regex),
        Err(err) => {
            warn!(pattern, error = %err, "正则编译失败，退化为字面量匹配");
            let literal = regex::escape(pattern);
            #[allow(clippy::expect_used)]
            // 不变量：`regex::escape` 的输出只包含被转义后的元字符，对 `RegexMatcherBuilder`
            // 而言必定是合法模式；此处失败意味着 grep-regex 自身的不变量被打破，而非用户输入问题。
            let matcher = builder
                .build(&literal)
                .expect("regex::escape 转义后的字面量对 RegexMatcherBuilder 必定合法");
            (matcher, MatcherKind::Literal)
        }
    }
}

/// 把 `include_globs` / `exclude_globs` 编译成 `ignore` 的 override 匹配器。
///
/// `include_globs` 中的每一项都是白名单 glob；`exclude_globs` 中的每一项取反后加入
/// （`ignore::overrides::OverrideBuilder` 的语义：不带 `!` 的 glob 是白名单，带 `!` 的是排除）。
fn build_overrides(
    base: &Path,
    include_globs: &[String],
    exclude_globs: &[String],
) -> Result<Override, GrepError> {
    let mut builder = OverrideBuilder::new(base);
    for glob in include_globs {
        builder.add(glob).map_err(|source| GrepError::InvalidGlob {
            glob: glob.clone(),
            source,
        })?;
    }
    for glob in exclude_globs {
        let negated = format!("!{glob}");
        builder
            .add(&negated)
            .map_err(|source| GrepError::InvalidGlob {
                glob: glob.clone(),
                source,
            })?;
    }
    builder.build().map_err(|source| GrepError::Walk { source })
}

/// 遍历过程中遇到的非致命/致命错误分类：`NotFound`/`PermissionDenied` 记为跳过并记日志，
/// 其他一律视为致命并写入 `state.fatal_error`。
fn handle_walk_error(err: ignore::Error, state: &SharedState) {
    let benign = err.io_error().is_some_and(|io_err| {
        matches!(
            io_err.kind(),
            io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied
        )
    });
    if benign {
        debug!(error = %err, "跳过不可访问的路径");
    } else {
        warn!(error = %err, "遍历目录失败，终止本次搜索");
        let _ = state.fatal_error.set(GrepError::Walk { source: err });
    }
}

/// 打开一个候选文件；`NotFound`/`PermissionDenied` 归为可跳过，其余视为致命。
fn open_file(path: &Path) -> Result<std::fs::File, OpenError> {
    match std::fs::File::open(path) {
        Ok(file) => Ok(file),
        Err(err)
            if matches!(
                err.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied
            ) =>
        {
            Err(OpenError::Skip)
        }
        Err(err) => Err(OpenError::Fatal(err)),
    }
}

/// 处理第一遍遍历中的单个 `DirEntry`：跳过目录/非常规文件，超限文件推迟到第二遍，
/// 其余立即整篇搜索。早停判据在进入文件之前检查。
fn process_entry(
    entry_result: Result<ignore::DirEntry, ignore::Error>,
    searcher: &mut Searcher,
    ctx: &SearchContext<'_>,
    state: &SharedState,
) -> Option<PassOutcome> {
    if should_stop(state, ctx) {
        return None;
    }

    let entry = match entry_result {
        Ok(entry) => entry,
        Err(err) => {
            handle_walk_error(err, state);
            return None;
        }
    };

    if !entry
        .file_type()
        .is_some_and(|file_type| file_type.is_file())
    {
        return None;
    }

    let size = match entry.metadata() {
        Ok(metadata) => metadata.len(),
        Err(err) => {
            handle_walk_error(err, state);
            return None;
        }
    };

    if size > ctx.limits.max_file_bytes {
        return Some(PassOutcome::Deferred(entry.path().to_path_buf()));
    }

    let result = match run_search(entry.path(), None, searcher, ctx) {
        Ok(result) => result,
        Err(OpenError::Skip) => return None,
        Err(OpenError::Fatal(source)) => {
            let _ = state.fatal_error.set(GrepError::Io {
                path: entry.path().to_path_buf(),
                source,
            });
            return None;
        }
    };

    // `max_files` 只约束“结果里的命中文件数”，不约束扫描规模：文件无论命中与否都要计入
    // `files_searched`，零命中文件绝不能因为占了配额而拖累后面真正的命中被漏掉。
    state.files_searched.fetch_add(1, Ordering::Relaxed);
    if result.binary_detected {
        state.stopped_on_binary.fetch_add(1, Ordering::Relaxed);
    }
    admit_result(entry.path(), result, ctx, state).map(PassOutcome::Matched)
}

/// 处理第二遍中被推迟的超大文件：只读取开头 `window` 字节。早停判据同样在进入文件之前检查。
fn process_deferred(
    path: &Path,
    window: u64,
    searcher: &mut Searcher,
    ctx: &SearchContext<'_>,
    state: &SharedState,
) -> Option<FileMatches> {
    if should_stop(state, ctx) {
        state.skipped_oversized.fetch_add(1, Ordering::Relaxed);
        return None;
    }

    let result = match run_search(path, Some(window), searcher, ctx) {
        Ok(result) => result,
        Err(OpenError::Skip) => return None,
        Err(OpenError::Fatal(source)) => {
            let _ = state.fatal_error.set(GrepError::Io {
                path: path.to_path_buf(),
                source,
            });
            return None;
        }
    };

    state.files_searched.fetch_add(1, Ordering::Relaxed);
    if result.binary_detected {
        state.stopped_on_binary.fetch_add(1, Ordering::Relaxed);
    }
    admit_result(path, result, ctx, state)
}

/// 决定一次成功的文件搜索是否进入结果集：零命中文件直接丢弃（不占 `max_files` 配额）；
/// 有命中的文件先争抢 `max_files` 名额，抢到后再把它的匹配逐条计入全局 `max_total_matches` 预算，
/// 预算不够时就地截断并标记 `truncated`。
fn admit_result(
    path: &Path,
    result: SearchResult,
    ctx: &SearchContext<'_>,
    state: &SharedState,
) -> Option<FileMatches> {
    if result.matches.is_empty() {
        return None;
    }
    if !try_reserve(&state.result_files, ctx.limits.max_files) {
        state.files_capped.store(true, Ordering::Relaxed);
        return None;
    }

    let mut matches = result.matches;
    let mut truncated = result.truncated;
    if !admit_into_total_budget(
        &mut matches,
        &state.total_matches,
        ctx.limits.max_total_matches,
    ) {
        truncated = true;
        state.matches_capped.store(true, Ordering::Relaxed);
    }
    Some(FileMatches {
        path: path.to_path_buf(),
        matches,
        truncated,
    })
}

/// 把已经收集好的 `matches` 逐条计入全局 `total_matches` 预算；一旦触顶就地截断剩余部分。
/// 返回 `false` 表示发生了截断。
fn admit_into_total_budget(
    matches: &mut Vec<LineMatch>,
    total_matches: &AtomicUsize,
    limit: usize,
) -> bool {
    let mut admitted = 0_usize;
    while admitted < matches.len() && try_reserve(total_matches, limit) {
        admitted += 1;
    }
    let complete = admitted == matches.len();
    matches.truncate(admitted);
    complete
}

/// 打开并搜索单个已确认为常规文件的路径。
///
/// `window` 为 `Some(n)` 时只读取文件开头 `n` 字节（超大文件的第二遍）；`None` 时整篇搜索。
/// 不使用内存映射：并发写入会在读取期间触发 page fault，因此统一走有界的 `Read` 路径。
fn run_search(
    path: &Path,
    window: Option<u64>,
    searcher: &mut Searcher,
    ctx: &SearchContext<'_>,
) -> Result<SearchResult, OpenError> {
    let file = open_file(path)?;
    let mut sink = LineSink::new(ctx.limits.max_matches_per_file, ctx.limits.max_line_columns);
    let outcome = match window {
        None => searcher.search_file(ctx.matcher, &file, &mut sink),
        Some(cap) => searcher.search_reader(ctx.matcher, file.take(cap), &mut sink),
    };
    if let Err(err) = outcome {
        // 搜索器自身报错（例如堆限制、编码问题）按“已搜索、零匹配”处理，不是跳过：
        // 文件确实被打开并尝试搜索过。
        debug!(path = %path.display(), error = %err, "grep 搜索器报错，按已搜索零匹配处理");
    }
    Ok(SearchResult {
        matches: sink.matches,
        truncated: sink.truncated,
        binary_detected: sink.binary_detected,
    })
}

/// 把 grep-searcher 的推送式匹配事件收集为 [`LineMatch`] 列表；只负责单文件内的
/// `max_matches_per_file` 截断，全局 `max_total_matches` 预算由调用方在搜索结束后统一核算
/// （见 [`admit_into_total_budget`]），避免在搜索进行中为了“回滚已计入的全局配额”而复杂化。
struct LineSink {
    max_per_file: usize,
    max_line_columns: usize,
    matches: Vec<LineMatch>,
    truncated: bool,
    binary_detected: bool,
}

impl LineSink {
    fn new(max_per_file: usize, max_line_columns: usize) -> Self {
        Self {
            max_per_file,
            max_line_columns,
            matches: Vec::new(),
            truncated: false,
            binary_detected: false,
        }
    }
}

impl Sink for LineSink {
    type Error = io::Error;

    fn matched(&mut self, searcher: &Searcher, mat: &SinkMatch<'_>) -> Result<bool, io::Error> {
        if self.matches.len() >= self.max_per_file {
            self.truncated = true;
            return Ok(false);
        }
        let line_number = mat
            .line_number()
            .ok_or_else(|| io::Error::other("搜索器未启用行号计数"))?;

        let terminator = searcher.line_terminator();
        let raw = mat
            .bytes()
            .strip_suffix(terminator.as_bytes())
            .unwrap_or_else(|| mat.bytes());
        let text = String::from_utf8_lossy(raw);
        let line = truncate_to_width(text.as_ref(), self.max_line_columns, "…").into_owned();

        self.matches.push(LineMatch {
            line_number,
            line,
            byte_offset: mat.absolute_byte_offset(),
        });
        Ok(true)
    }

    fn binary_data(
        &mut self,
        _searcher: &Searcher,
        _binary_byte_offset: u64,
    ) -> Result<bool, io::Error> {
        self.binary_detected = true;
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;

    use tempfile::TempDir;

    use super::*;
    use crate::width::visible_width;

    fn write_file(dir: &TempDir, name: &str, content: &str) -> PathBuf {
        let path = dir.path().join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, content).unwrap();
        path
    }

    fn request<'a>(pattern: &'a str, roots: &'a [PathBuf]) -> GrepRequest<'a> {
        GrepRequest {
            pattern,
            roots,
            include_globs: &[],
            exclude_globs: &[],
            case: CaseMode::Sensitive,
            multiline: false,
            respect_gitignore: true,
            include_hidden: true,
            limits: GrepLimits::default(),
            cancel: None,
        }
    }

    #[test]
    fn finds_basic_match_with_line_number() {
        let dir = TempDir::new().unwrap();
        write_file(&dir, "a.txt", "one\ntwo needle\nthree\n");
        let roots = [dir.path().to_path_buf()];

        let outcome = grep(&request("needle", &roots)).unwrap();

        assert_eq!(outcome.files.len(), 1);
        let file = &outcome.files[0];
        assert_eq!(file.matches.len(), 1);
        assert_eq!(file.matches[0].line_number, 2);
        assert!(file.matches[0].line.contains("needle"));
        assert!(!outcome.truncated);
        assert_eq!(outcome.matcher, MatcherKind::Regex);
    }

    #[test]
    fn per_file_cap_truncates_and_flags_outcome() {
        let dir = TempDir::new().unwrap();
        let content = "needle\n".repeat(5);
        write_file(&dir, "many.txt", &content);
        let roots = [dir.path().to_path_buf()];

        let mut req = request("needle", &roots);
        req.limits.max_matches_per_file = 2;
        let outcome = grep(&req).unwrap();

        assert_eq!(outcome.files.len(), 1);
        let file = &outcome.files[0];
        assert_eq!(file.matches.len(), 2);
        assert!(file.truncated);
        assert!(outcome.truncated);
    }

    #[test]
    fn zero_hit_files_do_not_consume_the_result_file_quota() {
        let dir = TempDir::new().unwrap();
        // 25 个不含 pattern 的文件；默认 `max_files == 20`，如果零命中文件错误地占用配额，
        // 遍历会在找到下面这条真正的命中之前就被 `should_stop` 拦停。
        for i in 0..25 {
            write_file(
                &dir,
                &format!("noise_{i:02}.txt"),
                "nothing interesting here\n",
            );
        }
        // 文件名保证字典序排在所有 noise_* 之后，即便遍历顺序恰好是排序的也不会侥幸命中。
        write_file(&dir, "zzz_target.txt", "needle\n");
        let roots = [dir.path().to_path_buf()];

        let outcome = grep(&request("needle", &roots)).unwrap();

        assert_eq!(outcome.files.len(), 1);
        assert_eq!(outcome.files[0].path.file_name().unwrap(), "zzz_target.txt");
        assert!(
            outcome.files_searched >= 26,
            "files_searched = {}",
            outcome.files_searched
        );
        assert!(!outcome.truncated);
    }

    #[test]
    fn invalid_regex_degrades_to_literal_match() {
        let dir = TempDir::new().unwrap();
        write_file(&dir, "lit.txt", "call foo(unclosed\n");
        let roots = [dir.path().to_path_buf()];

        // 未配对的左括号不是合法正则语法。
        let outcome = grep(&request("foo(unclosed", &roots)).unwrap();

        assert_eq!(outcome.matcher, MatcherKind::Literal);
        assert_eq!(outcome.files.len(), 1);
        assert_eq!(outcome.files[0].matches.len(), 1);
    }

    #[test]
    fn nul_byte_stops_read_and_is_counted_as_binary() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("binary.dat");
        std::fs::write(&path, b"before\x00needle-after").unwrap();
        let roots = [dir.path().to_path_buf()];

        let outcome = grep(&request("before", &roots)).unwrap();

        assert_eq!(outcome.files_searched, 1);
        assert_eq!(outcome.stopped_on_binary, 1);
    }

    #[test]
    fn gitignore_hides_file_unless_disabled() {
        let dir = TempDir::new().unwrap();
        write_file(&dir, ".gitignore", "ignored.txt\n");
        write_file(&dir, "ignored.txt", "needle\n");
        write_file(&dir, "kept.txt", "needle\n");
        let roots = [dir.path().to_path_buf()];

        let respecting = grep(&request("needle", &roots)).unwrap();
        let names: Vec<_> = respecting
            .files
            .iter()
            .map(|f| f.path.file_name().unwrap().to_owned())
            .collect();
        assert!(names.iter().any(|n| n == "kept.txt"));
        assert!(!names.iter().any(|n| n == "ignored.txt"));

        let mut ignoring = request("needle", &roots);
        ignoring.respect_gitignore = false;
        let outcome = grep(&ignoring).unwrap();
        assert_eq!(outcome.files.len(), 2);
    }

    #[test]
    fn hidden_files_are_searched_by_default() {
        let dir = TempDir::new().unwrap();
        write_file(&dir, ".env.example", "needle\n");
        let roots = [dir.path().to_path_buf()];

        let outcome = grep(&request("needle", &roots)).unwrap();

        assert_eq!(outcome.files.len(), 1);
    }

    #[test]
    fn pre_set_cancel_stops_before_any_file_is_searched() {
        let dir = TempDir::new().unwrap();
        for i in 0..5 {
            write_file(&dir, &format!("f{i}.txt"), "needle\n");
        }
        let roots = [dir.path().to_path_buf()];
        let cancel = AtomicBool::new(true);

        let mut req = request("needle", &roots);
        req.cancel = Some(&cancel);
        let outcome = grep(&req).unwrap();

        assert_eq!(outcome.files_searched, 0);
        assert_eq!(outcome.total_matches, 0);
        assert!(outcome.truncated);
    }

    #[test]
    fn long_cjk_line_is_truncated_by_display_width() {
        let dir = TempDir::new().unwrap();
        let cjk_tail = "中".repeat(40); // 每个字符显示宽度 2，共 80 列。
        let content = format!("needle {cjk_tail}\n");
        write_file(&dir, "wide.txt", &content);
        let roots = [dir.path().to_path_buf()];

        let mut req = request("needle", &roots);
        req.limits.max_line_columns = 20;
        let outcome = grep(&req).unwrap();

        let line = &outcome.files[0].matches[0].line;
        assert!(visible_width(line) <= 20);
        assert!(line.len() < content.len());
    }

    #[test]
    fn empty_pattern_is_rejected() {
        let dir = TempDir::new().unwrap();
        let roots = [dir.path().to_path_buf()];
        let err = grep(&request("", &roots)).unwrap_err();
        assert!(matches!(err, GrepError::EmptyPattern));
    }
}
