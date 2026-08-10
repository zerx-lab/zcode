/** zh-CN 词条：ui。键 = 英文原文（见 i18n/index.ts 头注释）。 */
export const uiDict: Record<string, string> = {
	// ui/DataTable.tsx, ui/EmptyState.tsx, ui/AsyncBoundary.tsx（共享默认文案）
	"No data available": "暂无数据",
	// ui/ErrorState.tsx
	"Failed to load data": "加载数据失败",
	Retry: "重试",
	// ui/MetricCluster.tsx（"Requests" 复用 app.ts 已有的键，此处不重复）
	"Total Cost": "总成本",
	"Cache Rate": "缓存命中率",
	"Error Rate": "错误率",
	"Uncached Input": "未缓存输入",
	"Cache Read": "缓存读取",
	"Output Tokens": "输出 Token",
	"Conversation Total": "会话总量",
	"Premium Requests": "高级请求",
	"Tokens/s": "Token/s",
	"Avg Latency": "平均延迟",
	"Avg TTFT": "平均 TTFT",
	"Conversation input not served from cache": "未从缓存提供的会话输入",
	"Conversation input read from the prompt cache": "从提示缓存读取的会话输入",
	"Uncached input + cache reads + cache writes + output": "未缓存输入 + 缓存读取 + 缓存写入 + 输出",
	// ui/JsonBlock.tsx
	"Copied to clipboard": "已复制到剪贴板",
	"Copy JSON to clipboard": "复制 JSON 到剪贴板",
	Copied: "已复制",
	Copy: "复制",
	"▶ Show": "▶ 展开",
	"▼ Hide": "▼ 收起",
	// app/SyncButton.tsx
	"Synced: {0} new request{1} found.": "同步完成，发现 {0} 个新请求。",
	"Sync failed: {0}": "同步失败：{0}",
	"Syncing...": "同步中…",
	"Sync DB": "同步数据库",
	// app/RangeControl.tsx
	"Select time range": "选择时间范围",
	All: "全部",
	// app/ThemeToggle.tsx
	"System theme": "跟随系统",
	"Light theme": "浅色主题",
	"Dark theme": "深色主题",
	"{0} (click to switch)": "{0}（点击切换）",
	"{0} — click to switch": "{0} — 点击切换",
};
