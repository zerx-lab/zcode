import type { SettingTextDict } from "../../../types";

/** Web 搜索服务商选项，供 webSearchOrder / webSearchExclude 共用。 */
const WEB_SEARCH_PROVIDER_OPTIONS = {
	perplexity: {
		label: "Perplexity",
		description: "已配置鉴权时使用；显式选择时会退回匿名搜索",
	},
	gemini: {
		label: "Gemini",
		description: "通过 Gemini 进行 Google 搜索关联（使用 google-gemini-cli 或 google-antigravity OAuth）",
	},
	anthropic: {
		label: "Anthropic",
		description: "Claude 原生 web_search 工具（使用 Anthropic OAuth 或 ANTHROPIC_API_KEY）",
	},
	codex: {
		label: "OpenAI",
		description: "OpenAI 原生 web_search（通过 /login openai-codex 使用 ChatGPT OAuth）",
	},
	xai: {
		label: "xAI",
		description:
			"通过 xAI Responses API 使用 Grok 网页搜索（通过 /login xai-oauth 使用 SuperGrok/X Premium+ OAuth，或使用 XAI_API_KEY）",
	},
	zai: {
		label: "Z.AI",
		description: "调用 Z.AI 的 webSearchPrime MCP",
	},
	exa: {
		label: "Exa",
		description: "通过 /login exa 或 EXA_API_KEY 使用 API；显式选择时可通过 MCP 免密钥兜底",
	},
	tinyfish: {
		label: "TinyFish",
		description: "需要 TINYFISH_API_KEY",
	},
	jina: {
		label: "Jina",
		description: "需要 JINA_API_KEY",
	},
	kagi: {
		label: "Kagi",
		description: "需要 KAGI_API_KEY 及 Kagi Search API 公测权限",
	},
	tavily: {
		label: "Tavily",
		description: "需要 TAVILY_API_KEY",
	},
	firecrawl: {
		label: "Firecrawl",
		description: "设置了 FIRECRAWL_API_KEY 时使用 Firecrawl API；否则退回免密钥模式",
	},
	brave: {
		label: "Brave",
		description: "需要 BRAVE_API_KEY",
	},
	kimi: {
		label: "Kimi",
		description:
			"Kimi Code 搜索（需要通过 KIMI_SEARCH_API_KEY/MOONSHOT_SEARCH_API_KEY 或 /login kimi-code 提供 Kimi Code Console 密钥；不是 MOONSHOT_API_KEY）",
	},
	parallel: {
		label: "Parallel",
		description: "需要 PARALLEL_API_KEY",
	},
	synthetic: {
		label: "Synthetic",
		description: "需要 SYNTHETIC_API_KEY",
	},
	searxng: {
		label: "SearXNG",
		description: "需要 SEARXNG_ENDPOINT 或 searxng.endpoint",
	},
	startpage: {
		label: "Startpage",
		description: "免密钥抓取 Startpage（基于 Google）搜索结果；可能触发反爬验证",
	},
	duckduckgo: {
		label: "DuckDuckGo",
		description: "免密钥尽力而为兜底方案；在数据中心/共享出口 IP 上可能触发反爬验证",
	},
	ecosia: {
		label: "Ecosia",
		description: "基于浏览器的免密钥抓取，获取 Ecosia（基于 Google）搜索结果",
	},
	google: {
		label: "Google",
		description: "基于浏览器的免密钥兜底方案；速度较慢且可能触发反爬验证",
	},
	mojeek: {
		label: "Mojeek",
		description: "基于浏览器的免密钥抓取，获取 Mojeek 独立索引的结果",
	},
	public: {
		label: "Public Web",
		description: "并行查询所有免密钥引擎，并对结果去重合并",
	},
} satisfies Record<string, { label: string; description?: string }>;

