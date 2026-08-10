import type { SettingTextDict } from "../../../types";

/** 设置面板「外观」页词条。键为 setting path。 */
export const APPEARANCE_SETTINGS: SettingTextDict = {
	"ui.language": {
		label: "界面语言",
		description: "设置面板、欢迎页与内置提示所用的界面语言",
		options: {
			auto: { label: "跟随系统", description: "按系统 locale 自动选择（LANG / LC_ALL，否则取 ICU）" },
			en: { label: "English", description: "英文" },
			"zh-CN": { label: "简体中文", description: "简体中文" },
		},
	},
	"theme.dark": {
		label: "深色主题",
		description: "终端为深色背景时使用的主题",
	},
	"theme.light": {
		label: "浅色主题",
		description: "终端为浅色背景时使用的主题",
	},
	symbolPreset: {
		label: "符号预设",
		description: "图标与符号使用的字形集（Unicode、Nerd Font 或 ASCII）",
		options: {
			unicode: { label: "Unicode", description: "标准符号（默认）" },
			nerd: { label: "Nerd Font", description: "需要 Nerd Font" },
			ascii: { label: "ASCII", description: "最大兼容性" },
		},
	},
	colorBlindMode: {
		label: "色盲模式",
		description: "差异新增内容使用蓝色而非绿色",
	},
	"statusLine.preset": {
		label: "状态栏预设",
		description: "预置的状态栏配置",
		options: {
			default: { label: "默认", description: "模型、路径、Git、上下文、token、花费" },
			minimal: { label: "极简", description: "仅路径与 Git" },
			compact: { label: "紧凑", description: "模型、Git、花费、上下文" },
			full: { label: "完整", description: "包含时间在内的所有分段" },
			nerd: { label: "Nerd", description: "使用 Nerd Font 图标显示最多信息" },
			ascii: { label: "ASCII", description: "无特殊字符" },
			custom: { label: "自定义", description: "用户自定义分段" },
		},
	},
	"statusLine.separator": {
		label: "状态栏分隔符",
		description: "分段之间分隔符的样式",
		options: {
			powerline: { label: "Powerline", description: "实心箭头（Nerd Font）" },
			"powerline-thin": { label: "细箭头", description: "细箭头（Nerd Font）" },
			slash: { label: "斜杠", description: "正斜杠" },
			pipe: { label: "竖线", description: "竖直管道符" },
			block: { label: "方块", description: "实心方块" },
			none: { label: "无", description: "仅空格" },
			ascii: { label: "ASCII", description: "大于号" },
		},
	},
	"statusLine.sessionAccent": {
		label: "会话强调色",
		description: "编辑器边框与状态栏间隙使用会话名称对应的颜色",
	},
	"statusLine.transparent": {
		label: "透明状态栏",
		description:
			"状态栏使用终端默认背景色，而非主题的 `statusLineBg`。启用后会去掉 Powerline 端帽，因为它们需要一个对比色填充来过渡到周围的终端背景。",
	},
	"statusLine.compactThinkingLevel": {
		label: "紧凑思考等级",
		description: "在模型名称上以单个图标显示思考等级，而不是单独的「· <等级>」后缀。",
	},
	"statusLine.showHookStatus": {
		label: "显示 Hook 状态",
		description: "在状态栏下方显示 hook 状态消息",
	},
	"terminal.showImages": {
		label: "显示行内图片",
		description: "在终端中行内渲染图片",
	},
	"images.autoResize": {
		label: "自动缩放图片",
		description: "将大图缩放至最大 2000x2000 以提升模型兼容性",
	},
	"images.blockImages": {
		label: "阻止图片",
		description: "阻止图片发送给 LLM 服务商",
	},
	"terminal.showProgress": {
		label: "原生终端进度",
		description: "在智能体或上下文维护运行时发出 OSC 9;4 不确定进度信号",
	},
	"tui.textSizing": {
		label: "大号标题（Kitty）",
		description:
			"使用 Kitty 的 OSC 66 文本缩放协议将 Markdown H1 标题渲染为 2 倍大小。仅在 Kitty 终端生效，其他终端忽略。默认关闭。",
	},
	"tui.renderMermaid": {
		label: "渲染 Mermaid 图表",
		description: "将 Mermaid 代码块渲染为 ASCII 图表",
	},
	"tui.codexResetFireworks": {
		label: "Codex 重置烟花",
		description:
			"为计划外的 Codex 每周用量重置以及新入账的已保存重置，显示一个位于屏幕上三分之一处的烟花浮层，直到按下 Escape 才消失",
	},
	"tui.titleState": {
		label: "终端标题运行状态",
		description:
			"在终端标题的分隔符处显示智能体运行状态——工作时显示动画加载符（Windows 上为静态「:」）、轮到你时显示「>」、智能体等待你时显示「!」",
	},
	"tui.hyperlinks": {
		label: "终端超链接",
		description:
			"将路径和 URL 包装为 OSC 8 超链接，以支持终端原生点击打开（auto：自动检测支持；off：从不；always：始终启用）",
	},
	"tui.tight": {
		label: "紧凑布局",
		description: "移除终端输出左右两侧 1 个字符的水平内边距",
	},
	"tui.scrollbackRebuild": {
		label: "重写回滚缓冲区",
		description:
			"当一个块的最终内容替换其实时预览时，清空并重放终端回滚缓冲区。关闭时（默认），旧的预览副本会保留在历史记录中，最终内容会追加在下方。",
	},
	"display.shimmer": {
		label: "微光效果",
		description: "工作/加载消息的动画样式",
		options: {
			classic: { label: "经典", description: "柔和的余弦波在文本上扫过" },
			kitt: { label: "KITT 扫描器", description: "《霹雳游侠》1982 款左右来回扫动的红光" },
			disabled: { label: "禁用", description: "无动画；静态灰色文本" },
		},
	},
	"display.smoothStreaming": {
		label: "平滑流式输出",
		description: "在数据块到达时平滑显示助手文本与流式工具输入",
	},
	"display.hideToolActivity": {
		label: "隐藏工具活动",
		description: "在会话记录中隐藏模型发起的工具调用与结果",
	},
	"display.showTokenUsage": {
		label: "显示 Token 用量",
		description: "在助手消息上显示每轮的 token 用量",
	},
	"display.cacheMissMarker": {
		label: "缓存未命中标记",
		description: "当某轮助手请求未命中（丢失）提示词缓存时，在该轮上方显示分隔线",
	},
	"display.collapseCompacted": {
		label: "折叠已压缩历史",
		description:
			"在实时会话记录中，将压缩前的历史折叠在摘要分隔线之后；关闭则保持完整会话记录内联显示，并在每个压缩点处显示分隔线",
	},
	showHardwareCursor: {
		label: "显示硬件光标",
		description: "显示终端光标以支持 IME",
	},
	"tui.imeSafeCursor": {
		label: "IME 安全提示框布局",
		description: "将输入框的底部边框移到单独一行，避免 macOS IME 预编辑文字将其顶开",
	},
	"task.showResolvedModelBadge": {
		label: "显示解析后的模型徽章",
		description: "在任务组件的状态行中显示每个子智能体实际使用的模型 ID",
	},
};
