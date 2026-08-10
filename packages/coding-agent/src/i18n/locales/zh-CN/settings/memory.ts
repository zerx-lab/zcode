import type { SettingTextDict } from "../../../types";

/** 设置面板「记忆」页词条。键为 setting path。 */
export const MEMORY_SETTINGS: SettingTextDict = {
	"memory.backend": {
		label: "记忆后端",
		description: "关闭、本地摘要流水线、Mnemopi SQLite 或 Hindsight 远程记忆",
		options: {
			off: { label: "关闭", description: "不运行任何记忆子系统" },
			local: { label: "本地", description: "本地会话摘要流水线（memory_summary.md）" },
			hindsight: { label: "Hindsight", description: "Vectorize 的 Hindsight 远程记忆服务" },
			mnemopi: { label: "Mnemopi", description: "本地 SQLite recall/retain 后端，可选启用嵌入（embeddings）" },
		},
	},
	"autolearn.enabled": {
		label: "自动学习（实验性）",
		description: "智能体停止后，引导其将经验记录到记忆中，并创建/完善独立的托管技能",
	},
	"autolearn.autoContinue": {
		label: "停止时自动运行捕获",
		description: "开启时，停止时自动运行一轮私有捕获（会消耗额外 token）。关闭时，仅保留常驻的自动学习引导。",
	},
	"mnemopi.dbPath": {
		label: "Mnemopi 数据库路径",
		description: "可选的 SQLite 数据库路径，默认使用智能体记忆目录。",
	},
	"mnemopi.bank": {
		label: "Mnemopi 记忆库",
		description: "可选的共享记忆库基础名称。按项目模式会基于此派生出项目本地记忆库。",
	},
	"mnemopi.scoping": {
		label: "Mnemopi 作用域",
		description:
			"global = 单个共享记忆库；per-project = 按 cwd 隔离的独立记忆库；per-project-tagged = 项目本地写入，同时可见全局 recall",
		options: {
			global: { label: "全局", description: "所有项目共享同一个 Mnemopi 记忆库" },
			"per-project": { label: "按项目", description: "按 cwd 目录名生成项目本地 Mnemopi 记忆库" },
			"per-project-tagged": {
				label: "按项目（打标签）",
				description: "写入项目本地记忆库，但合并项目与共享的 recall 结果",
			},
		},
	},
	"mnemopi.embeddingVariant": {
		label: "嵌入模型变体",
		description:
			"本地嵌入模型系列。en = 更强的英语模型；multilingual = 跨语言模型。修改此项会在下次启动时重建已有的记忆嵌入。",
		options: {
			en: { label: "英语（bge-base-en-v1.5）", description: "BAAI/bge-base-en-v1.5（768 维），仅支持英语" },
			multilingual: {
				label: "多语言（multilingual-e5-large）",
				description: "intfloat/multilingual-e5-large（1024 维），支持跨语言 recall",
			},
		},
	},
	"mnemopi.autoRecall": {
		label: "Mnemopi 自动 Recall",
		description: "在每个会话的第一轮自动 recall 本地记忆",
	},
	"mnemopi.autoRetain": {
		label: "Mnemopi 自动 Retain",
		description: "将已完成的对话轮次 retain 到本地 Mnemopi 记忆中",
	},
	"mnemopi.polyphonicRecall": {
		label: "Mnemopi 复调 Recall",
		description: "启用四路 recall（向量、图谱、事实、时间）并以 reciprocal rank fusion 融合结果",
	},
	"mnemopi.enhancedRecall": {
		label: "Mnemopi 增强 Recall",
		description: "为重复及相似的 recall 查询启用分级结果缓存",
	},
	"mnemopi.proactiveLinking": {
		label: "Mnemopi 主动关联",
		description: "在新记忆存储时即时写入情景图谱，并与相关实体和记忆建立关联",
	},
	"mnemopi.noEmbeddings": {
		label: "Mnemopi 禁用嵌入",
		description: "强制使用确定性的纯 FTS recall，而非向量嵌入",
	},
	"mnemopi.embeddingModel": {
		label: "Mnemopi 嵌入模型",
		description: "高级选项：显式指定嵌入模型 ID 以覆盖变体设置。留空则使用 mnemopi.embeddingVariant。",
	},
	"mnemopi.embeddingApiUrl": {
		label: "Mnemopi 嵌入 API 地址",
		description: "传递给 Mnemopi 的可选 OpenAI 兼容嵌入端点",
	},
	"mnemopi.embeddingApiKey": {
		label: "Mnemopi 嵌入 API 密钥",
		description: "传递给 Mnemopi 的可选嵌入 API 密钥",
	},
	"mnemopi.llmMode": {
		label: "Mnemopi LLM 模式",
		description: "选择不使用 LLM、在线小模型（/models 中的 TINY 角色，否则用 @smol），或远程 OpenAI 兼容端点",
		options: {
			none: { label: "无", description: "禁用 Mnemopi 基于 LLM 的抽取" },
			smol: { label: "在线（小模型）", description: "使用在线小模型（/models 中的 TINY 角色，否则用 @smol）" },
			remote: { label: "远程", description: "使用下方的 Mnemopi 远程 LLM 设置" },
		},
	},
	"mnemopi.llmBaseUrl": {
		label: "Mnemopi LLM 基础地址",
		description: "Mnemopi 远程模式使用的可选 OpenAI 兼容 LLM 端点",
	},
	"mnemopi.llmApiKey": {
		label: "Mnemopi LLM API 密钥",
		description: "Mnemopi 远程模式使用的可选 LLM API 密钥",
	},
	"mnemopi.llmModel": {
		label: "Mnemopi LLM 模型",
		description: "Mnemopi 远程模式使用的可选 LLM 模型名称",
	},
	"hindsight.apiUrl": {
		label: "Hindsight API 地址",
		description: "Hindsight 服务器地址（云端或自托管）",
	},
	"hindsight.apiToken": {
		label: "Hindsight API 令牌",
		description: "用于 Hindsight 服务器鉴权的 Bearer 令牌",
	},
	"hindsight.bankId": {
		label: "Hindsight 记忆库 ID",
		description: "记忆库标识符（默认使用项目名）",
	},
	"hindsight.scoping": {
		label: "Hindsight 作用域",
		description:
			"global = 单个共享记忆库；per-project = 按 cwd 隔离的独立记忆库；per-project-tagged = 带项目标签的共享记忆库，recall 时合并全局与项目记忆",
		options: {
			global: { label: "全局", description: "单个共享记忆库 — 所有项目可见相同的记忆" },
			"per-project": { label: "按项目", description: "按 cwd 目录名隔离记忆库 — 各项目之间互不可见" },
			"per-project-tagged": {
				label: "按项目（打标签）",
				description: "共享记忆库，retain 时打上 project:<cwd> 标签。recall 会同时呈现项目记忆与未打标签的全局记忆",
			},
		},
	},
	"hindsight.autoRecall": {
		label: "Hindsight 自动 Recall",
		description: "在每个会话的第一轮自动 recall 记忆",
	},
	"hindsight.autoRetain": {
		label: "Hindsight 自动 Retain",
		description: "每 N 轮及会话边界处自动 retain 会话记录",
	},
	"hindsight.retainMode": {
		label: "Hindsight Retain 模式",
		description: "full-session = 每个会话 upsert 一份文档，last-turn = 分块保留",
		options: {
			"full-session": { label: "完整会话", description: "每个会话 upsert 一份文档（推荐）" },
			"last-turn": { label: "最后一轮", description: "按轮次边界分块保留" },
		},
	},
	"hindsight.mentalModelsEnabled": {
		label: "Hindsight 心智模型",
		description:
			"启动时将精选的 reflect 摘要（心智模型）读入开发者指令。仅加载记忆库中已有的模型，不进行写入。搭配 hindsight.mentalModelAutoSeed 可同时自动创建内置的种子集。",
	},
	"hindsight.mentalModelAutoSeed": {
		label: "Hindsight 心智模型自动播种",
		description:
			"会话开始时，为记忆库中尚不存在的内置心智模型（project-conventions、project-decisions、user-preferences）创建条目。",
	},
	"providers.memoryModel": {
		label: "记忆模型",
		description:
			"用于事实抽取与整合的 Mnemopi LLM：默认在线（/models 中的 TINY 角色，否则 smol/remote），也可选本地端上模型",
		options: {
			online: {
				label: "在线（TINY 角色，否则 @smol）",
				description:
					"使用在线模型：设置时用 /models 中的 TINY 角色，否则用 @smol。不下载本地模型，也不进行端上推理。",
			},
			"qwen3-1.7b": {
				label: "Qwen3 1.7B",
				description: "已禁用本地推理：onnxruntime-node 无法运行该 ONNX 导出模型的 RotaryEmbedding 缓存更新。",
			},
			"llama3.2:3b": {
				label: "Llama 3.2 3B",
				description: "用于本地记忆/分类任务的更大号 Llama 3.2 选项；质量潜力更高，但磁盘/内存/延迟开销也更高。",
			},
			"gemma-3-1b": {
				label: "Gemma 3 1B",
				description: "整合/去重效果最佳，体积更轻，但抽取时会混入闲聊内容。",
			},
			"qwen2.5-1.5b": {
				label: "Qwen2.5 1.5B",
				description: "抽取粒度最佳（原子事实），但整合能力较弱。",
			},
			"lfm2-1.2b": {
				label: "LFM2 1.2B",
				description: "加载最快，综合表现均衡，但抽取标签略有噪声。",
			},
		},
	},
};
