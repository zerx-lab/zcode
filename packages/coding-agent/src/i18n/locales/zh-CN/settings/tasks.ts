import type { SettingTextDict } from "../../../types";

/** 设置面板「任务」页词条。键为 setting path。 */
export const TASKS_SETTINGS: SettingTextDict = {
	"plan.enabled": {
		label: "计划模式",
		description: "启用计划模式，在执行前进行只读探索与规划",
	},
	"plan.defaultOnStartup": {
		label: "启动时进入计划模式",
		description: "每个新会话开始时自动进入计划模式",
	},
	"goal.enabled": {
		label: "目标模式",
		description: "启用逐会话目标模式与隐藏的 goal 工具",
	},
	"goal.statusInFooter": {
		label: "状态栏显示目标状态",
		description: "在状态栏的目标指示器旁显示 token 预算",
	},
	"goal.continuationModes": {
		label: "目标自动续接的运行模式",
		description: "允许活跃目标在轮次之间自动续接的运行模式",
	},
	"title.refreshOnReplan": {
		label: "重新规划时刷新标题",
		description: "todo 初始化重新规划后刷新自动生成的会话标题，除非标题是用户设置的",
	},
	"task.isolation.mode": {
		label: "隔离模式",
		description:
			"子智能体使用的隔离后端。「auto」让原生 PAL 选择最佳可用后端（优先 CoW 感知的文件系统，其次 overlayfs/ProjFS，最后回退到 git 工作树/递归复制）。",
		options: {
			none: { label: "无", description: "不隔离" },
			auto: { label: "自动", description: "让 PAL 选择最佳可用后端" },
			apfs: { label: "APFS", description: "macOS clonefile reflink（APFS）" },
			btrfs: { label: "btrfs", description: "btrfs 子卷快照" },
			zfs: { label: "ZFS", description: "ZFS 快照 + 克隆" },
			reflink: { label: "Reflink", description: "Linux FICLONE 逐文件 reflink" },
			overlayfs: { label: "Overlayfs", description: "Linux 内核 overlay（或回退到 fuse-overlayfs）" },
			projfs: { label: "ProjFS", description: "Windows 投影文件系统" },
			"block-clone": {
				label: "块克隆",
				description: "Windows FSCTL_DUPLICATE_EXTENTS_TO_FILE（NTFS/ReFS）",
			},
			rcopy: { label: "递归复制", description: "可用时使用 git 工作树，否则递归复制" },
		},
	},
	"task.isolation.apply": {
		label: "自动应用隔离改动",
		description: "自动将成功的隔离任务改动应用到父检出；关闭后保留补丁或分支产物",
	},
	"task.isolation.merge": {
		label: "隔离合并策略",
		description: "隔离任务改动的整合方式（补丁应用或分支合并）",
		options: {
			patch: { label: "补丁", description: "合并差异并 git apply" },
			branch: { label: "分支", description: "每个任务提交一次，使用 --no-ff 合并" },
		},
	},
	"task.isolation.commits": {
		label: "隔离提交信息风格",
		description: "嵌套仓库改动的提交信息风格（通用或 AI 生成）",
		options: {
			generic: { label: "通用", description: "固定的提交信息" },
			ai: { label: "AI", description: "根据差异由 AI 生成提交信息" },
		},
	},
	"worktree.base": {
		label: "工作树根目录",
		description:
			"智能体管理的工作树的基础目录 —— 任务隔离副本、`github` PR 检出、以及 `omp worktree` 清理都存放于此。未设置时使用 ~/.omp/wt。必须是绝对路径或 ~ 相对路径；相对路径会被忽略。环境变量 OMP_WORKTREE_DIR 可覆盖此设置。",
	},
	"task.eager": {
		label: "偏好任务委派",
		description: "向子智能体委派工作的倾向强度",
		options: {
			default: { label: "默认", description: "由模型自行决定何时委派" },
			preferred: { label: "偏好", description: "在系统提示词中添加委派引导" },
			always: { label: "总是", description: "提示词引导加上首轮委派提醒" },
		},
	},
	"task.batch": {
		label: "批量任务调用",
		description:
			"将 task 工具切换为批量形态：一次调用携带 { context, tasks[] } —— 每个条目一个子智能体，可选每条目 agent（默认使用会话生成策略的 agent）、每条目隔离，以及一个必需的、会附加到每个任务前面的共享 context。当 async.enabled=true 时，每次生成都作为独立的后台智能体运行，具备正常的 idle/parked 生命周期；否则调用会阻塞直至合并结果。关闭以恢复为扁平的单次生成 schema。",
	},
	"task.enableEffort": {
		label: "任务级推理强度",
		description: "在任务生成时暴露可选的 effort 参数，允许调用方覆盖每个子智能体的思考等级",
	},
	"task.maxConcurrency": {
		label: "最大并发任务数",
		description: "并发运行的子智能体最大数量",
		options: {
			"0": { label: "无限制" },
			"1": { label: "1 个任务" },
			"2": { label: "2 个任务" },
			"4": { label: "4 个任务" },
			"8": { label: "8 个任务" },
			"16": { label: "16 个任务" },
			"32": { label: "32 个任务" },
			"64": { label: "64 个任务" },
		},
	},
	"task.enableLsp": {
		label: "子智能体中的 LSP",
		description:
			"允许通过 task 工具生成的子智能体使用 lsp 工具。默认关闭以保持子智能体轻量；当 LSP 感知的委派值得额外 token 开销时再启用。",
	},
	"task.maxRecursionDepth": {
		label: "最大任务递归深度",
		description: "子智能体可以生成自己的子智能体的最大层数",
		options: {
			"-1": { label: "无限制" },
			"0": { label: "禁止" },
			"1": { label: "单层" },
			"2": { label: "双层" },
			"3": { label: "三层" },
		},
	},
	"task.maxRuntimeMs": {
		label: "子智能体最大运行时长",
		description:
			"每个子智能体的硬性挂钟时间上限（毫秒）。0 表示禁用。作为纵深防御，用于应对逃过推理层看门狗的服务商侧流式挂起；触发时会以「超时」原因正常中止子智能体。",
		options: {
			"0": { label: "无限制", description: "默认" },
			"300000": { label: "5 分钟" },
			"900000": { label: "15 分钟" },
			"1800000": { label: "30 分钟" },
			"3600000": { label: "1 小时" },
		},
	},
	"task.agentIdleTtlMs": {
		label: "智能体空闲存活时间",
		description:
			"空闲子智能体在被换出（park）到磁盘前，在内存中保持存活的时长（毫秒）。已换出的智能体在被发消息或恢复时会自动复活。0 表示空闲智能体一直存活直到退出。",
	},
	"task.softRequestBudget": {
		label: "子智能体软请求预算",
		description:
			"每个子智能体的软性请求预算（每次运行的助手请求数）。超出后会注入收尾引导提示（见 task.softRequestBudgetNotice）；达到预算的 1.5 倍时运行会被强制停止，智能体必须交出其部分成果。0 表示禁用该防护。内置的 scout/sonic 智能体自身有更低的内置预算上限，因此低于该上限的值仍会对它们生效。",
		options: {
			"0": { label: "禁用" },
			"90": { label: "90 次请求" },
			"150": { label: "150 次请求" },
			"200": { label: "200 次请求", description: "默认" },
		},
	},
	"task.softRequestBudgetNotice": {
		label: "软请求预算提醒",
		description: "当子智能体超出软请求预算时注入一条收尾引导提示，要求其在触发 1.5 倍强制停止前收尾",
	},
	"task.maxEffort": {
		label: "单次生成最大推理强度",
		description:
			"task 工具单次生成推理强度提示所允许的最大值。较低的值可阻止调用方将子智能体的推理强度升级到该上限以上；默认值保留模型的完整范围。",
		options: {
			minimal: { label: "最低", description: "极简推理（约 1k token）" },
			low: { label: "低", description: "轻量推理（约 2k token）" },
			medium: { label: "中等", description: "适中推理（约 8k token）" },
			high: { label: "高", description: "深度推理（约 16k token）" },
			xhigh: { label: "超高", description: "扩展推理（约 32k token）" },
			max: { label: "最大", description: "模型支持的最大推理强度" },
		},
	},
	"task.prewalk": {
		label: "通用任务预跑（Prewalk）",
		description:
			"为内置的通用 `task` 子智能体启用 prewalk：它先在其解析出的模型上启动，进行规划并开始实现，再在首次编辑/写入时移交给「smol」角色。各智能体的覆盖设置（task.agentPrewalk，在 /agents 中用 P 切换）以及用户智能体 `prewalk` frontmatter 不受此开关影响，始终生效。",
	},
	"skills.enableSkillCommands": {
		label: "技能命令",
		description: "将技能注册为 /skill:name 命令",
	},
	"commands.enableClaudeUser": {
		label: "Claude 用户命令",
		description: "从 ~/.claude/commands/ 加载命令",
	},
	"commands.enableClaudeProject": {
		label: "Claude 项目命令",
		description: "从 .claude/commands/ 加载命令",
	},
	"commands.enableOpencodeUser": {
		label: "OpenCode 用户命令",
		description: "从 ~/.config/opencode/commands/ 加载命令",
	},
	"commands.enableOpencodeProject": {
		label: "OpenCode 项目命令",
		description: "从 .opencode/commands/ 加载命令",
	},
};
