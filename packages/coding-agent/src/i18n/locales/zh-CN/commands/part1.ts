import type { CommandTextDict } from "../../../types";

/** 内置 slash 命令描述（第 1 片）。键为命令名，绝不翻译命令名本身。 */
export const COMMANDS_PART1: CommandTextDict = {
	security: {
		description: "规划、运行、查看、导入和比较 OMP 原生安全扫描",
		subcommands: {
			plan: "创建不可变的安全扫描计划",
			scan: "启动已规划或新建的原生扫描",
			status: "显示原生扫描操作状态",
			cancel: "取消正在运行的原生扫描",
			scans: "列出项目中已保存的安全扫描",
			show: "渲染扫描或 security:// 资源",
			import: "导入 SARIF 或 Codex Security 包",
			export: "导出标准包、SARIF 或报告",
			validate: "用 OMP 原生工具验证一个发现项",
			compare: "比较两次扫描间发现项的演变",
			disposition: "设置发现项的处置结论及理由",
		},
	},
	settings: {
		description: "打开设置菜单",
	},
	setup: {
		description: "打开服务商设置",
		subcommands: {
			providers: "配置登录与网络搜索服务商",
		},
	},
	plan: {
		description: "切换计划模式(智能体先规划再执行)",
	},
	"plan-review": {
		description: "重新打开最新计划的评审(仅计划模式)",
	},
	vibe: {
		description: "切换 vibe 模式(直连持久 fast/good 工作会话；只读工具集)",
	},
	goal: {
		description: "切换目标模式(本会话的持久自主目标)",
		subcommands: {
			set: "设置或替换目标",
			show: "显示当前目标详情",
			pause: "暂停当前目标",
			resume: "恢复已暂停的目标",
			drop: "放弃当前目标",
			budget: "调整 token 预算",
		},
	},
	"guided-goal": {
		description: "让智能体在对话中访谈你，再设置目标模式",
	},
	loop: {
		description: "切换循环模式。启用后，下一条提示会在每次 yield 后重新提交；Esc 取消当前迭代，再次 /loop 关闭",
	},
	queue: {
		description: "排队一条消息，待智能体 yield 后发送",
	},
	model: {
		description: "切换本会话的模型",
	},
	switch: {
		description: "切换本会话的模型(同 alt+p)",
	},
	fast: {
		description: "切换优先服务层级(OpenAI service_tier=priority，Anthropic speed=fast)",
		subcommands: {
			on: "启用 fast 模式",
			off: "关闭 fast 模式",
			status: "显示 fast 模式状态",
		},
	},
	computer: {
		description: "切换本会话的原生 computer-use 工具",
		subcommands: {
			on: "为本会话启用 computer use",
			off: "为本会话关闭 computer use",
			status: "显示 computer use 状态",
		},
	},
	vision: {
		description: "控制本会话的 inspect_image 视觉委派工具",
		subcommands: {
			on: "本会话始终暴露 inspect_image",
			off: "本会话永不暴露 inspect_image",
			auto: "跟随 inspect_image.mode(auto 对支持视觉的模型自动隐藏)",
			status: "显示 inspect_image 状态",
		},
	},
	prewalk: {
		description: "下一个动作切换到 fast/cheap 模型(无需 --prewalk 也可用)",
	},
	advisor: {
		description: "切换 advisor(对每轮进行复核并注入笔记的第二模型)",
		subcommands: {
			on: "启用 advisor",
			off: "关闭 advisor",
			status: "显示 advisor 状态",
			dump: "将 advisor 的会话记录复制到剪贴板",
			configure: "打开 advisor 配置编辑器(TUI)",
		},
	},
	export: {
		description: "将会话导出为 HTML 文件",
	},
	dump: {
		description: "将会话记录复制到剪贴板(并将 LLM 请求 JSON 写入 tmp)",
	},
	share: {
		description: "通过加密链接分享会话(share server 或 secret gist)",
	},
	collab: {
		description: "通过 relay 实时分享本会话",
		subcommands: {
			view: "分享只读链接(访客只能查看，不能发送提示)",
			status: "显示链接和参与者",
			stop: "停止分享",
		},
	},
	join: {
		description: "加入共享的 collab 会话",
	},
	leave: {
		description: "离开 collab 会话",
	},
	browser: {
		description: "切换浏览器 headless / visible 模式",
		subcommands: {
			headless: "切换到 headless 模式",
			visible: "切换到 visible 模式",
		},
	},
};
