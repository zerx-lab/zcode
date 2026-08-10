import type { SettingTextDict } from "../../../types";

/** 设置面板「上下文」页词条。键为 setting path。 */
export const CONTEXT_SETTINGS: SettingTextDict = {
	"workspace.additionalDirectories": {
		label: "额外工作区目录",
		description:
			"为每个会话额外添加的工作区根目录（多根工作区）。可通过 /add-dir 和 /remove-dir 实时管理。路径相对 cwd 解析；建议使用绝对路径。智能体会被告知这些根目录存在，可对其执行 read/grep/glob。",
	},
	"contextPromotion.enabled": {
		label: "自动升级上下文",
		description: "上下文溢出时切换到更大上下文窗口的模型，而非执行压缩",
	},
	"compaction.enabled": {
		label: "自动压缩",
		description: "上下文过大时自动压缩",
	},
	"compaction.midTurnEnabled": {
		label: "回合中压缩",
		description: "在下一次服务商请求前，于安全的回合中工具循环边界检查阈值",
	},
	"compaction.strategy": {
		label: "压缩策略",
		description:
			"选择原地上下文已满维护、自动交接、外科手术式精简（丢弃重内容）、snapcompact（将历史归档为密集图像），或关闭自动维护（off）",
		options: {
			"context-full": { label: "Context-full", description: "原地摘要并保留当前会话" },
			handoff: { label: "Handoff", description: "生成交接文档并在新会话中继续" },
			shake: { label: "Shake", description: "原地丢弃重内容（工具结果与大块内容）；可通过 artifact 恢复" },
			snapcompact: {
				label: "Snapcompact",
				description: "将历史归档为模型可读回的密集位图图像；不调用 LLM",
			},
			off: { label: "Off", description: "关闭自动上下文维护（与 Auto-compact 关闭行为相同）" },
		},
	},
	"compaction.thresholdPercent": {
		label: "压缩阈值",
		description: "触发上下文维护的百分比阈值；设为 Default 使用旧版基于预留空间的行为",
		options: {
			default: { label: "Default", description: "旧版基于预留空间的阈值" },
			"10": { label: "10%", description: "极早维护" },
			"20": { label: "20%", description: "很早维护" },
			"30": { label: "30%", description: "较早维护" },
			"40": { label: "40%", description: "中等偏早维护" },
			"50": { label: "50%", description: "中间点" },
			"60": { label: "60%", description: "中等上下文占用" },
			"70": { label: "70%", description: "均衡" },
			"75": { label: "75%", description: "略偏激进" },
			"80": { label: "80%", description: "典型阈值" },
			"85": { label: "85%", description: "激进上下文占用" },
			"90": { label: "90%", description: "非常激进" },
			"95": { label: "95%", description: "接近上下文上限" },
		},
	},
	"compaction.thresholdTokens": {
		label: "压缩 token 上限",
		description: "上下文维护的固定 token 上限；设置后优先于百分比阈值",
		options: {
			default: { label: "Default", description: "使用基于百分比的阈值" },
			"25000": { label: "25K tokens", description: "200K 窗口的四分之一" },
			"50000": { label: "50K tokens", description: "200K 窗口的一半" },
			"100000": { label: "100K tokens", description: "200K 窗口的一半" },
			"150000": { label: "150K tokens", description: "200K 窗口的四分之三" },
			"200000": { label: "200K tokens", description: "完整标准上下文窗口" },
			"300000": { label: "300K tokens", description: "大型上下文窗口" },
			"500000": { label: "500K tokens", description: "超大型上下文窗口" },
		},
	},
	"compaction.handoffSaveToDisk": {
		label: "保存交接文档",
		description: "为自动交接流程将生成的交接文档保存为 Markdown 文件",
	},
	"compaction.remoteEnabled": {
		label: "远程压缩",
		description: "可用时使用远程压缩接口而非本地摘要",
	},
	"compaction.remoteStreamingV2Enabled": {
		label: "远程压缩 V2",
		description: "对兼容的远程压缩模型使用 Responses 流式压缩",
	},
	"compaction.idleEnabled": {
		label: "空闲压缩",
		description: "空闲时若 token 数超过阈值则压缩上下文",
	},
	"compaction.idleThresholdTokens": {
		label: "空闲压缩阈值",
		description: "触发空闲压缩的 token 数阈值",
		options: {
			"100000": { label: "100K tokens" },
			"200000": { label: "200K tokens" },
			"300000": { label: "300K tokens" },
			"400000": { label: "400K tokens" },
			"500000": { label: "500K tokens" },
			"600000": { label: "600K tokens" },
			"700000": { label: "700K tokens" },
			"800000": { label: "800K tokens" },
			"900000": { label: "900K tokens" },
		},
	},
	"compaction.idleTimeoutSeconds": {
		label: "空闲压缩延迟",
		description: "空闲多久后触发压缩",
		options: {
			"60": { label: "1 分钟" },
			"120": { label: "2 分钟" },
			"300": { label: "5 分钟" },
			"600": { label: "10 分钟" },
			"1800": { label: "30 分钟" },
			"3600": { label: "1 小时" },
		},
	},
	"compaction.supersedeReads": {
		label: "淘汰过期读取",
		description: "同一文件再次被读取时清除较旧的读取结果（感知缓存，每回合执行）",
	},
	"compaction.dropUseless": {
		label: "剔除无效结果",
		description: "在被消费后清除标记为无上下文价值的工具结果（无匹配、等待超时）（感知缓存）",
	},
	"snapcompact.systemPrompt": {
		label: "Snapcompact 系统提示词",
		description:
			"实验性功能：将选定的系统提示词文本渲染为密集 PNG 图像并附加到首条用户消息（仅限视觉模型）。节省 token；但图像化文本会失去提示词缓存。",
		options: {
			none: { label: "None", description: "系统提示词保持文本形式。" },
			"agents-md": {
				label: "AGENTS.md",
				description: "仅在能节省 token 时，将已加载的上下文文件指令移至图像。",
			},
			all: { label: "All", description: "在能节省 token 时，将完整系统提示词移至图像。" },
		},
	},
	"snapcompact.toolResults": {
		label: "Snapcompact 工具结果",
		description:
			"实验性功能：将大量历史工具结果渲染为密集 PNG 图像而非文本（仅限视觉模型）。节省累积读取/搜索输出所占用的 token。",
	},
	"tools.format": {
		label: "工具调用模式",
		description:
			"控制工具向模型暴露的方式。Auto 默认使用服务商原生工具调用，除非所选模型被标记为不支持，此时回退到 GLM 自有方言。Native 强制使用服务商原生工具；其余取值强制使用对应的自有方言。在会话开始时生效。",
		options: {
			auto: { label: "Auto", description: "除非模型已知不支持，否则使用原生工具调用。" },
			native: { label: "Native", description: "使用服务商原生工具调用。" },
			glm: { label: "GLM", description: "使用 GLM 风格的带内工具调用。" },
			hermes: { label: "Hermes", description: "使用 Hermes 风格的带内工具调用。" },
			kimi: { label: "Kimi", description: "使用 Kimi 风格的带内工具调用。" },
			xml: { label: "XML", description: "使用通用 XML 带内工具调用。" },
			anthropic: { label: "Anthropic", description: "使用 Anthropic 风格的带内工具调用。" },
			deepseek: { label: "DeepSeek", description: "使用 DeepSeek 风格的带内工具调用。" },
			harmony: { label: "Harmony", description: "使用 Harmony 风格的带内工具调用。" },
			qwen3: { label: "Qwen3", description: "使用 Qwen3 自有方言。" },
			gemini: { label: "Gemini", description: "使用 Gemini 自有方言。" },
			gemma: { label: "Gemma", description: "使用 Gemma 自有方言。" },
			minimax: { label: "MiniMax", description: "使用 MiniMax 自有方言。" },
		},
	},
	"snapcompact.shape": {
		label: "Snapcompact 图形样式",
		description: "snapcompact 输出文本所用的帧样式（压缩归档与内联成像）。Auto 会为当前模型挑选合适样式。",
		options: {
			auto: { label: "Auto", description: "为当前模型挑选合适样式，找不到时回退到其服务商系列。" },
			"8x8r-bw": {
				label: "8x8 重复，黑色",
				description: "unscii 方形字元，黑色墨水，每行打印两次，副本带有浅色高亮条。",
			},
			"8x8r-sent": {
				label: "8x8 重复，语句配色",
				description: "重复网格，墨水在语句边界循环切换六种色调。",
			},
			"8x8u-bw": { label: "8x8，黑色", description: "普通 unscii 方形字元，单次打印行，黑色墨水。" },
			"8x8u-sent": { label: "8x8，语句配色", description: "普通 unscii 方形字元，配语句色调墨水。" },
			"6x6u-bw": {
				label: "6x6 密集，黑色",
				description: "unscii 压缩至 6x6 —— 最密集的可读字元、帧数最少 —— 黑色墨水。",
			},
			"6x6u-sent": { label: "6x6 密集，语句配色", description: "最密集字元配语句色调墨水。" },
			"5x8-bw": {
				label: "5x8 旧版，黑色",
				description: "2576px 帧上的原始 X.org 5x8 字形，黑色墨水。",
			},
			"5x8-sent": {
				label: "5x8 旧版，语句配色",
				description: "最初的 snapcompact 样式（早期未使用样式表的会话即渲染此样式）。",
			},
			"6x12-dim": {
				label: "6x12，弱化虚词",
				description: "X.org 6x12 字形，黑色墨水，功能词以灰色弱化显示。",
			},
			"8x13-bw": { label: "8x13，黑色", description: "X.org 8x13 字形，黑色墨水。" },
			"8on16-bw": {
				label: "8x13 于 16px 行距，黑色",
				description: "8x13 字形置于 8x16 字元（加大行距），黑色墨水。",
			},
			"8on22-bw": {
				label: "8x13 于 22px 行距（加宽行距），黑色",
				description: "8x13 字形置于 8x22 字元 —— 行距加大以避免行间拥挤。OpenAI/Google 默认样式。",
			},
			"11on16-bw": {
				label: "8x13 于 11px 字距（加宽字距），黑色",
				description: "8x13 字形置于 11x16 字元 —— 字距加大以避免字符粘连。Anthropic 默认样式。",
			},
			"silver16-bw": {
				label: "Silver 16，CJK",
				description: "内置 Silver TrueType 字体，16px 网格，适用于 CJK 及其他非拉丁文本。",
			},
			"doc-8on16-bw": {
				label: "文档 8on16，黑色",
				description: "两栏报纸式排版，8x13 字形，16px 行距，黑色墨水。",
			},
			"doc-8on16-sent": {
				label: "文档 8on16，语句配色",
				description: "双栏文档排版，配语句色调墨水。",
			},
			"doc-8on16-sent-dim": {
				label: "文档 8on16，语句配色 + 弱化虚词",
				description: "双栏文档排版，语句色调墨水，功能词以灰色弱化显示。",
			},
		},
	},
	"branchSummary.enabled": {
		label: "分支摘要",
		description: "离开分支时提示进行摘要",
	},
	"ttsr.enabled": {
		label: "TTSR",
		description: "当输出匹配规则模式时中途打断智能体（Time-Traveling Stream Rules，时间旅行流式规则）",
	},
	"ttsr.contextMode": {
		label: "TTSR 上下文模式",
		description: "TTSR 触发时如何处理未完成的输出",
	},
	"ttsr.interruptMode": {
		label: "TTSR 打断模式",
		description: "何时中途打断，何时改为在完成后注入警告",
		options: {
			always: { label: "always", description: "对正文与工具流均中途打断" },
			"prose-only": { label: "prose-only", description: "仅在正文/思考匹配时打断" },
			"tool-only": { label: "tool-only", description: "仅在工具调用参数匹配时打断" },
			never: { label: "never", description: "从不打断；改为在完成后注入警告" },
		},
	},
	"ttsr.repeatMode": {
		label: "TTSR 重复模式",
		description: "规则的重复触发方式：每会话一次，或间隔一定消息数后可再次触发",
	},
	"ttsr.repeatGap": {
		label: "TTSR 重复间隔",
		description: "规则再次触发前需间隔的消息数",
		options: {
			"5": { label: "5 条消息" },
			"10": { label: "10 条消息" },
			"15": { label: "15 条消息" },
			"20": { label: "20 条消息" },
			"30": { label: "30 条消息" },
		},
	},
	"ttsr.builtinRules": {
		label: "内置规则",
		description: "加载智能体自带的默认规则（可通过 ttsr.disabledRules 单独覆盖）",
	},
	"ttsr.disabledRules": {
		label: "禁用的规则",
		description: "要完全忽略的规则名称（对内置默认规则与自定义规则均生效）",
	},
};
