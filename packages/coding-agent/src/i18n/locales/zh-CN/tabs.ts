import type { TextDict } from "../../types";

/** 设置面板 tab 标题，键为 `SettingTab`。 */
export const ZH_CN_TABS: TextDict = {
	appearance: "外观",
	model: "模型",
	interaction: "交互",
	context: "上下文",
	memory: "记忆",
	files: "文件",
	shell: "Shell",
	tools: "工具",
	tasks: "任务",
	providers: "服务商",
};

/** 设置面板分组标题，键为 `TAB_GROUPS` 里的英文组名。 */
export const ZH_CN_GROUPS: TextDict = {
	// appearance
	Theme: "主题",
	"Status Line": "状态栏",
	Display: "显示",
	Images: "图像",
	// model
	Thinking: "思考",
	Sampling: "采样",
	Prompt: "提示词",
	"Retry & Fallback": "重试与兜底",
	Advisor: "顾问",
	Prewalk: "预热",
	Vision: "视觉",
	// interaction
	Input: "输入",
	Approvals: "审批",
	Notifications: "通知",
	Speech: "语音",
	Collab: "协作",
	"Magic Keywords": "魔法关键词",
	"Startup & Updates": "启动与更新",
	"Power (macOS)": "电源（macOS）",
	Agent: "智能体",
	Git: "Git",
	// context
	General: "通用",
	Compaction: "压缩",
	"Rules (TTSR)": "规则（TTSR）",
	Experimental: "实验特性",
	// memory
	"Auto-Learn": "自动学习",
	Mnemopi: "Mnemopi",
	Hindsight: "Hindsight",
	// files
	Editing: "编辑",
	Reading: "读取",
	"Read Summaries": "读取摘要",
	LSP: "LSP",
	// shell
	Bash: "Bash",
	"Eval & Runtimes": "Eval 与运行时",
	// tools
	"Available Tools": "可用工具",
	Todos: "待办",
	"Grep & Browser": "Grep 与浏览器",
	Computer: "计算机控制",
	GitHub: "GitHub",
	"Output Limits": "输出限制",
	Execution: "执行",
	"Discovery & MCP": "发现与 MCP",
	Developer: "开发者",
	// tasks
	Modes: "模式",
	Subagents: "子智能体",
	Isolation: "隔离",
	"Commands & Skills": "命令与技能",
	// providers
	Services: "服务",
	Fireworks: "Fireworks",
	Protocol: "协议",
	Timeouts: "超时",
	Privacy: "隐私",
};
