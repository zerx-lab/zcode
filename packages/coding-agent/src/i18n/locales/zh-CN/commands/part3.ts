import type { CommandTextDict } from "../../../types";

/** 内置 slash 命令描述（第 3 片）。键为命令名，绝不翻译命令名本身。 */
export const COMMANDS_PART3: CommandTextDict = {
	shake: {
		description: "清除上下文中的重量级内容（工具结果、大块内容）",
		subcommands: {
			elide: "清除工具结果与大块内容（默认）",
			images: "清除图片内容",
		},
	},
	handoff: {
		description: "将会话上下文交接给新会话",
	},
	resume: {
		description: "恢复另一个会话",
	},
	btw: {
		description: "基于当前会话上下文提出一次性附带问题",
	},
	tan: {
		description: "对旁支任务启动完整后台智能体",
	},
	omfg: {
		description: "从抱怨中生成 TTSR 规则，制止反复出现的行为",
	},
	retry: {
		description: "重试上一次失败的智能体回合",
	},
	debug: {
		description: "打开调试工具选择器",
	},
	memory: {
		description: "查看并操作记忆维护",
		subcommands: {
			view: "显示当前记忆注入内容",
			stats: "显示记忆后端统计信息",
			diagnose: "运行记忆后端诊断",
			clear: "清除持久化的记忆数据与产物",
			reset: "clear 的别名",
			enqueue: "排队执行记忆巩固维护",
			rebuild: "enqueue 的别名",
			"mm list": "列出当前记忆库中的心智模型",
			"mm show": "显示单个心智模型（需要 id）",
			"mm refresh": "刷新库内所有自动刷新模型，或按 id 刷新单个模型",
			"mm history": "查看心智模型的变更历史差异",
			"mm seed": "创建缺失的内置心智模型",
			"mm delete": "从库中删除心智模型（需要 id）",
			"mm reload": "重新拉取缓存的 <mental_models> 块",
		},
	},
	rename: {
		description: "重命名当前会话",
	},
	move: {
		description: "将当前会话移动到其他目录",
	},
	"add-dir": {
		description: "为此会话添加工作区目录（多根目录）",
	},
	"remove-dir": {
		description: "从此会话移除工作区目录",
	},
	dirs: {
		description: "列出此会话的工作区目录",
	},
	exit: {
		description: "退出应用",
	},
	marketplace: {
		description: "管理市场插件源与已安装插件",
		subcommands: {
			add: "添加市场源",
			remove: "移除市场源",
			update: "更新市场目录",
			list: "列出已配置的市场",
			discover: "浏览可用插件",
			install: "安装插件（无参数时打开交互式浏览器）",
			uninstall: "卸载插件（无参数时打开选择器）",
			installed: "列出已安装的市场插件",
			upgrade: "升级过时插件",
			help: "显示使用指南",
		},
	},
	plugins: {
		description: "查看并管理已安装插件",
		subcommands: {
			list: "列出所有已安装插件（npm + 市场）",
			enable: "启用市场插件",
			disable: "禁用市场插件",
		},
	},
	"reload-plugins": {
		description: "重新加载所有插件（技能、命令、hook、工具、智能体、MCP）",
	},
	force: {
		description: "强制下一回合使用指定工具",
	},
	live: {
		description: "启动基于 Codex 的实时语音模式",
	},
	pause: {
		description: "冻结所有智能体（主智能体、子智能体、advisor），直至恢复",
	},
	quit: {
		description: "退出应用",
	},
};
