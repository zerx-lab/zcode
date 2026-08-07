//! `todo` 工具：会话内的结构化、分阶段任务清单。
//!
//! # 状态存哪：落盘（方案 b），不是进程内 `Mutex<HashMap<SessionId, _>>`
//!
//! 两个选项都能让同一会话内的多次 `todo` 调用看到彼此的写入：
//! - (a) 进程内 `Mutex<HashMap<SessionId, TodoList>>`：daemon 重启（升级、崩溃恢复）
//!   或客户端断线重连后状态直接丢失，而 todo 清单往往横跨整个 turn 甚至多个 turn。
//! - (b) 落盘到 `<session_dir>/<session_id>.todo.md`：与会话本身的 JSONL 事实来源同生命周期，
//!   重连、重启都不丢；而且 Markdown 往返（见下）本来就是为落盘设计的——序列化格式已经
//!   兼顾了"人可读可编辑"，选它不需要再多发明一套内存结构的持久化。
//!
//! 选 (b)。构造函数因此只收一个 `session_dir: PathBuf`（其余七个工具走
//! `XxxTool::new(workspace, &config.tools)` 的统一形状；todo 不解析路径、不读工具级配置，
//! 没有理由塞两个用不上的参数进来，见 `tools/mod.rs` 里对 `todo` 的构造调用）。
//!
//! # 并发档位：`Concurrency::Shared` + 内部锁，不是 oh-my-pi 的 `Exclusive`
//!
//! oh-my-pi 把 `todo` 声明成 `exclusive`（`packages/coding-agent/src/tools/todo.ts:771`），
//! 但它自己也承认这会挡住同一批次里其他只读工具（`read`/`grep`）——todo 只碰自己的状态
//! 文件，不碰用户源码，没有理由让它成为整批调用的屏障。本实现选 [`Concurrency::Shared`]，
//! 真正需要互斥的只有"同一个 `TodoTool` 实例并发处理同一份状态文件的读-改-写"这一件事，
//! 用一把进程内的 [`tokio::sync::Mutex`] 就够，不需要把无关工具也拖成串行。
//!
//! # 渲染边界
//!
//! task 内容与 phase 名既是寻址用的身份键，也是要持久化的原始内容：任何变换（截断、清洗
//! 控制字符）都会让下一次按内容定位任务失配，或让 Markdown 往返产生偏差。所以本文件内部
//! 一律搬运原始字符串，唯一的展示期清理发生在 [`crate::tools::output::finish`]——工具输出
//! 离开这个文件之前的最后一步。

use std::fmt;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use regex::Regex;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::Mutex as AsyncMutex;

use zcode_agent::{ApprovalDecision, Concurrency, Tier, Tool, ToolContext, ToolError, ToolOutput};

use crate::tools::output;

/// 工具对模型的说明。铁律 7：不做运行时字符串拼接，静态嵌入。
const DESCRIPTION: &str = include_str!("prompts/todo.md");

/// 单个任务的状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TodoStatus {
    /// 待办，尚未开始。
    Pending,
    /// 正在进行。同一份清单内至多一个，见 [`normalize_in_progress`]。
    InProgress,
    /// 已完成。
    Completed,
    /// 已放弃。
    Abandoned,
    /// 被阻塞：开放但等待清单外部的东西，见 [`TodoItem::blocker`]。
    Blocked,
}

impl TodoStatus {
    /// Markdown checklist 里的单字符标记。写侧唯一入口，读侧见 [`marker_to_status`]。
    fn marker(self) -> char {
        match self {
            Self::Pending => ' ',
            Self::InProgress => '/',
            Self::Completed => 'x',
            Self::Abandoned => '-',
            Self::Blocked => '!',
        }
    }
}

/// Markdown checklist 标记 → 状态。接受写侧未产出但人工编辑常见的同义标记
/// （`X`/`>`/`~`），抄自 oh-my-pi `packages/coding-agent/src/tools/todo.ts:623-633`：
/// 导入外部编辑过的清单时不该因为大小写或符号习惯就报废整份文件。
fn marker_to_status(marker: &str) -> Option<TodoStatus> {
    match marker {
        " " | "" => Some(TodoStatus::Pending),
        "x" | "X" => Some(TodoStatus::Completed),
        "/" | ">" => Some(TodoStatus::InProgress),
        "-" | "~" => Some(TodoStatus::Abandoned),
        "!" => Some(TodoStatus::Blocked),
        _ => None,
    }
}

/// 一个任务。
#[derive(Debug, Clone)]
struct TodoItem {
    /// 任务内容，兼作寻址用的身份键——见模块文档「渲染边界」。
    content: String,
    /// 当前状态。
    status: TodoStatus,
    /// `status == Blocked` 时的可选说明。已在写入前折叠空白，见 [`normalize_blocker`]。
    blocker: Option<String>,
}

/// 一个阶段，包含若干任务。
#[derive(Debug, Clone)]
struct TodoPhase {
    /// 阶段名，兼作寻址用的身份键。
    name: String,
    /// 阶段内的任务，按创建顺序排列。
    tasks: Vec<TodoItem>,
}

/// 工具接受的操作名。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum TodoOp {
    /// 用一份全新的分阶段计划替换整个清单。
    Init,
    /// 把某个任务标为进行中。
    Start,
    /// 把目标标为已完成。
    Done,
    /// 把目标标为已放弃。
    Drop,
    /// 把目标标为阻塞。
    Block,
    /// 把阻塞的目标放回待办。
    Unblock,
    /// 删除目标。
    Rm,
    /// 向某个阶段追加任务。
    Append,
    /// 只读：回显当前清单，不产生任何写入。
    View,
}

impl fmt::Display for TodoOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::Init => "init",
            Self::Start => "start",
            Self::Done => "done",
            Self::Drop => "drop",
            Self::Block => "block",
            Self::Unblock => "unblock",
            Self::Rm => "rm",
            Self::Append => "append",
            Self::View => "view",
        };
        f.write_str(text)
    }
}

/// `init` 的 `list` 数组条目：一个阶段名 + 该阶段下的任务内容。
#[derive(Debug, Clone, Deserialize)]
struct InitListEntry {
    phase: String,
    items: Vec<String>,
}

/// 工具入参。`op` 必填，其余按操作各自可选——校验在 [`apply_entry`] 里按操作分别做，
/// 不在 schema 层加 schema 级"至少一项"之类约束（见 [`parameters_schema`] 顶部注释）。
#[derive(Debug, Clone, Deserialize)]
struct TodoArgs {
    op: TodoOp,
    #[serde(default)]
    list: Option<Vec<InitListEntry>>,
    #[serde(default)]
    task: Option<String>,
    #[serde(default)]
    phase: Option<String>,
    #[serde(default)]
    items: Option<Vec<String>>,
    #[serde(default)]
    reason: Option<String>,
}

