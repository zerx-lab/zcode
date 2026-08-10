import type { SettingTextDict } from "../../../types";

/** 设置面板「模型」页词条。键为 setting path。 */
export const MODEL_SETTINGS: SettingTextDict = {
	"advisor.enabled": {
		label: "启用 Advisor",
		description: "搭配第二个模型（分配给 'advisor' 角色），被动审查每一轮对话并注入笔记",
	},
	"prewalk.enabled": {
		label: "启用 Prewalk",
		description:
			"先用当前激活模型启动，在计划提醒的 todo 列表出现后的第一次编辑/写入操作时切换到一个快/廉价模型（默认 'smol' 角色）——强模型负责规划、提交 todo，再交由后者开始实现。可通过 --prewalk / --no-prewalk 按会话覆盖。",
	},
	"advisor.subagents": {
		label: "子智能体启用 Advisor",
		description: "在派生的 task/eval 子智能体上也启用 advisor",
	},
	"advisor.syncBacklog": {
		label: "Advisor 同步积压",
		description: "若 advisor 落后达到此轮数，暂停主智能体最多 30 秒。关闭则禁用追赶延迟。",
	},
	"advisor.immuneTurns": {
		label: "Advisor 免打扰轮数",
		description: "advisor 的关注点或阻塞项打断一次后，在此后的若干主轮次内，将后续关注点/阻塞项改为非打断方式路由。",
		options: {
			"0": { label: "0 轮", description: "允许每一个关注点/阻塞项都打断" },
			"1": { label: "1 轮" },
			"2": { label: "2 轮" },
			"3": { label: "3 轮", description: "默认" },
			"4": { label: "4 轮" },
			"5": { label: "5 轮" },
		},
	},
	modelRoleStorage: {
		label: "模型角色存储位置",
		description: "模型选择器的角色分配保存在何处",
		options: {
			global: { label: "全局", description: "将角色模型保存到当前激活的 profile 配置中（现有行为）" },
			project: {
				label: "按项目",
				description: "将项目级角色模型保存到 .omp/config.yml；缺失的项目角色回退到全局默认值",
			},
		},
	},
	"images.describeForTextModels": {
		label: "为纯文本模型描述图片",
		description:
			"当图片附加给不支持视觉的模型时，将其保存到 local:// 下，并注入一个视觉模型生成的描述，而不是直接丢弃",
	},
	defaultThinkingLevel: {
		label: "思考等级",
		description: "支持思考的模型的推理深度",
		options: {
			auto: { label: "auto", description: "按提示词自动检测" },
			minimal: { label: "min", description: "非常简短的推理（约 1k tokens）" },
			low: { label: "low", description: "轻量推理（约 2k tokens）" },
			medium: { label: "medium", description: "适中推理（约 8k tokens）" },
			high: { label: "high", description: "深度推理（约 16k tokens）" },
			xhigh: { label: "xhigh", description: "扩展推理（约 32k tokens）" },
			max: { label: "max", description: "模型支持的最大推理量" },
		},
	},
	hideThinkingBlock: {
		label: "隐藏思考块",
		description: "在助手回复中隐藏思考块",
	},
	proseOnlyThinking: {
		label: "仅文字思考",
		description: "在思考摘要中省略代码块，改用省略号替代",
	},
	omitThinking: {
		label: "省略思考摘要",
		description: "指示上游服务商在响应中完全省略思考摘要（如受支持）",
	},
	"model.loopGuard.enabled": {
		label: "循环守卫",
		description: "为模型推理与正文内容启用自动流式循环检测",
	},
	"model.loopGuard.checkAssistantContent": {
		label: "循环守卫扫描正文",
		description: "除思考日志外，也对助手正文消息应用循环守卫",
	},
	"model.loopGuard.toolCallReminder": {
		label: "循环守卫工具调用提醒",
		description:
			"当 Gemini 推理流连续输出多个规划性标题却不调用工具时，中断该流并注入提醒以促使其发起工具调用（需启用循环守卫）",
	},
	"model.toolCallLoopGuard.enabled": {
		label: "工具调用循环守卫",
		description: "检测跨轮次连续相同的工具调用，并注入纠正性引导",
	},
	"model.toolCallLoopGuard.threshold": {
		label: "工具调用循环阈值",
		description: "触发纠正性引导所需的连续相同工具调用次数",
	},
	"model.toolCallLoopGuard.exemptTools": {
		label: "工具调用循环豁免工具",
		description: "允许连续重复而不会触发跨轮次循环守卫的工具名称",
	},
	inlineToolDescriptors: {
		label: "内联工具描述",
		description:
			"在系统提示词中渲染完整的工具描述，并从服务商工具 schema 中剥离顶层/嵌套描述，使描述文本只发送一次。Auto 会对 Gemini 模型自动启用，其他模型则禁用",
		options: {
			auto: { label: "Auto", description: "对 Gemini 模型内联描述；其他模型保留在工具 schema 中" },
			on: { label: "On", description: "始终在系统提示词中内联描述" },
			off: { label: "Off", description: "描述只保留在服务商工具 schema 中" },
		},
	},
	includeModelInPrompt: {
		label: "在提示词中包含模型",
		description: "在系统提示词中显示当前激活的模型标识，使智能体知道自己是哪个模型",
	},
	includeWorkspaceTree: {
		label: "包含工作区目录树",
		description: "在系统提示词中渲染工作区目录树。警告：文件被修改时，这可能会破坏跨会话的提示词缓存。",
	},
	personality: {
		label: "人格风格",
		description: "渲染进系统提示词人格模块中的沟通风格",
		options: {
			default: { label: "Default", description: "简洁、以证据为先的工程师；回复密集、注重行动" },
			friendly: { label: "Friendly", description: "温暖、鼓励型协作者，注重推进节奏与士气" },
			pragmatic: { label: "Pragmatic", description: "直接、高效的工程师，注重清晰与严谨" },
			none: { label: "None", description: "完全省略人格模块" },
		},
	},
	temperature: {
		label: "Temperature",
		description: "采样温度（0 = 确定性，1 = 富有创造性，-1 = 服务商默认值）",
		options: {
			"-1": { label: "Default", description: "使用服务商默认值" },
			"0": { label: "0", description: "确定性" },
			"0.2": { label: "0.2", description: "聚焦" },
			"0.5": { label: "0.5", description: "均衡" },
			"0.7": { label: "0.7", description: "富有创造性" },
			"1": { label: "1", description: "最大多样性" },
		},
	},
	topP: {
		label: "Top P",
		description: "核采样截断值（0-1，-1 = 服务商默认值）",
		options: {
			"-1": { label: "Default", description: "使用服务商默认值" },
			"0.1": { label: "0.1", description: "非常聚焦" },
			"0.3": { label: "0.3", description: "聚焦" },
			"0.5": { label: "0.5", description: "均衡" },
			"0.9": { label: "0.9", description: "宽泛" },
			"1": { label: "1", description: "不做核过滤" },
		},
	},
	topK: {
		label: "Top K",
		description: "从概率最高的 K 个 token 中采样（-1 = 服务商默认值）",
		options: {
			"-1": { label: "Default", description: "使用服务商默认值" },
			"1": { label: "1", description: "贪心选取最高概率 token" },
			"20": { label: "20", description: "聚焦" },
			"40": { label: "40", description: "均衡" },
			"100": { label: "100", description: "宽泛" },
		},
	},
	minP: {
		label: "Min P",
		description: "最小概率阈值（0-1，-1 = 服务商默认值）",
		options: {
			"-1": { label: "Default", description: "使用服务商默认值" },
			"0.01": { label: "0.01", description: "非常宽松" },
			"0.05": { label: "0.05", description: "均衡" },
			"0.1": { label: "0.1", description: "严格" },
		},
	},
	presencePenalty: {
		label: "Presence Penalty",
		description: "对引入已出现过的 token 施加的惩罚（-1 = 服务商默认值）",
		options: {
			"-1": { label: "Default", description: "使用服务商默认值" },
			"0": { label: "0", description: "无惩罚" },
			"0.5": { label: "0.5", description: "轻度求新" },
			"1": { label: "1", description: "鼓励求新" },
			"2": { label: "2", description: "强烈求新" },
		},
	},
	repetitionPenalty: {
		label: "Repetition Penalty",
		description: "对重复 token 施加的惩罚（-1 = 服务商默认值）",
		options: {
			"-1": { label: "Default", description: "使用服务商默认值" },
			"0.8": { label: "0.8", description: "允许重复" },
			"1": { label: "1", description: "无惩罚" },
			"1.1": { label: "1.1", description: "轻度惩罚" },
			"1.2": { label: "1.2", description: "均衡" },
			"1.5": { label: "1.5", description: "强惩罚" },
		},
	},
	textVerbosity: {
		label: "文本详略程度",
		description: "OpenAI Responses 与 Codex 的响应详略程度（low、medium 或 high）",
		options: {
			low: { label: "Low", description: "倾向于简洁回复" },
			medium: { label: "Medium", description: "在简洁与详尽之间取得平衡（默认）" },
			high: { label: "High", description: "倾向于详尽回复" },
		},
	},
	"tier.openai": {
		label: "服务层级 — OpenAI",
		description:
			"OpenAI / OpenAI-Codex 请求，以及经 OpenRouter 路由的 OpenAI 系模型的处理层级（none = 不发送）。以 `service_tier` 字段发送。",
		options: {
			none: { label: "None", description: "不发送 service_tier（标准处理）" },
			auto: { label: "Auto", description: "由服务商默认选择层级" },
			default: { label: "Default", description: "标准优先级处理" },
			flex: { label: "Flex", description: "更低成本、可用时延迟更高" },
			scale: { label: "Scale", description: "使用 Scale Tier 额度（如可用）" },
			priority: { label: "Priority", description: "更快、成本更高（高级请求）" },
		},
	},
	"tier.anthropic": {
		label: "服务层级 — Anthropic",
		description:
			'Claude 请求的处理层级。`priority` 会在受支持的直连 Anthropic 模型上启用快速模式（`speed: "fast"`）；在 Bedrock/Vertex Claude 及经 OpenRouter 时被忽略。',
		options: {
			none: { label: "None", description: "标准处理" },
			priority: {
				label: "Priority",
				description: '在受支持的直连 Claude 模型上启用快速模式（`speed: "fast"`）；在 Bedrock/Vertex 上被忽略',
			},
		},
	},
	"tier.google": {
		label: "服务层级 — Google",
		description:
			"Gemini（Google AI Studio + Vertex）请求，以及经 OpenRouter 路由的 Google 系模型的处理层级（none = 不发送）。以顶层 `serviceTier` 字段发送。",
		options: {
			none: { label: "None", description: "标准处理" },
			flex: { label: "Flex", description: "更低成本、更高延迟（Gemini API + Vertex）" },
			priority: { label: "Priority", description: "更快、更高可靠性（Gemini API + Vertex）" },
		},
	},
	"tier.subagent": {
		label: "服务层级 — 子智能体",
		description:
			"派生的 task/eval 子智能体使用的服务层级。Inherit = 跟随主智能体当前各模型系的实时层级（随 /fast 变化）；也可选定某个值，将其应用到子智能体所用模型所属的那个系。",
		options: {
			inherit: { label: "Inherit", description: "跟随主智能体当前各模型系的实时层级" },
			none: { label: "None", description: "标准处理" },
			auto: { label: "Auto", description: "由服务商默认选择层级（OpenAI 系）" },
			default: { label: "Default", description: "标准优先级处理（OpenAI 系）" },
			flex: { label: "Flex", description: "弹性容量层级（OpenAI/Google 系）" },
			scale: { label: "Scale", description: "使用 Scale Tier 额度（OpenAI 系）" },
			priority: { label: "Priority", description: "在派生模型所支持的每个系上均使用 Priority" },
		},
	},
	"tier.advisor": {
		label: "服务层级 — Advisor",
		description:
			"advisor 模型使用的服务层级。None = 标准处理；Inherit = 跟随主智能体当前各模型系的实时层级；也可选定某个值，将其应用到 advisor 模型所属的系。",
		options: {
			inherit: { label: "Inherit", description: "跟随主智能体当前各模型系的实时层级" },
			none: { label: "None", description: "标准处理" },
			auto: { label: "Auto", description: "由服务商默认选择层级（OpenAI 系）" },
			default: { label: "Default", description: "标准优先级处理（OpenAI 系）" },
			flex: { label: "Flex", description: "弹性容量层级（OpenAI/Google 系）" },
			scale: { label: "Scale", description: "使用 Scale Tier 额度（OpenAI 系）" },
			priority: { label: "Priority", description: "在派生模型所支持的每个系上均使用 Priority" },
		},
	},
	"retry.maxRetries": {
		label: "重试次数",
		description: "遇到 API 错误时的最大重试次数",
		options: {
			"1": { label: "1 次重试" },
			"2": { label: "2 次重试" },
			"3": { label: "3 次重试" },
			"5": { label: "5 次重试" },
			"10": { label: "10 次重试" },
		},
	},
	"retry.maxDelayMs": {
		label: "最大重试延迟",
		description:
			"重试之间的最大等待时间（毫秒）。当服务商要求等待时间超过此值，且无凭据或模型兜底成功时，请求会直接失败而不再继续等待（例如 Anthropic 3 小时的速率限制窗口）。",
	},
	"retry.modelFallback": {
		label: "重试模型兜底",
		description: "允许重试恢复流程切换到已配置的兜底模型",
	},
	"retry.usageAwareFallback": {
		label: "感知用量的兜底",
		description:
			"利用可靠的编码套餐用量报告，在触及硬性用量上限之前优先选用同服务商的其他账号，其次再用已配置的兜底模型。普通配置的 API key 不在此范围内。",
	},
	"retry.usageReservePct": {
		label: "预留余量",
		description: "当编码套餐模型的剩余百分比低于此值时视为接近上限。未知或未映射的用量将保持主模型不变。",
		options: {
			"5": { label: "5%", description: "仅在几乎耗尽时才动作" },
			"10": { label: "10%", description: "均衡的安全余量" },
			"15": { label: "15%", description: "保守" },
			"20": { label: "20%", description: "提前保护" },
			"25": { label: "25%", description: "非常保守" },
		},
	},
	"retry.usageReservePolicy": {
		label: "预留策略",
		description: "当同一服务商的所有编码套餐账号都进入预留余量范围内时应如何处理。",
		options: {
			confirm: {
				label: "交互式确认",
				description: "交互式会话在确认前保持在主模型上；后台智能体自动兜底",
			},
			auto: { label: "自动兜底", description: "始终选择下一个符合条件的已配置兜底模型" },
			"fail-closed": { label: "失败即止", description: "不消耗预留额度，也不选择兜底模型" },
		},
	},
	"retry.fallbackChains": {
		label: "重试兜底链",
		description:
			'一个 JSON 对象，将模型角色、模型选择器（"provider/model-id"）或服务商通配符（"provider/*"）映射到有序的兜底选择器列表，例如 {"default":["openai/gpt-4o-mini"],"google-antigravity/*":["google/*","google-vertex/*"]}。面向模型的键只要该模型/服务商处于激活状态就会生效，与角色无关；"provider/*" 条目会保留失败模型的 id 并替换服务商。带 id 前缀的通配符（"openrouter/google/*"）会为失败模型的裸 id 重新加前缀（google-antigravity/gemini-x -> openrouter/google/gemini-x），作为键时只匹配该前缀下该服务商的 id。',
	},
	"retry.fallbackRevertPolicy": {
		label: "兜底回退策略",
		description: "何时在切换到兜底模型后返回主模型",
		options: {
			"cooldown-expiry": { label: "冷却到期", description: "在其抑制窗口结束后返回主模型" },
			never: { label: "永不", description: "一直停留在兜底模型上，直到手动更改" },
		},
	},
	"providers.anthropic.serverSideFallback": {
		label: "Anthropic 服务端兜底（Fable 5）",
		description:
			"当 Claude Fable 5 / Mythos 5 请求被 Anthropic 安全分类器拦截时，在服务端重试为 Claude Opus 4.8（Anthropic `server-side-fallback-2026-06-01` beta）。此为可选启用项——保持关闭将维持兜底功能上线前的既有行为。",
	},
	"providers.autoThinkingModel": {
		label: "自动思考分类模型",
		description:
			"用于 `auto` 思考等级的难度分类器：默认在线运行（来自 /models 的 TINY 角色，否则用 smol），也可选择一个本地设备端模型",
		options: {
			online: {
				label: "在线（TINY 角色，否则 @smol）",
				description:
					"使用 TINY 角色模型（在 /models 中设置）或 @smol 在线分类提示词难度；不下载、不进行设备端推理。",
			},
			"qwen3-1.7b": {
				label: "Qwen3 1.7B",
				description: "已禁用本地推理：onnxruntime-node 无法运行该 ONNX 导出模型的 RotaryEmbedding 缓存更新。",
			},
			"llama3.2:3b": {
				label: "Llama 3.2 3B",
				description: "用于本地记忆/分类任务的更大规模 Llama 3.2 选项；质量潜力更高，但磁盘/内存/延迟成本也更高。",
			},
			"gemma-3-1b": {
				label: "Gemma 3 1B",
				description: "整合/去重效果最佳；体积更轻，但在抽取时会泄漏一些闲聊内容。",
			},
			"qwen2.5-1.5b": {
				label: "Qwen2.5 1.5B",
				description: "抽取粒度最佳（原子事实）；整合能力较弱。",
			},
			"lfm2-1.2b": {
				label: "LFM2 1.2B",
				description: "加载最快；整体表现均衡，但抽取标签略微更嘈杂。",
			},
		},
	},
	"providers.autoThinkingMaxEffort": {
		label: "自动思考上限",
		description:
			"`auto` 分类器可解析到的最高强度。`xhigh` 使分类器始终低于最高档一级，只有显式 `ultrathink` 才能达到 `max`；`max` 则允许分类器判定为特别复杂的轮次，在支持该档位的模型上计入最高档用量。",
		options: {
			xhigh: { label: "xhigh", description: "分类器最高停在 xhigh（默认）" },
			max: { label: "max", description: "分类器可在模型支持的情况下解析到 max" },
		},
	},
};
