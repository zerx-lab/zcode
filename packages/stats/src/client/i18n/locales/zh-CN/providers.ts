/** zh-CN 词条：providers。键 = 英文原文（见 i18n/index.ts 头注释）。 */
export const providersDict: Record<string, string> = {
	// ProviderTotalsPanel
	Provider: "提供商",
	"Error Rate": "错误率",
	Tokens: "Token",
	"Input {0} · Output {1} · Cache read {2} · Cache write {3}": "输入 {0} · 输出 {1} · 缓存读取 {2} · 缓存写入 {3}",
	Share: "占比",
	Cost: "成本",
	"Tok/s": "Token/s",
	"Provider Totals": "提供商总览",
	"Token, request, and cost totals per provider over the active range":
		"当前时间范围内各提供商的 Token、请求与成本总量",
	"No requests recorded in this range": "此时间范围内暂无请求记录",
	// ProviderTrendPanel
	"Total: {0}": "总计：{0}",
	"Burn by Provider": "各提供商消耗",
	"Stacked token/cost burn per provider over time": "各提供商 Token/成本消耗随时间的堆叠趋势",
	"No provider activity in this range": "此时间范围内无提供商活动",
	// PeakHoursPanel
	"Peak Burn Hours": "峰值消耗时段",
	"Token burn by local hour of day — peak at {0}:00": "按本地小时统计的 Token 消耗 — 峰值在 {0}:00",
	"Token burn by local hour of day": "按本地小时统计的 Token 消耗",
	"All providers": "全部提供商",
	"No activity in this range": "此时间范围内无活动",
	// WindowInsightsPanel
	Window: "窗口",
	Accounts: "账户数",
	"Windows Burned": "已消耗窗口数",
	"Subscription-window equivalents consumed in range (sum of used-fraction increases across accounts)":
		"范围内消耗的订阅窗口等效数量（各账户已用比例增量之和）",
	"Est. Tokens / Window": "预估 Token / 窗口",
	"Provider tokens burned in range ÷ windows burned — what one full window is worth":
		"范围内提供商消耗的 Token ÷ 已消耗窗口数 — 一个完整窗口价值多少 Token",
	"Peak Utilization": "峰值利用率",
	"Peak of summed used fraction across accounts at any sampled instant": "任意采样时刻各账户已用比例之和的峰值",
	"Ideal Accounts": "理想账户数",
	"Accounts needed to keep peak demand under 90% of fleet capacity": "将峰值需求控制在集群容量 90% 以下所需的账户数",
	" (have {0})": " (现有 {0})",
	Exhaustions: "耗尽次数",
	"Subscription Windows": "订阅窗口",
	"What each usage window buys you, and how many accounts peak demand needs":
		"每个用量窗口能提供多少额度，以及峰值需求需要多少账户",
	"No usage snapshots recorded yet — they accumulate whenever usage is fetched (TUI footer, /usage, {0} usage)":
		"尚无用量快照记录 — 每次获取用量时会自动累积（TUI 状态栏、/usage、{0} usage）",
	// WindowUtilizationPanel
	Used: "已用",
	"{0}% used": "已用 {0}%",
	"{0} · recorded {1}": "{0} · 记录于 {1}",
	"Window Utilization": "窗口利用率",
	"Latest recorded limit utilization per account and window — red bars are exhausted, amber above 80%":
		"各账户与窗口的最新额度利用率记录 — 红色为已耗尽，超过 80% 为琥珀色",
	"No usage snapshots recorded yet — they accumulate whenever usage is fetched":
		"尚无用量快照记录 — 每次获取用量时会自动累积",
};