/// `init` 在只给了扁平 `items`（没有 `list`）时使用的默认阶段名。
///
/// 出处 oh-my-pi `packages/coding-agent/src/tools/todo.ts:362`；上游未给出这个具体名字
/// 之外的额外依据，纯粹是一个可读的默认值。
const DEFAULT_INIT_PHASE: &str = "Tasks";

/// 参数的 JSON Schema。
fn parameters_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "op": {
                "type": "string",
                "enum": ["init", "start", "done", "drop", "block", "unblock", "rm", "append", "view"],
                "description": "operation to apply"
            },
            "list": {
                "type": "array",
                "description": "phased task list (init)",
                "items": {
                    "type": "object",
                    "properties": {
                        "phase": { "type": "string", "description": "phase name" },
                        "items": {
                            "type": "array",
                            "items": { "type": "string" },
                            "minItems": 1,
                            "description": "tasks for this phase"
                        }
                    },
                    "required": ["phase", "items"],
                    "additionalProperties": false
                }
            },
            "task": { "type": "string", "description": "task content" },
            "phase": { "type": "string", "description": "phase name" },
            "items": {
                "type": "array",
                "items": { "type": "string" },
                "description": "tasks to append"
            },
            "reason": { "type": "string", "description": "blocker note (block op)" }
        },
        "required": ["op"],
        "additionalProperties": false
    })
}

/// 判断 `content` 是不是形如 `task-123` 的自动编号——模型偶尔会把上一次结果里的展示序号
/// 当成 id 传回来，这种情况下的"未找到"提示要专门纠正这个误解。
/// 镜像 oh-my-pi `packages/coding-agent/src/tools/todo.ts:326` 的 `/^task-\d+$/`。
fn looks_like_task_id(content: &str) -> bool {
    content
        .strip_prefix("task-")
        .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
}

/// 按内容在全部阶段里查找任务，报告一条模型读得懂的定位失败说明。命中返回 `true`。
fn validate_task(phases: &[TodoPhase], content: &str, errors: &mut Vec<String>) -> bool {
    if phases
        .iter()
        .any(|phase| phase.tasks.iter().any(|task| task.content == content))
    {
        return true;
    }
    if looks_like_task_id(content) {
        errors.push(format!(
            "Task \"{content}\" not found. Tasks are referenced by content, not by IDs — \
             pass the task's full text from the previous result."
        ));
    } else {
        let total: usize = phases.iter().map(|phase| phase.tasks.len()).sum();
        let hint = if total == 0 {
            " (todo list is empty — was it replaced or not yet created?)"
        } else {
            ""
        };
        errors.push(format!("Task \"{content}\" not found{hint}"));
    }
    false
}

/// 按名字查找阶段，命中返回 `true`，否则报错。
fn validate_phase(phases: &[TodoPhase], name: &str, errors: &mut Vec<String>) -> bool {
    if phases.iter().any(|phase| phase.name == name) {
        true
    } else {
        errors.push(format!("Phase \"{name}\" not found"));
        false
    }
}

/// 校验 `task`/`phase` 定位参数：`task` 优先，其次 `phase`，两者都缺省表示"作用于全部任务"
/// （由调用方决定是否允许这种缺省——`block`/`unblock` 要求必须给一个）。
fn validate_target(
    phases: &[TodoPhase],
    task: Option<&str>,
    phase: Option<&str>,
    errors: &mut Vec<String>,
) -> bool {
    if let Some(content) = task {
        return validate_task(phases, content, errors);
    }
    if let Some(name) = phase {
        return validate_phase(phases, name, errors);
    }
    true
}

/// 按内容查找单个任务的可变引用，全部阶段里搜索第一个匹配。
fn find_task_mut<'a>(phases: &'a mut [TodoPhase], content: &str) -> Option<&'a mut TodoItem> {
    phases
        .iter_mut()
        .find_map(|phase| phase.tasks.iter_mut().find(|task| task.content == content))
}

/// 按 `task`/`phase`/全部 解析出这次操作要处理的任务集合（可变引用）。
/// 假设定位参数已经过 [`validate_target`]：这里不再报错，找不到就返回空集合。
fn resolve_targets_mut<'a>(
    phases: &'a mut [TodoPhase],
    task: Option<&str>,
    phase: Option<&str>,
) -> Vec<&'a mut TodoItem> {
    if let Some(content) = task {
        return find_task_mut(phases, content).into_iter().collect();
    }
    if let Some(name) = phase {
        return match phases.iter_mut().find(|phase| phase.name == name) {
            Some(phase) => phase.tasks.iter_mut().collect(),
            None => Vec::new(),
        };
    }
    phases
        .iter_mut()
        .flat_map(|phase| phase.tasks.iter_mut())
        .collect()
}

/// 折叠 blocker 说明里的空白游程（含换行）为单个空格并去首尾空白；空字符串归一成 `None`。
///
/// 出处与理由 oh-my-pi `packages/coding-agent/src/tools/todo.ts:487-492`：blocker 要塞进
/// Markdown checklist 一行的尾部注释与一行 HUD 摘要，嵌入的换行会破坏这两处的单行假设，
/// 在写入前统一折叠比让每个消费者各自防御更省心。
fn normalize_blocker(reason: Option<&str>) -> Option<String> {
    let collapsed = reason?.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        None
    } else {
        Some(collapsed)
    }
}