/** Kokoro 语音音色选项，供本地 TTS 与语音朗读共用。 */
const KOKORO_VOICE_OPTIONS = {
	af_heart: { label: "Heart（美式女声）" },
	af_bella: { label: "Bella（美式女声）" },
	af_nicole: { label: "Nicole（美式女声）" },
	af_aoede: { label: "Aoede（美式女声）" },
	af_kore: { label: "Kore（美式女声）" },
	af_sarah: { label: "Sarah（美式女声）" },
	am_michael: { label: "Michael（美式男声）" },
	am_fenrir: { label: "Fenrir（美式男声）" },
	am_puck: { label: "Puck（美式男声）" },
	bf_emma: { label: "Emma（英式女声）" },
	bm_george: { label: "George（英式男声）" },
	bm_fable: { label: "Fable（英式男声）" },
} satisfies Record<string, { label: string; description?: string }>;

/** 设置面板「服务商」页词条。键为 setting path。 */
export const PROVIDERS_SETTINGS: SettingTextDict = {
	"providers.maxInFlightRequests": {
		label: "最大并发请求数",
		description:
			'每个 provider id（例如 "openai" 或 "anthropic"）允许的最大并发 LLM 请求数，在使用同一配置根目录的本地 OMP 进程间共享。未列出的 provider 不受限制。',
	},
	"secrets.enabled": {
		label: "隐藏密钥",
		description: "在发送给 AI 服务商前混淆已配置的密钥，并遮蔽疑似凭据的 token",
	},
	"providers.ollama-cloud.maxConcurrency": {
		label: "Ollama Cloud 最大并发数",
		description: "每个进程中 Ollama Cloud 子智能体的最大并发运行数；0 表示禁用该 provider 专属限制",
	},
	"providers.webSearchOrder": {
		label: "网页搜索服务商顺序",
		description: "web_search 工具的优先服务商顺序；未列出的服务商保留其默认顺序，排在之后",
		options: WEB_SEARCH_PROVIDER_OPTIONS,
	},
	"providers.webSearchExclude": {
		label: "排除的网页搜索服务商",
		description: "web_search 绝不使用的服务商，即便作为兜底也不用",
		options: WEB_SEARCH_PROVIDER_OPTIONS,
	},
	"providers.webSearchTimeoutSeconds": {
		label: "网页搜索超时",
		description: "web_search 转向下一个兜底服务商前，单个服务商搜索传输的硬性超时时间，单位秒（最大 300）",
		options: {
			"30": { label: "30 秒" },
			"60": { label: "1 分钟" },
			"120": { label: "2 分钟" },
			"180": { label: "3 分钟" },
			"300": { label: "5 分钟" },
		},
	},
	"providers.webSearchGeminiModel": {
		label: "Gemini web_search 模型",
		description: "用于 Gemini Google 搜索关联的模型 ID。默认为 gemini-2.5-flash。",
	},
	"providers.antigravityEndpoint": {
		label: "Antigravity 端点模式",
		description: "google-antigravity 服务商的端点路由策略（chat、search、image、discovery）",
		options: {
			auto: { label: "自动", description: "优先尝试生产端点，遇到 5xx/429 时故障转移到沙盒端点" },
			production: { label: "仅生产环境", description: "强制仅使用生产端点" },
			sandbox: { label: "仅沙盒环境", description: "强制仅使用沙盒端点" },
		},
	},
	"providers.imageOrder": {
		label: "图像生成服务商顺序",
		description: "图像生成的优先服务商顺序；未列出的服务商沿用当前会话服务商及内置顺序",
		options: {
			openai: {
				label: "OpenAI",
				description: "OPENAI_API_KEY（gpt-image-2）或当前 GPT 模型；无法使用时退回已连接的 Codex 订阅",
			},
			"openai-codex": {
				label: "OpenAI Codex（ChatGPT）",
				description: "使用已连接的 Codex / ChatGPT 订阅 —— 无需 OPENAI_API_KEY",
			},
			antigravity: { label: "Antigravity", description: "需要 google-antigravity OAuth" },
			xai: { label: "xAI Grok Imagine", description: "需要 xAI Grok OAuth 或 XAI_API_KEY" },
			gemini: { label: "Gemini", description: "需要 GEMINI_API_KEY" },
			openrouter: { label: "OpenRouter", description: "需要 OPENROUTER_API_KEY" },
		},
	},
	"providers.fireworksTier": {
		label: "Fireworks 服务等级",
		description:
			'Fireworks 请求的服务路径。Priority 会发送 `service_tier: "priority"`，在流量高峰期以更高价格换取更高可靠性；Standard 不发送该字段。Fast（`-fast`）模型忽略此设置 —— Fast 本身就是独立的服务路径。',
		options: {
			standard: { label: "Standard", description: "默认服务路径（不带 service_tier）" },
			priority: { label: "Priority", description: "优先服务路径：更高可靠性，按 token 计费更贵" },
		},
	},
	"live.voice": {
		label: "实时语音音色",
		description: "由 Codex 支持的实时语音会话所使用的音色",
		options: {
			arbor: { label: "Arbor" },
			breeze: { label: "Breeze" },
			cove: { label: "Cove" },
			ember: { label: "Ember" },
			juniper: { label: "Juniper" },
			maple: { label: "Maple" },
			sol: { label: "Sol" },
			spruce: { label: "Spruce" },
			vale: { label: "Vale" },
		},
	},
	"providers.tts": {
		label: "文本转语音服务商",
		description: "tts 工具的后端：本地设备端神经网络 TTS（Kokoro-82M）或 xAI Grok Voice",
		options: {
			auto: { label: "Auto", description: "优先使用本地设备端 TTS；存在凭据时将 .mp3 输出路由到 xAI" },
			local: { label: "Local", description: "设备端神经网络 TTS（Kokoro-82M）；输出为 WAV/PCM16" },
			xai: { label: "xAI Grok Voice", description: "需要 xAI Grok OAuth 或 XAI_API_KEY；输出 MP3 或 WAV" },
		},
	},
	"tts.localModel": {
		label: "本地 TTS 模型",
		description: "本地 TTS 后端使用的设备端神经网络 TTS 模型（Kokoro-82M）",
		options: {
			kokoro: {
				label: "Kokoro-82M",
				description: "Kokoro-82M 神经网络 TTS —— 设备端 SoTA 音质，多音色，完全本地运行",
			},
		},
	},
	"tts.localVoice": {
		label: "本地 TTS 音色",
		description: "本地 TTS 后端使用的 Kokoro 音色（美式/英式，女声/男声）",
		options: KOKORO_VOICE_OPTIONS,
	},
	"speech.enabled": {
		label: "语音朗读",
		description: "在助手输出流式生成时通过扬声器朗读出来",
	},
	"speech.mode": {
		label: "语音朗读模式",
		description: "朗读内容范围：all = 助手消息 + 思考过程；assistant = 仅消息；yield = 仅在回合结束时朗读最终消息",
		options: {
			all: { label: "全部（消息 + 思考过程）" },
			assistant: { label: "助手消息" },
			yield: { label: "仅最终消息" },
		},
	},
	"speech.enhanced": {
		label: "增强语音改写",
		description:
			"在合成前用 tiny/smol 模型将助手输出改写为自然口语化文本（描述代码内容，去除链接与 Markdown）。改写失败时退回机械清理",
	},
	"speech.voice": {
		label: "语音朗读音色",
		description: "朗读助手输出时使用的 Kokoro 音色",
		options: KOKORO_VOICE_OPTIONS,
	},
	"providers.tinyModel": {
		label: "微型模型",
		description: "会话标题生成模型：默认在线（使用 /models 中的 TINY 角色，否则用 @smol），也可选择本地设备端模型",
		options: {
			online: {
				label: "在线（TINY 角色，否则 @smol）",
				description:
					"在线生成标题：已设置时使用 TINY 模型角色（在 /models 中设置），否则退回在线兜底（先 commit 角色，再 @smol）。不涉及本地下载或设备端推理。",
			},
			"lfm2-350m": { label: "LFM2 350M", description: "推荐的本地模型；速度与质量平衡最佳，缓存约 212 MB。" },
			"qwen3-0.6b": { label: "Qwen3 0.6B", description: "最稳健的本地选项；首次加载较慢，缓存约 500 MB。" },
			"gemma-270m": { label: "Gemma 270M", description: "可用的最小本地选项；质量较低，缓存占用最小。" },
			"qwen2.5-0.5b": { label: "Qwen2.5 0.5B", description: "均衡的本地兜底选项；质量与缓存占用适中。" },
			"lfm2-700m": { label: "LFM2 700M", description: "质量最高的本地选项；比 LFM2 350M 更大更慢。" },
		},
	},
	"providers.tinyModelDevice": {
		label: "微型模型设备",
		description:
			"本地微型模型（标题生成 + 记忆）使用的 ONNX 执行提供者。默认仅使用 CPU 推理。PI_TINY_DEVICE 环境变量可覆盖此设置。",
		options: {
			default: { label: "默认", description: "仅 CPU 推理" },
			gpu: { label: "GPU", description: "加速提供者（WebGPU/Metal、CUDA 或 DirectML）" },
			cpu: { label: "CPU", description: "仅 CPU 推理" },
			metal: { label: "Metal", description: "面向 Apple GPU 的 WebGPU 别名" },
			webgpu: { label: "WebGPU", description: "WebGPU/Metal 后端" },
			cuda: { label: "CUDA", description: "NVIDIA CUDA（Linux x64）" },
			dml: { label: "DirectML", description: "DirectML 后端（Windows）" },
			coreml: { label: "CoreML", description: "Apple CoreML（需手动启用；可能加载失败）" },
			auto: { label: "自动", description: "由 ONNX Runtime 自行选择提供者" },
			wasm: { label: "WASM", description: "WebAssembly 后端" },
			webnn: { label: "WebNN", description: "WebNN 后端" },
			"webnn-gpu": { label: "WebNN GPU", description: "WebNN GPU 设备" },
			"webnn-cpu": { label: "WebNN CPU", description: "WebNN CPU 设备" },
			"webnn-npu": { label: "WebNN NPU", description: "WebNN NPU 设备" },
		},
	},
	"providers.tinyModelDtype": {
		label: "微型模型精度",
		description:
			"本地微型模型的 ONNX 量化/精度设置。默认使用各模型自带的 dtype（q4）；精度越低速度越快，精度越高越接近原始效果。PI_TINY_DTYPE 环境变量可覆盖此设置。",
		options: {
			default: { label: "默认", description: "各模型自带的 dtype（当前为 q4）" },
			q4: { label: "q4", description: "4 位权重；最小最快" },
			q4f16: { label: "q4f16", description: "4 位权重 + fp16 激活值" },
			q8: { label: "q8", description: "8 位量化" },
			fp16: { label: "fp16", description: "16 位浮点；精度更高，体积更大" },
			fp32: { label: "fp32", description: "全精度；最大最慢" },
			int8: { label: "int8", description: "有符号 8 位整数" },
			uint8: { label: "uint8", description: "无符号 8 位整数" },
			bnb4: { label: "bnb4", description: "bitsandbytes 4 位" },
			q2: { label: "q2", description: "2 位权重" },
			q2f16: { label: "q2f16", description: "2 位权重 + fp16 激活值" },
			q1: { label: "q1", description: "1 位权重" },
			q1f16: { label: "q1f16", description: "1 位权重 + fp16 激活值" },
			auto: { label: "自动", description: "由 transformers.js 根据设备自行选择" },
		},
	},
	"providers.unexpectedStopModel": {
		label: "异常中断检测模型",
		description:
			"用于异常中断检测的分类模型：默认在线（使用 /models 中的 TINY 角色，否则用 smol），也可选择本地设备端模型。",
		options: {
			online: {
				label: "在线（TINY 角色，否则 @smol）",
				description:
					"使用在线模型：已设置时使用 /models 中的 TINY 角色，否则使用 @smol。不涉及本地模型下载或设备端推理。",
			},
			"qwen3-1.7b": {
				label: "Qwen3 1.7B",
				description: "已禁用本地推理：onnxruntime-node 无法运行该 ONNX 导出模型的 RotaryEmbedding 缓存更新。",
			},
			"llama3.2:3b": {
				label: "Llama 3.2 3B",
				description: "更大的 Llama 3.2 选项，用于本地记忆/分类任务；质量潜力更高，但磁盘/内存/延迟开销也更大。",
			},
			"gemma-3-1b": {
				label: "Gemma 3 1B",
				description: "整合/去重效果最佳；占用较轻，但提取过程中会混入闲聊内容。",
			},
			"qwen2.5-1.5b": {
				label: "Qwen2.5 1.5B",
				description: "提取粒度最佳（原子事实）；整合能力较弱。",
			},
			"lfm2-1.2b": {
				label: "LFM2 1.2B",
				description: "加载最快；综合表现稳健，提取标签略有噪声。",
			},
		},
	},
	"providers.kimiApiFormat": {
		label: "Kimi API 格式",
		description: "Kimi Code 服务商使用的 API 格式（auto 跟随实时模型元数据）",
		options: {
			auto: { label: "自动", description: "使用模型服务端声明的协议" },
			openai: { label: "OpenAI", description: "api.kimi.com" },
			anthropic: { label: "Anthropic", description: "api.moonshot.ai" },
		},
	},
	"providers.openaiWebsockets": {
		label: "OpenAI WebSocket",
		description: "OpenAI Codex 模型的 WebSocket 策略（auto 使用模型默认值，on 强制启用，off 禁用）",
		options: {
			auto: { label: "自动", description: "使用模型/服务商默认的 WebSocket 行为" },
			off: { label: "关闭", description: "禁用 OpenAI Codex 模型的 WebSocket" },
			on: { label: "开启", description: "强制为 OpenAI Codex 模型启用 WebSocket" },
		},
	},
	"providers.streamFirstEventTimeoutSeconds": {
		label: "流式首个事件超时",
		description: "等待模型流首个事件的秒数；-1 使用服务商/环境变量默认值，0 表示禁用看门狗",
		options: {
			"-1": { label: "自动", description: "使用服务商默认值及 PI_* 超时环境变量" },
			"0": { label: "关闭", description: "禁用首个事件超时" },
			"300": { label: "5 分钟" },
			"600": { label: "10 分钟" },
			"1800": { label: "30 分钟" },
		},
	},
	"providers.streamIdleTimeoutSeconds": {
		label: "流式空闲超时",
		description: "模型流事件之间允许保持静默的秒数；-1 使用服务商/环境变量默认值，0 表示禁用看门狗",
		options: {
			"-1": { label: "自动", description: "使用服务商默认值及 PI_* 超时环境变量" },
			"0": { label: "关闭", description: "禁用空闲超时" },
			"300": { label: "5 分钟" },
			"600": { label: "10 分钟" },
			"1800": { label: "30 分钟" },
		},
	},
	"providers.openrouterVariant": {
		label: "OpenRouter 路由",
		description: "附加到 OpenRouter 模型 ID 后的默认路由变体后缀（若选择器已指定变体则会被覆盖）",
		options: {
			default: { label: "默认", description: "不添加后缀；使用 OpenRouter 的默认路由" },
			nitro: { label: ":nitro", description: "优先考虑吞吐量 / 最低延迟" },
			floor: { label: ":floor", description: "优先选择价格最低的可用服务商" },
			online: { label: ":online", description: "启用 OpenRouter 的网页搜索插件" },
			exacto: { label: ":exacto", description: "精选高质量服务商（仅部分模型支持）" },
		},
	},
	"providers.fetch": {
		label: "网页抓取服务商",
		description: "fetch/read URL 工具使用的阅读器后端优先级",
		options: {
			auto: { label: "自动", description: "优先级：native > trafilatura > lynx > parallel > jina" },
			native: { label: "内置", description: "进程内 HTML→Markdown 转换器（始终可用）" },
			trafilatura: { label: "Trafilatura", description: "通过 uv/pip 自动安装" },
			lynx: { label: "Lynx", description: "需要 lynx 系统包" },
			parallel: { label: "Parallel", description: "需要 PARALLEL_API_KEY" },
			jina: { label: "Jina", description: "使用 r.jina.ai 阅读器（JINA_API_KEY 可选）" },
		},
	},
	"codexResets.autoRedeem": {
		label: "Codex 自动兑换已保存重置",
		description:
			"自动消耗已保存的 Codex 速率限制重置额度：当回合卡住且没有其他账号可以接管时，恢复因 5 小时或每周窗口耗尽而被阻塞的账号，并抢救即将过期的额度。unset 会在首次消耗前询问，yes 无需确认直接消耗，no 关闭两项检查。",
		options: {
			unset: { label: "未设置", description: "检查是否符合条件，并在首次消耗已保存重置额度前询问。" },
			yes: { label: "是", description: "无需确认，直接消耗符合条件的已保存重置额度。" },
			no: { label: "否", description: "不运行已保存重置额度的自动兑换检查。" },
		},
	},
	"codexResets.minBlockedMinutes": {
		label: "Codex 自动兑换最小阻塞时长",
		description:
			"仅当自然解锁时间——即耗尽的 5 小时/每周窗口中最晚的重置时间——距离当前至少还有这么多分钟时才自动兑换（避免为节省短暂等待而浪费稀缺额度）。调高此值（例如 360）可忽略仅由 5 小时窗口造成的阻塞。",
	},
	"codexResets.keepCredits": {
		label: "Codex 自动兑换保留额度",
		description:
			"已保存重置额度低于此数量时绝不自动消耗（0 表示最后一份额度也可自动消耗）。即将过期的额度不受此限制 —— 保留一份即将过期的额度毫无意义。",
	},
	"codexResets.salvageHorizonHours": {
		label: "Codex 重置抢救时限",
		description:
			"当某份已保存的 Codex 重置额度将在此时限（小时）内过期，且任一对话窗口（5 小时或每周）存在值得恢复的可观用量时，自动消耗该额度（0 表示禁用过期抢救）。",
	},
	"provider.appendOnlyContext": {
		label: "仅追加上下文",
		description:
			"缓存 system prompt 与工具规格，并保持消息日志仅追加，使服务商的前缀缓存（DeepSeek、Xiaomi/SGLang、Anthropic）能以最高命中率生效。Auto 会为已知支持前缀缓存的服务商自动启用。",
		options: {
			auto: { label: "自动", description: "为已知支持前缀缓存的服务商启用（推荐）" },
			on: { label: "开启", description: "始终启用仅追加上下文" },
			off: { label: "关闭", description: "禁用仅追加上下文" },
		},
	},
	"exa.enabled": {
		label: "Exa",
		description: "启用 Exa 网页搜索服务商",
	},
	"exa.searchDelayMs": {
		label: "Exa 搜索延迟",
		description: "Exa 网页搜索请求之间的最小延迟，单位毫秒；设为 0 可禁用节流",
	},
	"searxng.endpoint": {
		label: "SearXNG 端点",
		description: "用于网页搜索的自托管 SearXNG 实例基础 URL",
	},
};
