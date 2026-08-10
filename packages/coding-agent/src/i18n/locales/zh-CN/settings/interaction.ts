import type { SettingTextDict } from "../../../types";

/** 设置面板「交互」页词条。键为 setting path。 */
export const INTERACTION_SETTINGS: SettingTextDict = {
	autoResume: {
		label: "自动恢复",
		description: "自动恢复当前目录中最近的会话",
	},
	"power.sleepPrevention": {
		label: "防止休眠",
		description: "在会话进行期间阻止 macOS 休眠。级别逐级累加——每一级都包含所有更低级别的标志。",
		options: {
			off: { label: "关闭", description: "不阻止任何休眠" },
			idle: { label: "阻止空闲休眠", description: "在会话打开期间保持系统唤醒（caffeinate -i）" },
			display: {
				label: "阻止显示器休眠",
				description: "同时阻止显示器空闲休眠（caffeinate -i -d）",
			},
			system: {
				label: "阻止系统休眠",
				description: "同时阻止交流电源下的所有系统休眠，并声明用户处于活跃状态（caffeinate -i -d -s -u）",
			},
		},
	},
	"git.enabled": {
		label: "启用 Git 集成",
		description: "在 TUI 中显示 git 分支、状态与 PR 信息，并监视仓库元数据。",
	},
	steeringMode: {
		label: "引导模式",
		description: "智能体工作期间如何处理排队消息",
	},
	followUpMode: {
		label: "后续消息模式",
		description: "回合结束后如何处理排队的后续消息",
	},
	interruptMode: {
		label: "中断模式",
		description: "引导消息何时中断工具执行",
	},
	"loop.mode": {
		label: "循环模式",
		description: "/loop 迭代之间、重新提交提示词前发生的操作",
		options: {
			prompt: { label: "提示词", description: "重新提交提示词作为后续消息（当前行为）" },
			compact: { label: "压缩", description: "压缩会话上下文，然后重新提交提示词" },
			reset: { label: "重置", description: "开始新会话，然后重新提交提示词" },
		},
	},
	doubleEscapeAction: {
		label: "双击 Escape 操作",
		description: "编辑器为空时连按两次 Escape 触发的操作",
	},
	treeFilterMode: {
		label: "会话树过滤器",
		description: "打开会话树时的默认过滤模式",
	},
	autocompleteMaxVisible: {
		label: "自动补全条目数",
		description: "自动补全下拉列表中最多可见的条目数（3-20）",
		options: {
			"3": { label: "3 项" },
			"5": { label: "5 项" },
			"7": { label: "7 项" },
			"10": { label: "10 项" },
			"15": { label: "15 项" },
			"20": { label: "20 项" },
		},
	},
	emojiAutocomplete: {
		label: "表情符号自动补全",
		description: "根据 `:name:` 短代码建议表情符号，并展开文本表情符号，如 `:D` 或 `:-)`",
	},
	"paste.largeMenuThreshold": {
		label: "大粘贴菜单",
		description:
			"粘贴内容达到此行数时，提供菜单以将其包装为代码块、包装为 XML 标签，或保存为文件。0 表示禁用该菜单（大粘贴内容仍会折叠为 [Paste] 标记）。",
		options: {
			"0": { label: "关闭" },
			"100": { label: "100 行" },
			"250": { label: "250 行" },
			"500": { label: "500 行" },
			"1000": { label: "1000 行" },
		},
	},
	"startup.quiet": {
		label: "安静启动",
		description: "跳过欢迎界面与启动状态消息",
	},
	"startup.showSplash": {
		label: "显示启动画面",
		description: "在正常交互式启动时显示完整的动画设置画面，但不重新运行设置。安静启动仍会抑制它。",
	},
	"startup.setupWizard": {
		label: "设置向导",
		description: "每个设置版本仅显示一次新增的引导步骤",
	},
	"startup.checkUpdate": {
		label: "检查更新",
		description: "启动时检查 omp 更新",
	},
	"marketplace.autoUpdate": {
		label: "插件市场自动更新",
		description: "启动时检查插件更新",
		options: {
			off: { label: "关闭", description: "不检查插件更新" },
			notify: { label: "通知", description: "启动时检查并在有更新时通知" },
			auto: { label: "自动", description: "启动时检查并自动安装更新" },
		},
	},
	"startup.changelogMode": {
		label: "启动更新日志",
		description: "选择更新说明启动时以摘要显示、完整显示，还是隐藏",
		options: {
			summary: { label: "摘要", description: "显示发布与变更数量，并附带 /changelog 提示" },
			expanded: { label: "展开", description: "完整显示最近的发布说明" },
			hidden: { label: "隐藏", description: "启动时不显示发布说明" },
		},
	},
	"magicKeywords.enabled": {
		label: "魔法关键词",
		description: "为独立出现的 ultrathink、orchestrate 与 workflowz 关键词启用隐藏提示",
	},
	"magicKeywords.ultrathink": {
		label: "Ultrathink 关键词",
		description: "允许独立出现的 ultrathink 请求最大自动思考，并附加其隐藏提示",
	},
	"magicKeywords.orchestrate": {
		label: "Orchestrate 关键词",
		description: "允许独立出现的 orchestrate 附加其隐藏的多智能体编排提示",
	},
	"magicKeywords.workflow": {
		label: "Workflow 关键词",
		description: "允许独立出现的 workflowz 附加其隐藏的 eval 工作流提示",
	},
	"completion.notify": {
		label: "完成通知",
		description: "智能体完成一个回合时发送通知",
	},
	"error.notify": {
		label: "错误通知",
		description: "智能体因错误停止时发送通知",
	},
	"ask.timeout": {
		label: "Ask 超时",
		description: "经过此秒数后自动选择推荐的 ask 选项（0 表示禁用）",
		options: {
			"0": { label: "禁用" },
			"15": { label: "15 秒" },
			"30": { label: "30 秒" },
			"60": { label: "60 秒" },
			"120": { label: "120 秒" },
		},
	},
	"ask.notify": {
		label: "Ask 通知",
		description: "ask 工具等待输入时发送通知",
	},
	"recap.enabled": {
		label: "空闲摘要",
		description: "终端空闲一段时间后，生成一段简短的 LLM 现状摘要",
	},
	"recap.idleSeconds": {
		label: "空闲摘要延迟",
		description: "空闲多少秒后显示摘要",
		options: {
			"60": { label: "1 分钟" },
			"120": { label: "2 分钟" },
			"240": { label: "4 分钟" },
			"300": { label: "5 分钟" },
			"600": { label: "10 分钟" },
		},
	},
	"collab.relayUrl": {
		label: "中继 URL",
		description: "/collab 使用的中继地址（wss://host[:port]）",
	},
	"collab.webUrl": {
		label: "Web UI 地址",
		description: "/collab 链接使用的浏览器 UI；留空则从 collab.relayUrl 推导；显式 http:// 仅限本地",
	},
	"collab.displayName": {
		label: "显示名称",
		description: "展示给其他 collab 参与者的名称（默认：操作系统用户名）",
	},
	"share.serverUrl": {
		label: "分享服务器",
		description: "/share 使用的分享查看器/上传基础地址（加密 blob 上传 + 查看器；链接形式为 <base>/<id>#<key>）",
	},
	"share.store": {
		label: "分享存储方式",
		description: "/share 上传加密会话 blob 的位置",
		options: {
			blob: {
				label: "加密 Blob",
				description: "上传到分享服务器（无需 GitHub 账号；避免 gist API 速率限制）",
			},
			gist: {
				label: "GitHub Gist",
				description: "推送到私密 gist（需要已认证的 gh），失败时回退到分享服务器",
			},
		},
	},
	"share.redactSecrets": {
		label: "分享敏感信息遮蔽",
		description: "在上传前对 /share 快照运行敏感信息混淆器（使用 secrets.* 配置）",
	},
	"stt.enabled": {
		label: "语音转文字",
		description: "启用通过麦克风的语音转文字输入",
	},
	"stt.modelName": {
		label: "语音模型",
		description:
			"本地设备端语音模型。Parakeet TDT v3（sherpa-onnx）是 SoTA 默认模型；Whisper base/small/large-v3-turbo 各档在体积与多语言覆盖之间权衡。首次使用时下载。",
		options: {
			fast: {
				label: "快速（Whisper base）",
				description: "Whisper base，多语言。体积最小、速度最快；准确率最低。适合低资源机器。",
			},
			balanced: {
				label: "均衡（Whisper small）",
				description: "Whisper small，多语言。比 Fast 更准确，CPU/内存占用仍然较轻。",
			},
			turbo: {
				label: "Turbo（Whisper large-v3）",
				description: "Whisper large-v3-turbo，支持 99 种语言。语言覆盖最广；下载体积大，速度较慢。",
			},
			parakeet: {
				label: "Parakeet TDT v3（SoTA）",
				description:
					"NVIDIA Parakeet TDT 0.6B v3，支持 25 种语言。Open ASR Leaderboard 榜首——准确率最高，解码速度也最快。默认选项。",
			},
		},
	},
	"stt.submitTrigger": {
		label: "语音转文字提交触发方式",
		description: "选择语音听写何时自动提交：从不、松开时（2 个以上单词）、松开且句子完整时，或说出提交时。",
		options: {
			never: { label: "从不", description: "从不自动提交；插入听写内容并停留在编辑器中。" },
			release: {
				label: "松开时",
				description: "松开时若语句包含 2 个以上单词则提交，以避免误发送。",
			},
			"release-complete": {
				label: "松开且句子完整",
				description: "松开时若语句以句末标点结尾（. ? ! 等）则提交。",
			},
			"say-submit": {
				label: "说出提交时",
				description: "若语句以包含 'submit' 的单词结尾则提交（提交前去除该单词）。",
			},
		},
	},
	"tools.approval": {
		label: "工具审批策略",
		description:
			"各工具的审批策略。设为 'allow' 自动批准，'prompt' 需要确认，'deny' 阻止执行。覆盖设置在任何审批模式下都会生效。",
	},
	"tools.approvalMode": {
		label: "工具审批",
		description:
			"工具调用的默认审批行为。“始终询问”仅自动批准只读工具。“写入”自动批准只读与工作区写入工具。“Yolo”自动批准所有层级；用户策略仍可能要求确认或阻止。",
		options: {
			"always-ask": {
				label: "始终询问",
				description: "自动批准只读工具；写入与执行类工具需要确认。",
			},
			write: {
				label: "写入",
				description: "自动批准只读与写入工具；bash、eval、browser、task 等执行类工具需要确认。",
			},
			yolo: {
				label: "Yolo",
				description: "自动批准读取、写入与执行类工具。用户策略仍可能要求确认或阻止调用。",
			},
		},
	},
	"features.unexpectedStopDetection": {
		label: "检测意外停止",
		description: "使用小模型检测助手声称将继续但未发出工具调用便停止的情况，并自动提示其继续。",
	},
};