/// `init`：用全新的分阶段计划替换整个清单。
///
/// 接受两种入参形状：规范的 `list: [{phase, items}]`，或扁平的 `items`（可选 `phase`）——
/// 后者会被合成为单阶段的 `list`，因为模型经常把单阶段 init 写成扁平形式
/// （oh-my-pi `todo.ts:365-373`）。
fn init_phases(
    list: Option<&Vec<InitListEntry>>,
    items: Option<&Vec<String>>,
    phase: Option<&str>,
    errors: &mut Vec<String>,
) -> Vec<TodoPhase> {
    let synthesized;
    let list: &[InitListEntry] = match list {
        Some(entries) if !entries.is_empty() => entries,
        _ => match items {
            Some(flat) if !flat.is_empty() => {
                synthesized = [InitListEntry {
                    phase: phase.unwrap_or(DEFAULT_INIT_PHASE).to_owned(),
                    items: flat.clone(),
                }];
                &synthesized
            }
            _ => {
                errors.push("Missing list for init operation".to_owned());
                return Vec::new();
            }
        },
    };

    // 重复的阶段名/任务内容会让后续按内容定位的操作永久失配——寻址键唯一性在 init 时就要保证。
    let mut seen_phases = std::collections::HashSet::new();
    let mut seen_tasks = std::collections::HashSet::new();
    for entry in list {
        if !seen_phases.insert(entry.phase.as_str()) {
            errors.push(format!("Duplicate phase \"{}\" in init list", entry.phase));
        }
        for content in &entry.items {
            if !seen_tasks.insert(content.as_str()) {
                errors.push(format!("Duplicate task \"{content}\" in init list"));
            }
        }
    }

    list.iter()
        .map(|entry| TodoPhase {
            name: entry.phase.clone(),
            tasks: entry
                .items
                .iter()
                .map(|content| TodoItem {
                    content: content.clone(),
                    status: TodoStatus::Pending,
                    blocker: None,
                })
                .collect(),
        })
        .collect()
}

/// `start`：把恰好一个任务标为进行中，同批次里其余进行中的任务降回待办。
fn start_task(phases: &mut [TodoPhase], task: Option<&str>, errors: &mut Vec<String>) {
    let Some(content) = task else {
        errors.push("Missing task content".to_owned());
        return;
    };
    if !validate_task(phases, content, errors) {
        return;
    }
    for phase in phases.iter_mut() {
        for candidate in &mut phase.tasks {
            if candidate.status == TodoStatus::InProgress && candidate.content != content {
                candidate.status = TodoStatus::Pending;
            }
        }
    }
    if let Some(item) = find_task_mut(phases, content) {
        item.status = TodoStatus::InProgress;
    }
}

/// `done`/`drop` 共用：把定位到的目标（单任务/整阶段/全部）统一置成给定状态，无资格过滤。
fn set_status_on_targets(
    phases: &mut [TodoPhase],
    task: Option<&str>,
    phase: Option<&str>,
    status: TodoStatus,
    errors: &mut Vec<String>,
) {
    if !validate_target(phases, task, phase, errors) {
        return;
    }
    for item in resolve_targets_mut(phases, task, phase) {
        item.status = status;
    }
}

/// `block`：要求显式给 `task` 或 `phase`（不接受"全部"这种缺省）。
///
/// 只作用于开放工作：`completed`/`abandoned` 的任务不会被拉回阻塞状态；已经是 `blocked`
/// 的任务仍然是合法目标，允许用一次新的 `block` 调用替换它的 `reason`
/// （oh-my-pi `todo.ts:493-499`）。
fn block_targets(
    phases: &mut [TodoPhase],
    task: Option<&str>,
    phase: Option<&str>,
    reason: Option<&str>,
    errors: &mut Vec<String>,
) {
    if task.is_none() && phase.is_none() {
        errors.push("block requires a task or phase target".to_owned());
        return;
    }
    if !validate_target(phases, task, phase, errors) {
        return;
    }
    let blocker = normalize_blocker(reason);
    for item in resolve_targets_mut(phases, task, phase) {
        if matches!(
            item.status,
            TodoStatus::Pending | TodoStatus::InProgress | TodoStatus::Blocked
        ) {
            item.status = TodoStatus::Blocked;
            item.blocker.clone_from(&blocker);
        }
    }
}

/// `unblock`：同样要求显式目标；只有当前 `blocked` 的任务会被放回 `pending`。
fn unblock_targets(
    phases: &mut [TodoPhase],
    task: Option<&str>,
    phase: Option<&str>,
    errors: &mut Vec<String>,
) {
    if task.is_none() && phase.is_none() {
        errors.push("unblock requires a task or phase target".to_owned());
        return;
    }
    if !validate_target(phases, task, phase, errors) {
        return;
    }
    for item in resolve_targets_mut(phases, task, phase) {
        if item.status == TodoStatus::Blocked {
            item.status = TodoStatus::Pending;
            item.blocker = None;
        }
    }
}

/// `rm`：`task` 删单个任务；`phase` 清空该阶段；都不给则清空全部阶段的任务。
fn remove_tasks(
    phases: &mut [TodoPhase],
    task: Option<&str>,
    phase: Option<&str>,
    errors: &mut Vec<String>,
) {
    if let Some(content) = task {
        if !validate_task(phases, content, errors) {
            return;
        }
        for phase in phases.iter_mut() {
            phase.tasks.retain(|candidate| candidate.content != content);
        }
        return;
    }
    if let Some(name) = phase {
        if !validate_phase(phases, name, errors) {
            return;
        }
        if let Some(phase) = phases.iter_mut().find(|phase| phase.name == name) {
            phase.tasks.clear();
        }
        return;
    }
    for phase in phases.iter_mut() {
        phase.tasks.clear();
    }
}

/// `append`：向一个阶段追加任务，阶段不存在就惰性创建。整批 `items` 先校验再落地——
/// 任何一个内容重复（批内重复或与既有任务撞名）都会让整次调用不产生任何变更。
fn append_items(
    phases: &mut Vec<TodoPhase>,
    phase: Option<&str>,
    items: Option<&Vec<String>>,
    errors: &mut Vec<String>,
) {
    let Some(phase_name) = phase else {
        errors.push("Missing phase name for append operation".to_owned());
        return;
    };
    let items = match items {
        Some(values) if !values.is_empty() => values,
        _ => {
            errors.push("Missing items for append operation".to_owned());
            return;
        }
    };

    let mut seen = std::collections::HashSet::new();
    let mut has_duplicate = false;
    for content in items {
        let exists_elsewhere = phases
            .iter()
            .any(|phase| phase.tasks.iter().any(|task| &task.content == content));
        if !seen.insert(content.as_str()) || exists_elsewhere {
            errors.push(format!("Task \"{content}\" already exists"));
            has_duplicate = true;
        }
    }
    if has_duplicate {
        return;
    }

    let idx = if let Some(idx) = phases.iter().position(|phase| phase.name == phase_name) {
        idx
    } else {
        phases.push(TodoPhase {
            name: phase_name.to_owned(),
            tasks: Vec::new(),
        });
        phases.len().saturating_sub(1)
    };
    if let Some(phase) = phases.get_mut(idx) {
        for content in items {
            phase.tasks.push(TodoItem {
                content: content.clone(),
                status: TodoStatus::Pending,
                blocker: None,
            });
        }
    }
}

