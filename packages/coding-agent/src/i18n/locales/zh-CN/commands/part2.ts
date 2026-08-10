import type { CommandTextDict } from "../../../types";

/** 内置 slash 命令描述（第 2 片）。键为命令名，绝不翻译命令名本身。 */
export const COMMANDS_PART2: CommandTextDict = {
	copy: {
		description: "从对话中选取文本或代码复制",
	},
	todo: {
		description: "查看或修改待办列表",
		subcommands: {
			edit: "用 $EDITOR 打开待办（Markdown 往返）",
			copy: "将待办复制为 Markdown 到剪贴板",
			export: "将待办写为 Markdown 文件（默认 TODO.md）",
			import: "从 Markdown 文件替换待办（默认 TODO.md）",
			append: "追加任务；阶段模糊匹配或自动创建",
			start: "将任务标记为进行中（模糊匹配）",
			done: "将任务/阶段/全部标记为已完成（模糊匹配）",
			drop: "将任务/阶段/全部标记为已放弃（模糊匹配）",
			rm: "移除任务/阶段/全部（模糊匹配）",
		},
	},
	session: {
		description: "会话管理命令",
		subcommands: {
			info: "显示会话信息与统计",
			delete: "删除当前会话并返回选择器",
			pin: "将当前服务商固定到已保存的 OAuth 账号",
		},
	},
	jobs: {
		description: "显示异步后台任务状态",
	},
	usage: {
		description: "显示服务商用量与限额",
		subcommands: {
			show: "显示服务商用量与限额",
			reset: "使用已保存的 Codex 限流重置",
		},
	},
	stats: {
		description: "启动本地统计面板",
	},
	changelog: {
		description: "显示更新日志条目",
		subcommands: {
			full: "显示完整更新日志",
		},
	},
	hotkeys: {
		description: "显示所有快捷键",
	},
	tools: {
		description: "显示 agent 当前可见的工具",
	},
	context: {
		description: "显示预估上下文用量明细",
	},
	extensions: {
		description: "打开扩展控制中心面板",
	},
	agents: {
		description: "打开 Agent 控制中心面板",
	},
	branch: {
		description: "从历史消息创建新分支",
	},
	fork: {
		description: "从历史消息创建新分叉",
	},
	tree: {
		description: "浏览会话树（切换分支）",
	},
	login: {
		description: "使用 OAuth 服务商登录",
	},
	logout: {
		description: "从 OAuth 服务商登出",
	},
	mcp: {
		description: "管理 MCP 服务器（添加、列出、移除、测试）",
		subcommands: {
			add: "添加新的 MCP 服务器",
			list: "列出所有已配置的 MCP 服务器",
			remove: "移除一个 MCP 服务器",
			test: "测试与服务器的连接",
			reauth: "为服务器重新授权 OAuth",
			unauth: "移除服务器的 OAuth 授权",
			enable: "启用一个 MCP 服务器",
			disable: "禁用一个 MCP 服务器",
			"smithery-search": "搜索 Smithery 注册表并部署 MCP 服务器",
			"smithery-login": "登录 Smithery 并缓存 API key",
			"smithery-logout": "移除已缓存的 Smithery API key",
			reconnect: "重新连接指定的 MCP 服务器",
			reload: "强制重新加载 MCP 运行时工具",
			resources: "列出已连接服务器的可用资源",
			prompts: "列出已连接服务器的可用提示词",
			notifications: "显示通知能力与订阅",
			help: "显示帮助信息",
		},
	},
	ssh: {
		description: "管理 SSH 主机（添加、列出、移除）",
		subcommands: {
			add: "添加一个 SSH 主机",
			list: "列出所有已配置的 SSH 主机",
			remove: "移除一个 SSH 主机",
			help: "显示帮助信息",
		},
	},
	new: {
		description: "开始一个新会话",
	},
	fresh: {
		description: "重置服务商流状态，不改动本地会话记录",
	},
	clear: {
		description: "原地清空对话上下文，保留会话",
	},
	drop: {
		description: "删除当前会话并新建一个",
	},
	compact: {
		description: "手动压缩会话上下文",
		subcommands: {
			soft: "用当前模型本地总结（跳过远程端点）",
			remote: "通过远程端点/服务商原生压缩来总结",
			snapcompact: "将历史归档为模型可读回的密集位图（不调用 LLM）",
		},
	},
};
