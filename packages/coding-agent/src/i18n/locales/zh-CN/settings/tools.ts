import type { SettingTextDict } from "../../../types";

/** 设置面板「工具」页词条。键为 setting path。 */
export const TOOLS_SETTINGS: SettingTextDict = {
	"tools.artifactSpillThreshold": {
		label: "Artifact 溢出阈值（KB）",
		description: "工具输出超过此大小时保存为 artifact；尾部内容保留在行内",
		options: {
			"1": { label: "1 KB", description: "~250 tokens" },
			"2.5": { label: "2.5 KB", description: "~625 tokens" },
			"5": { label: "5 KB", description: "~1.25K tokens" },
			"10": { label: "10 KB", description: "~2.5K tokens" },
			"20": { label: "20 KB", description: "~5K tokens" },
			"30": { label: "30 KB", description: "~7.5K tokens" },
			"50": { label: "50 KB", description: "默认；~12.5K tokens" },
			"75": { label: "75 KB", description: "~19K tokens" },
			"100": { label: "100 KB", description: "~25K tokens" },
			"200": { label: "200 KB", description: "~50K tokens" },
			"500": { label: "500 KB", description: "~125K tokens" },
			"1000": { label: "1 MB", description: "~250K tokens" },
		},
	},
	"tools.artifactTailBytes": {
		label: "Artifact 尾部大小（KB）",
		description: "输出溢出为 artifact 时，行内保留的尾部内容量",
		options: {
			"1": { label: "1 KB", description: "~250 tokens" },
			"2.5": { label: "2.5 KB", description: "~625 tokens" },
			"5": { label: "5 KB", description: "~1.25K tokens" },
			"10": { label: "10 KB", description: "~2.5K tokens" },
			"20": { label: "20 KB", description: "默认；~5K tokens" },
			"50": { label: "50 KB", description: "~12.5K tokens" },
			"100": { label: "100 KB", description: "~25K tokens" },
			"200": { label: "200 KB", description: "~50K tokens" },
		},
	},
	"tools.artifactHeadBytes": {
		label: "Artifact 头部大小（KB）",
		description: "输出溢出为 artifact 时，与尾部一起保留在行内的头部内容量（中间省略）。0 表示禁用——仅保留尾部。",
		options: {
			"0": { label: "0 KB", description: "禁用；仅保留尾部截断" },
			"1": { label: "1 KB", description: "~250 tokens" },
			"2.5": { label: "2.5 KB", description: "~625 tokens" },
			"5": { label: "5 KB", description: "~1.25K tokens" },
			"10": { label: "10 KB", description: "~2.5K tokens" },
			"20": { label: "20 KB", description: "默认；~5K tokens" },
			"50": { label: "50 KB", description: "~12.5K tokens" },
			"100": { label: "100 KB", description: "~25K tokens" },
			"200": { label: "200 KB", description: "~50K tokens" },
		},
	},
	"tools.outputMaxColumns": {
		label: "输出列数上限",
		description:
			"流式工具输出（bash、python、js eval）及 `read` 的单行字节上限。超出此宽度的行会被省略号截断；到下一个换行符之间的剩余字节将被丢弃。0 表示禁用。",
		options: {
			"0": { label: "关闭", description: "不设单行上限" },
			"256": { label: "256", description: "紧凑" },
			"512": { label: "512" },
			"768": { label: "768", description: "默认" },
			"1024": { label: "1024" },
			"2048": { label: "2048" },
			"4096": { label: "4096", description: "宽松" },
		},
	},
	"tools.artifactTailLines": {
		label: "Artifact 尾部行数",
		description: "输出溢出为 artifact 时，行内保留的尾部内容最大行数",
		options: {
			"50": { label: "50 行", description: "~250 tokens" },
			"100": { label: "100 行", description: "~500 tokens" },
			"250": { label: "250 行", description: "~1.25K tokens" },
			"500": { label: "500 行", description: "默认；~2.5K tokens" },
			"1000": { label: "1000 行", description: "~5K tokens" },
			"2000": { label: "2000 行", description: "~10K tokens" },
			"5000": { label: "5000 行", description: "~25K tokens" },
		},
	},
	"todo.enabled": {
		label: "待办事项",
		description: "启用 todo 工具以进行任务跟踪",
	},
	"todo.reminders": {
		label: "待办提醒",
		description: "在智能体停止前提醒其完成待办事项",
	},
	"todo.remindersMax": {
		label: "待办提醒上限",
		description: "放弃前的最大待办提醒次数",
		options: {
			"1": { label: "1 次提醒" },
			"2": { label: "2 次提醒" },
			"3": { label: "3 次提醒" },
			"5": { label: "5 次提醒" },
		},
	},
	"todo.eager": {
		label: "自动创建待办事项",
		description: "首条消息后推动自动创建待办列表的力度",
		options: {
			default: { label: "默认", description: "由模型自行决定；不自动创建待办列表" },
			preferred: { label: "推荐", description: "首条消息时建议创建待办列表（提醒而非强制）" },
			always: { label: "总是", description: "首条消息时强制创建完整待办列表" },
		},
	},
	"glob.enabled": {
		label: "Glob",
		description: "启用 glob 工具进行基于通配符的文件查找",
	},
	"grep.enabled": {
		label: "Grep",
		description: "启用 grep 工具进行正则内容搜索",
	},
	"grep.contextBefore": {
		label: "Grep 匹配前上下文",
		description: "每个 grep 匹配项前保留的上下文行数",
		options: {
			"0": { label: "0 行" },
			"1": { label: "1 行" },
			"2": { label: "2 行" },
			"3": { label: "3 行" },
			"5": { label: "5 行" },
		},
	},
	"grep.contextAfter": {
		label: "Grep 匹配后上下文",
		description: "每个 grep 匹配项后保留的上下文行数",
		options: {
			"0": { label: "0 行" },
			"1": { label: "1 行" },
			"2": { label: "2 行" },
			"3": { label: "3 行" },
			"5": { label: "5 行" },
			"10": { label: "10 行" },
		},
	},
	"astGrep.enabled": {
		label: "AST Grep",
		description: "启用 ast_grep 工具进行结构化 AST 搜索",
	},
	"astEdit.enabled": {
		label: "AST Edit",
		description: "启用 ast_edit 工具进行结构化 AST 重写",
	},
	"debug.enabled": {
		label: "Debug",
		description: "启用 debug 工具进行基于 DAP 的调试",
	},
	"launch.enabled": {
		label: "Launch",
		description: "启用 launch 工具以监管共享的长时间运行项目进程",
	},
	"speechgen.enabled": {
		label: "语音生成",
		description: "启用 tts 工具，通过设备本地（Kokoro）或 xAI Grok Voice 合成语音文件",
	},
	"generate_image.enabled": {
		label: "生成图片",
		description: "启用 generate_image 工具（文本生成图片与编辑）。当 tools.xdev 开启时以 xd:// 设备形式暴露。",
	},
	"inspect_image.mode": {
		label: "图片检查",
		description:
			"控制 inspect_image 工具，该工具将图片理解委托给具备视觉能力的模型。'auto' 仅在当前模型缺少原生图片输入能力时暴露该工具；'on' 始终暴露；'off' 从不暴露。",
		options: {
			auto: { label: "自动（仅当模型不支持视觉时）" },
			on: { label: "开启" },
			off: { label: "关闭" },
		},
	},
	"computer.enabled": {
		label: "Computer",
		description: "启用可脚本化的主机桌面控制工具（截图、输入、无障碍访问）",
	},
	"computer.display": {
		label: "Computer 显示器",
		description: "合成所有显示器画面，或选择一个原生显示器 id",
	},
	"computer.maxWidth": {
		label: "Computer 截图宽度",
		description: "合成截图的最大宽度（像素）",
	},
	"computer.maxHeight": {
		label: "Computer 截图高度",
		description: "合成截图的最大高度（像素）",
	},
	"inspect_image.timeoutMs": {
		label: "图片检查超时",
		description:
			"inspect_image 视觉模型调用的单次请求超时时间（毫秒）。发生卡顿的服务商会快速返回超时错误，而不是阻塞直到手动中止。设为 0 可禁用超时。",
		options: {
			"0": { label: "禁用" },
			"60000": { label: "1 分钟" },
			"120000": { label: "2 分钟" },
			"180000": { label: "3 分钟" },
			"300000": { label: "5 分钟" },
		},
	},
	"checkpoint.enabled": {
		label: "检查点/回退",
		description: "启用 checkpoint 与 rewind 工具以进行上下文检查点管理",
	},
	"fetch.enabled": {
		label: "读取 URL",
		description: "允许 read 工具抓取并处理 URL",
	},
	"vault.enabled": {
		label: "Obsidian Vault",
		description:
			"启用 vault:// 内部 URL，通过 Obsidian CLI 读取和编辑 Obsidian vault 内容。禁用时 vault:// 解析会被拒绝，且 vault:// 条目不会出现在系统提示词中。",
	},
	"github.enabled": {
		label: "GitHub CLI",
		description:
			"启用 github 工具（基于 op 的调度，覆盖仓库、issue、PR、diff、搜索、checkout、push 及 Actions watch 等工作流）",
	},
	"github.cache.enabled": {
		label: "GitHub 视图缓存",
		description: "将渲染后的 issue/PR 视图输出缓存到 ~/.omp/cache/github-cache.db，使重复读取免费",
	},
	"github.cache.softTtlSec": {
		label: "GitHub 缓存软 TTL",
		description: "在此时间窗内，直接返回缓存的 issue/PR 视图行（单位秒；默认 5 分钟）",
	},
	"github.cache.hardTtlSec": {
		label: "GitHub 缓存硬 TTL",
		description: "超过软 TTL 后返回缓存行并在后台刷新；超过硬 TTL 后丢弃该缓存（单位秒；默认 7 天）",
	},
	"web_search.enabled": {
		label: "网络搜索",
		description: "启用 web_search 工具以获取实时网络结果",
	},
	"security.enabled": {
		label: "安全",
		description: "启用 OMP 原生安全扫描规划与执行，以及只读的 security:// 资源命名空间",
	},
	"ask.enabled": {
		label: "Ask",
		description: "启用 ask 工具以进行交互式用户提问",
	},
	"browser.enabled": {
		label: "浏览器",
		description: "启用 browser 工具进行脚本化 Chromium 自动化（puppeteer）",
	},
	"browser.cdpUrl": {
		label: "浏览器 CDP URL",
		description:
			"默认的 HTTP CDP 发现端点（例如 http://127.0.0.1:9222），用于附加连接而非启动新浏览器。工具调用中显式的 app.cdp_url 或 app.path 优先级更高。",
	},
	"browser.relay": {
		label: "浏览器中继",
		description:
			"通过 omp 浏览器中继驱动你自己的 Chrome 标签页。安装一次扩展（`omp browser-relay install`）；browser 工具需要时中继服务器会自动启动。优先级高于浏览器 CDP URL；可设置 PI_BROWSER_RELAY=0 或 PI_BROWSER_RELAY=1 覆盖。",
	},
	"browser.relayUrl": {
		label: "浏览器中继 URL",
		description: "omp 浏览器中继端点（默认 http://127.0.0.1:9224）。",
	},
	"browser.headless": {
		label: "无头浏览器",
		description: "以无头模式启动浏览器（关闭则显示浏览器界面）",
	},
	"browser.cmux": {
		label: "cmux 浏览器",
		description:
			"当 cmux socket 可用时，使用 cmux WKWebView 界面进行浏览器自动化。可设置 PI_BROWSER_CMUX=0 或 PI_BROWSER_CMUX=1 覆盖。",
	},
	"browser.screenshotDir": {
		label: "截图目录",
		description:
			"保存截图的目录。未设置时截图保存到临时文件。支持 ~。示例：~/Downloads、~/Desktop、/sdcard/Download（Android）",
	},
	"tools.intentTracing": {
		label: "意图追踪",
		description: "要求智能体在执行每次工具调用前描述其意图",
	},
	"tools.abortOnFabricatedResult": {
		label: "检测到伪造工具结果时中止",
		description:
			"在带内工具调用场景下，一旦模型在轮次中途开始虚构工具结果，立即停止模型。关闭后则让模型继续生成完毕，再丢弃虚构出的续写内容。",
	},
	"tools.maxTimeout": {
		label: "工具超时上限",
		description: "智能体可为任意工具设置的最大超时时间（秒，0 表示无限制）",
		options: {
			"0": { label: "无限制" },
			"30": { label: "30 秒" },
			"60": { label: "60 秒" },
			"120": { label: "120 秒" },
			"300": { label: "5 分钟" },
			"600": { label: "10 分钟" },
		},
	},
	"async.enabled": {
		label: "异步执行",
		description: "启用异步 bash 命令与后台任务执行",
	},
	"async.pollWaitDuration": {
		label: "最大轮询时间",
		description:
			"`hub` wait 在返回当前状态前监视后台任务的时长。固定值每次都等待相同时长。`smart` 会自适应：从 5 秒开始，随着连续等待逐渐延长（最长 5 分钟），约一分钟无等待后重置为 5 秒。",
		options: {
			"5s": { label: "5 秒" },
			"10s": { label: "10 秒" },
			"30s": { label: "30 秒" },
			"1m": { label: "1 分钟" },
			"5m": { label: "5 分钟" },
			smart: { label: "智能", description: "默认——5 秒到 5 分钟自适应，停止轮询后重置" },
		},
	},
	"irc.timeoutMs": {
		label: "IRC 超时",
		description: "hub 消息等待（及 send await:true）的默认超时时间（毫秒）；0 表示禁用超时",
		options: {
			"0": { label: "禁用" },
			"30000": { label: "30 秒" },
			"60000": { label: "1 分钟" },
			"120000": { label: "2 分钟" },
			"300000": { label: "5 分钟" },
		},
	},
	"tools.xdev": {
		label: "xd:// 工具",
		description:
			"将不常用（可发现）的工具挂载到 xd:// 设备 URL 下，通过 read/write 驱动，而非在每次请求中都携带其 schema。没有被授予 write 工具的会话会跳过挂载，将所有工具以顶层形式暴露。禁用后所有已启用工具都以顶层形式暴露。",
	},
	"tools.xdevDocs": {
		label: "xd:// 提示词文档",
		description:
			"选择哪些已挂载设备的文档与 schema 内联到系统提示词中。Built-ins 保持核心工具内联，MCP 与扩展工具则按需获取。",
		options: {
			inline: { label: "全部设备", description: "为每个已挂载设备内联文档与 schema。" },
			builtins: { label: "仅内置", description: "内联内置工具文档；MCP 与扩展工具文档按需获取。" },
			catalog: { label: "仅目录", description: "列出所有设备；所有文档按需获取。" },
		},
	},
	"tools.xdevInlineDevices": {
		label: "xd:// 内联设备",
		description:
			"当 xd:// 提示词文档 设为「仅内置」时，内联名称匹配这些通配符模式的动态设备（例如 mcp__context_mode_*）。「仅目录」模式下此设置无效。",
	},
	"mcp.enableProjectConfig": {
		label: "MCP 项目配置",
		description: "从项目根目录加载 .mcp.json/mcp.json",
	},
	"mcp.renderMarkdownResults": {
		label: "MCP Markdown 结果",
		description: "在会话记录中将非 JSON 的 MCP 文本结果渲染为 Markdown",
	},
	"mcp.notifications": {
		label: "MCP 更新注入",
		description: "将 MCP 资源更新注入到智能体对话中",
	},
	"mcp.notificationDebounceMs": {
		label: "MCP 通知防抖",
		description: "MCP 资源更新注入对话前的防抖窗口（毫秒）",
	},
	"tasks.todoClearDelay": {
		label: "待办自动清除延迟",
		description: "已完成或已放弃的待办事项从待办小部件中移除前的延迟时间",
		options: {
			"0": { label: "立即" },
			"60": { label: "1 分钟", description: "默认" },
			"300": { label: "5 分钟" },
			"900": { label: "15 分钟" },
			"1800": { label: "30 分钟" },
			"3600": { label: "1 小时" },
			"-1": { label: "从不" },
		},
	},
	"dev.autoqa": {
		label: "自动 QA",
		description:
			"自动化工具问题上报（xd://report_issue）。默认开启；首次上报会请求同意，拒绝后将禁用上报直至显式重新启用",
	},
	"dev.autoqaPush.endpoint": {
		label: "自动 QA 推送端点",
		description: "接收自动 QA JSON 报告的完整 URL（默认 https://qa.omp.sh/v1/grievances）",
	},
};