/// 单例归一：`in_progress` 多于一个时，除第一个外全部降回 `pending`；一个都没有时把第一个
/// `pending` 提成 `in_progress`。每次非只读操作后都要跑一遍，抄自
/// oh-my-pi `packages/coding-agent/src/tools/todo.ts:146-161`。
fn normalize_in_progress(phases: &mut [TodoPhase]) {
    let mut seen_in_progress = false;
    for phase in phases.iter_mut() {
        for task in &mut phase.tasks {
            if task.status == TodoStatus::InProgress {
                if seen_in_progress {
                    task.status = TodoStatus::Pending;
                } else {
                    seen_in_progress = true;
                }
            }
        }
    }
    if seen_in_progress {
        return;
    }
    for phase in phases.iter_mut() {
        if let Some(task) = phase
            .tasks
            .iter_mut()
            .find(|task| task.status == TodoStatus::Pending)
        {
            task.status = TodoStatus::InProgress;
            return;
        }
    }
}

/// 按 `args.op` 分派到具体的应用函数，把产生的错误收进 `errors`。
/// `view` 是恒等函数：不产生任何变更也不产生错误。
fn apply_entry(
    mut phases: Vec<TodoPhase>,
    args: &TodoArgs,
    errors: &mut Vec<String>,
) -> Vec<TodoPhase> {
    match args.op {
        TodoOp::Init => init_phases(
            args.list.as_ref(),
            args.items.as_ref(),
            args.phase.as_deref(),
            errors,
        ),
        TodoOp::Start => {
            start_task(&mut phases, args.task.as_deref(), errors);
            phases
        }
        TodoOp::Done => {
            set_status_on_targets(
                &mut phases,
                args.task.as_deref(),
                args.phase.as_deref(),
                TodoStatus::Completed,
                errors,
            );
            phases
        }
        TodoOp::Drop => {
            set_status_on_targets(
                &mut phases,
                args.task.as_deref(),
                args.phase.as_deref(),
                TodoStatus::Abandoned,
                errors,
            );
            phases
        }
        TodoOp::Block => {
            block_targets(
                &mut phases,
                args.task.as_deref(),
                args.phase.as_deref(),
                args.reason.as_deref(),
                errors,
            );
            phases
        }
        TodoOp::Unblock => {
            unblock_targets(
                &mut phases,
                args.task.as_deref(),
                args.phase.as_deref(),
                errors,
            );
            phases
        }
        TodoOp::Rm => {
            remove_tasks(
                &mut phases,
                args.task.as_deref(),
                args.phase.as_deref(),
                errors,
            );
            phases
        }
        TodoOp::Append => {
            append_items(
                &mut phases,
                args.phase.as_deref(),
                args.items.as_ref(),
                errors,
            );
            phases
        }
        TodoOp::View => phases,
    }
}

// ============================================================================
// Markdown 往返
// ============================================================================
//
// 存储格式选 Markdown checklist 而不是 JSON：状态文件本身要能被人直接打开阅读/编辑
// （`/todo edit` 这类交互式场景，以及故障排查时手工修复），JSON 做不到这一点还要多一层
// 转换。核心选型：
// - 状态标记 `{pending:" ", in_progress:"/", completed:"x", abandoned:"-", blocked:"!"}`
//   全部落在渲染后仍然可见的 checklist 方括号里，肉眼可读；
// - blocker 说明落在 `<!-- blocker: ... -->` 尾部 HTML 注释——多数 Markdown 渲染器会隐藏
//   HTML 注释，视觉上不污染 checklist，解析时用固定前后缀锚定，任务内容本身不可能包含
//   `<!--`/`-->` 分隔符（它们不是合法的任务内容，见 `normalize_blocker` 对写入路径的折叠），
//   所以解析是明确的，也扛得住导入/导出（先 `phases_to_markdown` 再 `markdown_to_phases`
//   得到相同结构）。
// 全部抄自 oh-my-pi `packages/coding-agent/src/tools/todo.ts:591-684`。

/// 把阶段渲染成 Markdown checklist。阶段用一级标题分隔，阶段之间空一行。
fn phases_to_markdown(phases: &[TodoPhase]) -> String {
    if phases.is_empty() {
        return "# Todos\n".to_owned();
    }
    let mut out = String::new();
    for (idx, phase) in phases.iter().enumerate() {
        if idx > 0 {
            out.push('\n');
        }
        out.push_str("# ");
        out.push_str(&phase.name);
        out.push('\n');
        for task in &phase.tasks {
            out.push_str("- [");
            out.push(task.status.marker());
            out.push_str("] ");
            out.push_str(&task.content);
            if task.status == TodoStatus::Blocked
                && let Some(blocker) = &task.blocker
            {
                out.push_str(" <!-- blocker: ");
                out.push_str(blocker);
                out.push_str(" -->");
            }
            out.push('\n');
        }
    }
    out
}

/// 解析形如 `^#{1,6}\s+(.+)$` 的一级到六级标题行；要求井号后至少一个空白，标题非空。
fn parse_heading(trimmed: &str) -> Option<String> {
    let hashes = trimmed.chars().take_while(|&ch| ch == '#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let rest = trimmed.get(hashes..)?;
    let after_ws = rest.trim_start();
    if after_ws.len() == rest.len() {
        return None;
    }
    let title = after_ws.trim();
    if title.is_empty() {
        None
    } else {
        Some(title.to_owned())
    }
}

/// checklist 行的正则：`- [x] content` / `* [ ] content` / `+ [!] content`。
/// 只编译一次，供 [`parse_checklist`] 复用。
static CHECKLIST_LINE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();

/// 解析一行 checklist：返回 `(标记, 内容)`；不匹配返回 `None`（调用方按"无法识别的语法"处理）。
fn parse_checklist(trimmed: &str) -> Option<(String, String)> {
    let re = CHECKLIST_LINE.get_or_init(build_checklist_regex);
    let captures = re.captures(trimmed)?;
    let marker = captures.get(1).map_or("", |m| m.as_str()).to_owned();
    let content = captures.get(2)?.as_str().trim().to_owned();
    if content.is_empty() {
        None
    } else {
        Some((marker, content))
    }
}

/// `parse_checklist` 用到的正则字面量在编译期已知合法（覆盖不到失败分支，也没有可失败的
/// 外部输入），构造失败在实践中不可达；即便如此也不用 `.expect`——用 `unreachable!` 会把
/// "库代码不许 panic"这条铁律留一个理论缺口，改成返回一个恒不匹配的正则，让调用方的
/// `.captures()` 稳定返回 `None`，行为上退化为"这一行按无法识别处理"而不是让整个工具崩溃。
fn build_checklist_regex() -> Regex {
    Regex::new(r"^[-*+]\s*\[(.?)\]\s+(.+?)\s*$").unwrap_or_else(|_| {
        // 退化正则：任何字符串都不可能匹配空字符集里的字符类，等价于"永不匹配"。
        #[allow(
            clippy::unwrap_used,
            reason = "退化正则本身是字面量常量，不依赖外部输入"
        )]
        Regex::new(r"[^\s\S]").unwrap()
    })
}

/// 还原写入路径塞进内容尾部的 `<!-- blocker: ... -->` 注释，返回 `(去注释内容, 说明)`。
fn strip_blocker_comment(content: &str) -> (String, Option<String>) {
    let trimmed = content.trim_end();
    let Some(without_suffix) = trimmed.strip_suffix("-->") else {
        return (content.trim().to_owned(), None);
    };
    let Some(start) = without_suffix.rfind("<!-- blocker:") else {
        return (content.trim().to_owned(), None);
    };
    let before = without_suffix
        .get(..start)
        .unwrap_or("")
        .trim_end()
        .to_owned();
    let reason = without_suffix
        .get(start + "<!-- blocker:".len()..)
        .unwrap_or("")
        .trim()
        .to_owned();
    (before, Some(reason))
}

/// 把 Markdown checklist 解析回阶段列表，同时收集每一行的解析问题（不阻断其余行的解析）。
/// 解析结束后统一跑一遍 [`normalize_in_progress`]，让手工编辑出的"两个 `in_progress`"或
/// "全 pending"也能被拉回单例状态。
fn markdown_to_phases(markdown: &str) -> (Vec<TodoPhase>, Vec<String>) {
    let mut errors = Vec::new();
    let mut phases: Vec<TodoPhase> = Vec::new();
    for (idx, raw) in markdown.lines().enumerate() {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(title) = parse_heading(trimmed) {
            phases.push(TodoPhase {
                name: title,
                tasks: Vec::new(),
            });
            continue;
        }
        if let Some((marker, content)) = parse_checklist(trimmed) {
            let Some(status) = marker_to_status(&marker) else {
                errors.push(format!(
                    "Line {}: unknown status marker \"[{marker}]\" (use [ ], [x], [/], [-], [!])",
                    idx + 1
                ));
                continue;
            };
            if phases.is_empty() {
                phases.push(TodoPhase {
                    name: "Todos".to_owned(),
                    tasks: Vec::new(),
                });
            }
            let (text, blocker) = if status == TodoStatus::Blocked {
                strip_blocker_comment(&content)
            } else {
                (content, None)
            };
            if let Some(phase) = phases.last_mut() {
                phase.tasks.push(TodoItem {
                    content: text,
                    status,
                    blocker,
                });
            }
            continue;
        }
        errors.push(format!(
            "Line {}: unrecognized syntax \"{trimmed}\"",
            idx + 1
        ));
    }
    normalize_in_progress(&mut phases);
    (phases, errors)
}

// ============================================================================
// 落盘
// ============================================================================

/// 会话 todo 状态文件的完整路径：`<session_dir>/<session_id>.todo.md`。
fn state_path(session_dir: &Path, session_id: &zcode_agent::SessionId) -> PathBuf {
    session_dir.join(format!("{}.todo.md", session_id.as_str()))
}

/// 读出会话当前的 todo 状态；文件不存在视为空清单（尚未 `init` 过）。
///
/// 状态文件只由本工具自己写，正常运行下不会出现解析错误；万一被手工改坏，选择"尽力解析、
/// 跳过看不懂的行"而不是硬失败——一次外部改动不该让模型连 `view` 都调不通，无法解析的行
/// 只记一条 `tracing::warn!`，可疑内容仍然能通过下一次 `init` 覆盖修复。
async fn load_phases(path: &Path) -> Result<Vec<TodoPhase>, ToolError> {
    match tokio::fs::read_to_string(path).await {
        Ok(text) => {
            let (phases, errors) = markdown_to_phases(&text);
            if !errors.is_empty() {
                tracing::warn!(path = %path.display(), issues = ?errors, "todo 状态文件存在无法解析的行，已跳过");
            }
            Ok(phases)
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(err) => Err(output::error(format!("Failed to read todo state: {err}"))),
    }
}

/// 把当前状态整体覆盖写回会话的 todo 状态文件。
async fn save_phases(path: &Path, phases: &[TodoPhase]) -> Result<(), ToolError> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|err| output::error(format!("Failed to create session directory: {err}")))?;
    }
    let markdown = phases_to_markdown(phases);
    tokio::fs::write(path, markdown)
        .await
        .map_err(|err| output::error(format!("Failed to persist todo state: {err}")))
}

// ============================================================================
// 面向模型的摘要文本
// ============================================================================

/// 汇总一行 UI 标题：`<op> (<done>/<total> done)`。
fn build_title(op: TodoOp, phases: &[TodoPhase]) -> String {
    let total: usize = phases.iter().map(|phase| phase.tasks.len()).sum();
    let done: usize = phases
        .iter()
        .flat_map(|phase| &phase.tasks)
        .filter(|task| matches!(task.status, TodoStatus::Completed | TodoStatus::Abandoned))
        .count();
    format!("todo {op} ({done}/{total} done)")
}

/// 汇总回给模型的正文：整体进度 + 逐阶段逐任务的清单。
fn summarize(phases: &[TodoPhase], read_only: bool) -> String {
    let total: usize = phases.iter().map(|phase| phase.tasks.len()).sum();
    if total == 0 {
        return if read_only {
            "Todo list is empty.".to_owned()
        } else {
            "Todo list cleared.".to_owned()
        };
    }

    let closed: usize = phases
        .iter()
        .flat_map(|phase| &phase.tasks)
        .filter(|task| matches!(task.status, TodoStatus::Completed | TodoStatus::Abandoned))
        .count();
    let blocked: usize = phases
        .iter()
        .flat_map(|phase| &phase.tasks)
        .filter(|task| task.status == TodoStatus::Blocked)
        .count();
    let open = total.saturating_sub(closed);

    let mut lines = Vec::new();
    let blocked_suffix = if blocked > 0 {
        format!(", {blocked} blocked")
    } else {
        String::new()
    };
    lines.push(format!(
        "Overall: {closed}/{total} done, {open} open{blocked_suffix}."
    ));

    for phase in phases {
        lines.push(format!("{}:", phase.name));
        for task in &phase.tasks {
            let marker = task.status.marker();
            let tag = match task.status {
                TodoStatus::InProgress => " (in progress)".to_owned(),
                TodoStatus::Abandoned => " (dropped)".to_owned(),
                TodoStatus::Blocked => match &task.blocker {
                    Some(reason) => format!(" (blocked: {reason})"),
                    None => " (blocked)".to_owned(),
                },
                TodoStatus::Pending | TodoStatus::Completed => String::new(),
            };
            lines.push(format!("  [{marker}] {}{tag}", task.content));
        }
    }
    lines.join("\n")
}

// ============================================================================
// 工具
// ============================================================================

/// `todo` 工具：会话内的结构化、分阶段任务清单。构造与并发选型见模块文档。
#[derive(Debug)]
pub(crate) struct TodoTool {
    /// 状态落盘目录；实际文件是 `<session_dir>/<session_id>.todo.md`。
    session_dir: PathBuf,
    /// 序列化对状态文件的读-改-写，配合 `Concurrency::Shared`——见模块文档「并发档位」。
    lock: AsyncMutex<()>,
}

impl TodoTool {
    /// 构造 todo 工具。`session_dir` 是会话状态目录（等于 `config.session.dir`）。
    pub(crate) fn new(session_dir: PathBuf) -> Self {
        Self {
            session_dir,
            lock: AsyncMutex::new(()),
        }
    }
}

#[async_trait]
impl Tool for TodoTool {
    fn name(&self) -> &'static str {
        "todo"
    }

    fn description(&self) -> &str {
        DESCRIPTION
    }

    fn parameters(&self) -> Value {
        parameters_schema()
    }

    fn approval(&self, _args: &Value) -> ApprovalDecision {
        // 只改自己的会话状态文件，不碰用户源码、不执行任何外部命令。
        ApprovalDecision::tier(Tier::Read)
    }

    fn concurrency(&self, _args: &Value) -> Concurrency {
        Concurrency::Shared
    }

    async fn execute(&self, args: Value, ctx: ToolContext) -> Result<ToolOutput, ToolError> {
        if ctx.cancel.is_set() {
            return Err(ToolError::Cancelled);
        }

        let args: TodoArgs = serde_json::from_value(args)
            .map_err(|err| output::error(format!("Invalid todo arguments: {err}")))?;

        let path = state_path(&self.session_dir, &ctx.session_id);
        let _guard = self.lock.lock().await;

        let previous = load_phases(&path).await?;
        let read_only = matches!(args.op, TodoOp::View);

        let mut errors = Vec::new();
        let candidate = if read_only {
            previous.clone()
        } else {
            let mut next = apply_entry(previous.clone(), &args, &mut errors);
            normalize_in_progress(&mut next);
            next
        };

        // 批次失败整体回滚：任一 op 出错，整批丢弃，状态与执行前逐字节相同。半成品落盘会让
        // 自然的重试在已落地的部分撞上"已存在"之类的二次错误——见模块文档引用的
        // oh-my-pi `todo.ts:858-864`。
        let failed = !errors.is_empty();
        let effective = if failed { previous } else { candidate };

        if !read_only && !failed {
            save_phases(&path, &effective).await?;
        }

        if failed {
            return Err(output::error(format!(
                "Todo operation failed: {}",
                errors.join("; ")
            )));
        }

        let title = build_title(args.op, &effective);
        let body = summarize(&effective, read_only);
        Ok(output::finish(body, title))
    }
}

#[cfg(test)]
mod tests {
    use tokio::sync::mpsc;

    use zcode_agent::{EntryId, InterruptSignal, SessionId, StoredToolResultContent};

    use super::*;

    fn make_tool(dir: &Path) -> TodoTool {
        TodoTool::new(dir.to_path_buf())
    }

    fn make_ctx(session_id: SessionId, cwd: &Path) -> ToolContext {
        let (tx, _rx) = mpsc::unbounded_channel();
        ToolContext {
            session_id,
            entry_id: EntryId::generate(),
            call_id: "call-1".to_owned(),
            cwd: cwd.to_path_buf(),
            cancel: InterruptSignal::new(),
            steering: InterruptSignal::new(),
            progress: tx,
        }
    }

    fn text_of(output: &ToolOutput) -> String {
        output
            .content
            .iter()
            .map(|block| match block {
                StoredToolResultContent::Text { text } => text.clone(),
                StoredToolResultContent::Image { .. } => String::new(),
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    async fn read_state(dir: &Path, session_id: &SessionId) -> String {
        tokio::fs::read_to_string(state_path(dir, session_id))
            .await
            .unwrap_or_default()
    }

    fn init_args() -> Value {
        json!({
            "op": "init",
            "list": [
                { "phase": "Foundation", "items": ["Scaffold crate", "Wire workspace"] },
                { "phase": "Verification", "items": ["Run cargo test"] },
            ],
        })
    }

    #[tokio::test]
    async fn init_creates_phased_list() {
        let dir = tempfile::tempdir().expect("tempdir");
        let tool = make_tool(dir.path());
        let session = SessionId::generate();

        let out = tool
            .execute(init_args(), make_ctx(session.clone(), dir.path()))
            .await
            .expect("init succeeds");
        let text = text_of(&out);
        assert!(text.contains("Foundation"));
        assert!(text.contains("Scaffold crate"));
        // 单例归一：清单里第一个 pending 任务自动提升为 in_progress。
        assert!(text.contains("[/] Scaffold crate"));

        let markdown = read_state(dir.path(), &session).await;
        assert!(markdown.contains("# Foundation"));
        assert!(markdown.contains("# Verification"));
    }

    #[tokio::test]
    async fn start_marks_single_task_in_progress() {
        let dir = tempfile::tempdir().expect("tempdir");
        let tool = make_tool(dir.path());
        let session = SessionId::generate();
        tool.execute(init_args(), make_ctx(session.clone(), dir.path()))
            .await
            .expect("init");

        let out = tool
            .execute(
                json!({"op": "start", "task": "Wire workspace"}),
                make_ctx(session.clone(), dir.path()),
            )
            .await
            .expect("start succeeds");
        let text = text_of(&out);
        assert!(text.contains("[/] Wire workspace"));
        assert!(text.contains("[ ] Scaffold crate"));
    }

    #[tokio::test]
    async fn done_marks_task_completed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let tool = make_tool(dir.path());
        let session = SessionId::generate();
        tool.execute(init_args(), make_ctx(session.clone(), dir.path()))
            .await
            .expect("init");

        let out = tool
            .execute(
                json!({"op": "done", "task": "Scaffold crate"}),
                make_ctx(session.clone(), dir.path()),
            )
            .await
            .expect("done succeeds");
        let text = text_of(&out);
        assert!(text.contains("[x] Scaffold crate"));
        // 完成一个任务后，下一个 pending 任务自动提升为 in_progress。
        assert!(text.contains("[/] Wire workspace"));
    }

    #[tokio::test]
    async fn drop_marks_task_abandoned() {
        let dir = tempfile::tempdir().expect("tempdir");
        let tool = make_tool(dir.path());
        let session = SessionId::generate();
        tool.execute(init_args(), make_ctx(session.clone(), dir.path()))
            .await
            .expect("init");

        let out = tool
            .execute(
                json!({"op": "drop", "task": "Run cargo test"}),
                make_ctx(session.clone(), dir.path()),
            )
            .await
            .expect("drop succeeds");
        assert!(text_of(&out).contains("[-] Run cargo test"));
    }

    #[tokio::test]
    async fn block_sets_status_and_reason() {
        let dir = tempfile::tempdir().expect("tempdir");
        let tool = make_tool(dir.path());
        let session = SessionId::generate();
        tool.execute(init_args(), make_ctx(session.clone(), dir.path()))
            .await
            .expect("init");

        let out = tool
            .execute(
                json!({"op": "block", "task": "Wire workspace", "reason": "waiting on design review"}),
                make_ctx(session.clone(), dir.path()),
            )
            .await
            .expect("block succeeds");
        let text = text_of(&out);
        assert!(text.contains("[!] Wire workspace"));
        assert!(text.contains("blocked: waiting on design review"));
    }

    #[tokio::test]
    async fn unblock_returns_task_to_pending() {
        let dir = tempfile::tempdir().expect("tempdir");
        let tool = make_tool(dir.path());
        let session = SessionId::generate();
        tool.execute(init_args(), make_ctx(session.clone(), dir.path()))
            .await
            .expect("init");
        tool.execute(
            json!({"op": "block", "task": "Wire workspace"}),
            make_ctx(session.clone(), dir.path()),
        )
        .await
        .expect("block");

        let out = tool
            .execute(
                json!({"op": "unblock", "task": "Wire workspace"}),
                make_ctx(session.clone(), dir.path()),
            )
            .await
            .expect("unblock succeeds");
        let text = text_of(&out);
        assert!(!text.contains("[!] Wire workspace"));
        assert!(text.contains("Wire workspace"));
    }

    #[tokio::test]
    async fn rm_removes_single_task() {
        let dir = tempfile::tempdir().expect("tempdir");
        let tool = make_tool(dir.path());
        let session = SessionId::generate();
        tool.execute(init_args(), make_ctx(session.clone(), dir.path()))
            .await
            .expect("init");

        let out = tool
            .execute(
                json!({"op": "rm", "task": "Scaffold crate"}),
                make_ctx(session.clone(), dir.path()),
            )
            .await
            .expect("rm succeeds");
        assert!(!text_of(&out).contains("Scaffold crate"));
    }

    #[tokio::test]
    async fn append_adds_tasks_to_existing_phase() {
        let dir = tempfile::tempdir().expect("tempdir");
        let tool = make_tool(dir.path());
        let session = SessionId::generate();
        tool.execute(init_args(), make_ctx(session.clone(), dir.path()))
            .await
            .expect("init");

        let out = tool
            .execute(
                json!({"op": "append", "phase": "Foundation", "items": ["Handle retries"]}),
                make_ctx(session.clone(), dir.path()),
            )
            .await
            .expect("append succeeds");
        assert!(text_of(&out).contains("Handle retries"));
    }

    #[tokio::test]
    async fn view_is_read_only_and_does_not_write_state() {
        let dir = tempfile::tempdir().expect("tempdir");
        let tool = make_tool(dir.path());
        let session = SessionId::generate();

        // 没有 init 过：view 在空清单上不该报错，也不该创建状态文件。
        let out = tool
            .execute(json!({"op": "view"}), make_ctx(session.clone(), dir.path()))
            .await
            .expect("view succeeds");
        assert_eq!(text_of(&out), "Todo list is empty.");
        assert!(!state_path(dir.path(), &session).exists());
    }

    #[tokio::test]
    async fn in_progress_singleton_collapses_extra_in_progress() {
        // 手工构造一份带两个 in_progress 的状态文件，模拟外部编辑或历史数据。
        let dir = tempfile::tempdir().expect("tempdir");
        let session = SessionId::generate();
        let path = state_path(dir.path(), &session);
        tokio::fs::write(&path, "# Phase\n- [/] one\n- [/] two\n- [ ] three\n")
            .await
            .expect("seed state");

        let tool = make_tool(dir.path());
        // `view` 只读，不会主动跑归一化，用 `done` 触发一次非只读操作观察结果。
        let out = tool
            .execute(
                json!({"op": "done", "task": "three"}),
                make_ctx(session.clone(), dir.path()),
            )
            .await
            .expect("done succeeds");
        let text = text_of(&out);
        assert_eq!(
            text.matches("[/]").count(),
            1,
            "至多一个 in_progress: {text}"
        );
        assert!(text.contains("[/] one"), "第一个 in_progress 保留: {text}");
        assert!(
            text.contains("[ ] two"),
            "第二个 in_progress 降回 pending: {text}"
        );
    }

    #[tokio::test]
    async fn in_progress_singleton_promotes_first_pending_when_none_active() {
        let dir = tempfile::tempdir().expect("tempdir");
        let tool = make_tool(dir.path());
        let session = SessionId::generate();
        tool.execute(
            json!({"op": "init", "list": [{"phase": "P", "items": ["a", "b"]}]}),
            make_ctx(session.clone(), dir.path()),
        )
        .await
        .expect("init");

        // 把唯一的 in_progress 任务标记完成，全部任务此刻都不是 in_progress……
        let out = tool
            .execute(
                json!({"op": "done", "task": "a"}),
                make_ctx(session.clone(), dir.path()),
            )
            .await
            .expect("done");
        let text = text_of(&out);
        // ……归一化应当把下一个 pending（"b"）提成 in_progress。
        assert!(text.contains("[/] b"), "应当自动提升下一个 pending: {text}");
    }

    #[tokio::test]
    async fn failed_batch_rolls_back_state_completely() {
        let dir = tempfile::tempdir().expect("tempdir");
        let tool = make_tool(dir.path());
        let session = SessionId::generate();
        tool.execute(init_args(), make_ctx(session.clone(), dir.path()))
            .await
            .expect("init");

        let before = read_state(dir.path(), &session).await;

        // 目标任务不存在：done 应当整体失败，不产生任何落盘变化。
        let err = tool
            .execute(
                json!({"op": "done", "task": "does not exist"}),
                make_ctx(session.clone(), dir.path()),
            )
            .await
            .expect_err("unresolvable target fails the call");
        assert!(matches!(err, ToolError::Failed(_)));

        let after = read_state(dir.path(), &session).await;
        assert_eq!(before, after, "失败的批次必须与执行前逐字节相同");
    }

    #[tokio::test]
    async fn append_batch_failure_does_not_partially_apply() {
        let dir = tempfile::tempdir().expect("tempdir");
        let tool = make_tool(dir.path());
        let session = SessionId::generate();
        tool.execute(init_args(), make_ctx(session.clone(), dir.path()))
            .await
            .expect("init");
        let before = read_state(dir.path(), &session).await;

        // "New task" 是新的，"Scaffold crate" 已存在——整批应当因为后者被拒绝，"New task" 也不落地。
        let err = tool
            .execute(
                json!({"op": "append", "phase": "Foundation", "items": ["New task", "Scaffold crate"]}),
                make_ctx(session.clone(), dir.path()),
            )
            .await
            .expect_err("duplicate in batch fails the whole call");
        assert!(matches!(err, ToolError::Failed(_)));

        let after = read_state(dir.path(), &session).await;
        assert_eq!(before, after);
    }

    #[tokio::test]
    async fn block_does_not_pull_back_completed_tasks() {
        let dir = tempfile::tempdir().expect("tempdir");
        let tool = make_tool(dir.path());
        let session = SessionId::generate();
        tool.execute(init_args(), make_ctx(session.clone(), dir.path()))
            .await
            .expect("init");
        tool.execute(
            json!({"op": "done", "task": "Scaffold crate"}),
            make_ctx(session.clone(), dir.path()),
        )
        .await
        .expect("done");

        // 对整个阶段 block：已完成的 "Scaffold crate" 不该被拉回 blocked。
        let out = tool
            .execute(
                json!({"op": "block", "phase": "Foundation", "reason": "paused"}),
                make_ctx(session.clone(), dir.path()),
            )
            .await
            .expect("block succeeds");
        let text = text_of(&out);
        assert!(
            text.contains("[x] Scaffold crate"),
            "已完成任务不应被 block 拉回: {text}"
        );
        assert!(
            text.contains("[!] Wire workspace"),
            "开放任务应当被 block: {text}"
        );
    }

    #[tokio::test]
    async fn block_can_reblock_already_blocked_task_to_update_reason() {
        let dir = tempfile::tempdir().expect("tempdir");
        let tool = make_tool(dir.path());
        let session = SessionId::generate();
        tool.execute(init_args(), make_ctx(session.clone(), dir.path()))
            .await
            .expect("init");
        tool.execute(
            json!({"op": "block", "task": "Wire workspace", "reason": "first reason"}),
            make_ctx(session.clone(), dir.path()),
        )
        .await
        .expect("first block");

        // 已经是 blocked 的任务仍是合法目标：第二次 block 用来替换 reason，不报错。
        let out = tool
            .execute(
                json!({"op": "block", "task": "Wire workspace", "reason": "updated reason"}),
                make_ctx(session.clone(), dir.path()),
            )
            .await
            .expect("re-block on an already-blocked task succeeds");
        let text = text_of(&out);
        assert!(
            text.contains("[!] Wire workspace"),
            "仍然是 blocked: {text}"
        );
        assert!(
            text.contains("blocked: updated reason"),
            "reason 应当被替换: {text}"
        );
        assert!(!text.contains("first reason"), "旧 reason 不应残留: {text}");
    }

    #[test]
    fn blocker_whitespace_is_collapsed() {
        assert_eq!(
            normalize_blocker(Some("waiting on\n  review\tplease")),
            Some("waiting on review please".to_owned())
        );
        assert_eq!(normalize_blocker(Some("   ")), None);
        assert_eq!(normalize_blocker(None), None);
    }

    #[test]
    fn markdown_round_trip_preserves_blocked_and_blocker() {
        let mut phases = vec![TodoPhase {
            name: "Foundation".to_owned(),
            tasks: vec![
                TodoItem {
                    content: "Scaffold crate".to_owned(),
                    status: TodoStatus::Completed,
                    blocker: None,
                },
                TodoItem {
                    content: "Wire workspace".to_owned(),
                    status: TodoStatus::Blocked,
                    blocker: Some("waiting on design review".to_owned()),
                },
                TodoItem {
                    content: "Third task".to_owned(),
                    status: TodoStatus::Pending,
                    blocker: None,
                },
            ],
        }];
        normalize_in_progress(&mut phases);

        let markdown = phases_to_markdown(&phases);
        assert!(markdown.contains("<!-- blocker: waiting on design review -->"));

        let (parsed, errors) = markdown_to_phases(&markdown);
        assert!(
            errors.is_empty(),
            "round trip should not produce parse errors: {errors:?}"
        );
        assert_eq!(parsed.len(), phases.len());
        assert_eq!(parsed[0].tasks.len(), phases[0].tasks.len());
        for (original, round_tripped) in phases[0].tasks.iter().zip(parsed[0].tasks.iter()) {
            assert_eq!(original.content, round_tripped.content);
            assert_eq!(original.status, round_tripped.status);
            assert_eq!(original.blocker, round_tripped.blocker);
        }
    }

    #[test]
    fn markdown_round_trip_handles_all_statuses() {
        let phases = vec![TodoPhase {
            name: "All statuses".to_owned(),
            tasks: vec![
                TodoItem {
                    content: "pending task".to_owned(),
                    status: TodoStatus::Pending,
                    blocker: None,
                },
                TodoItem {
                    content: "abandoned task".to_owned(),
                    status: TodoStatus::Abandoned,
                    blocker: None,
                },
            ],
        }];
        let markdown = phases_to_markdown(&phases);
        let (parsed, errors) = markdown_to_phases(&markdown);
        assert!(errors.is_empty());
        // in_progress 归一化会把第一个 pending 提升，所以直接比较状态集合而不是原始顺序值。
        let statuses: Vec<TodoStatus> = parsed[0].tasks.iter().map(|task| task.status).collect();
        assert!(
            statuses.contains(&TodoStatus::InProgress) || statuses.contains(&TodoStatus::Pending)
        );
        assert!(statuses.contains(&TodoStatus::Abandoned));
    }
}
