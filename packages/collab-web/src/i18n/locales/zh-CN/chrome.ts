import type { Dict } from "../../types";

/**
 * App chrome: connect screen, header, composer, banners, transcript markers,
 * agents rail/drawer, and the client/socket status strings surfaced as toasts
 * or end-of-session reasons. Keys are the English source strings.
 */
export const chrome: Dict = {
	// connect screen
	"live agent session, in your browser": "浏览器里的实时 agent 会话",
	"join link": "加入链接",
	"paste a join link first": "请先粘贴加入链接",
	"display name": "显示名称",
	"paste a /collab link from any {0} session": "粘贴任意 {0} 会话的 /collab 链接",
	Connect: "连接",

	// theme + language toggles
	"System theme": "跟随系统主题",
	"Light theme": "浅色主题",
	"Dark theme": "深色主题",
	"{0} — click to switch": "{0} — 点击切换",
	"Language: {0}": "语言：{0}",
	"Auto (browser)": "自动（跟随浏览器）",
	English: "English",
	"Simplified Chinese": "简体中文",

	// header
	"read-only": "只读",
	"you joined with a read-only link — watching only": "你使用只读链接加入，仅可观看",
	"context · {0}": "上下文 · {0}",
	"{0} · {1}": "{0} · {1}",
	host: "主持",
	guest: "访客",
	"view-only": "仅观看",
	"show agents": "显示 agent 面板",
	"hide agents": "隐藏 agent 面板",
	"leave session": "离开会话",

	// connection phases (also the status-dot tooltip)
	connecting: "连接中",
	waiting: "等待中",
	live: "进行中",
	reconnecting: "重连中",
	ended: "已结束",

	// banners
	"connecting to relay…": "正在连接中继…",
	"joining session…": "正在加入会话…",
	"reconnecting…": "正在重连…",
	"session ended": "会话已结束",
	Rejoin: "重新加入",
	"New link": "新链接",

	// toasts
	dismiss: "关闭",

	// composer
	"prompt the host agent…": "向主持端 agent 发送指令…",
	"read-only session — watching only": "只读会话 — 仅可观看",
	"waiting for session…": "等待会话…",
	"type your response…": "输入你的回复…",
	"submit response": "提交回复",
	Submit: "提交",
	Cancel: "取消",
	"stop the current turn": "停止当前回合",
	Stop: "停止",
	queued: "排队",
	"send (Enter)": "发送（Enter）",
	Send: "发送",

	// transcript
	agent: "agent",
	thinking: "思考",
	redacted: "已隐去",
	"(redacted by provider)": "（内容已被提供方隐去）",
	attachment: "附件",
	"context compacted · {0} tokens": "上下文已压缩 · {0} tokens",
	"branch summary": "分支摘要",
	"model → {0}": "模型 → {0}",
	"thinking → {0}": "思考 → {0}",
	off: "关闭",
	error: "错误",
	aborted: "已中止",
	"no activity yet": "暂无活动",
	"thinking…": "思考中…",

	// agents rail + drawer
	"no subagents": "暂无子 agent",
	main: "主",
	sub: "子",
	running: "运行中",
	started: "已启动",
	pending: "待处理",
	parked: "已挂起",
	completed: "已完成",
	failed: "已失败",
	kill: "终止",
	revive: "恢复",
	close: "关闭",
	send: "发送",
	"context {0}": "上下文 {0}",
	cost: "费用",
	tools: "工具",
	"transcript unavailable: {0}": "无法读取会话记录：{0}",
	"no transcript available": "暂无会话记录",
	"message {0}…": "发消息给 {0}…",

	// relative time (format.ts)
	now: "刚刚",
	"{0}s ago": "{0} 秒前",
	"{0}m ago": "{0} 分钟前",
	"{0}h ago": "{0} 小时前",
	"{0}d ago": "{0} 天前",

	// client / socket status
	"room closed": "房间已关闭",
	"no such room": "房间不存在",
	"a host is already connected for this room": "该房间已有主持端连接",
	"room is full": "房间已满",
	"bad key or corrupted frame": "密钥错误或数据帧损坏",
	"connection lost (code {0})": "连接已断开（代码 {0}）",
	"timed out waiting for the host's welcome": "等待主持端欢迎帧超时",
	"timed out waiting for the host's session snapshot": "等待主持端会话快照超时",
	"failed to apply session snapshot: {0}": "应用会话快照失败：{0}",
	"failed to apply {0} frame": "应用 {0} 帧失败",
	"retry {0}/{1}: {2}": "重试 {0}/{1}：{2}",
	"retry failed": "重试失败",
	"compacting context ({0})": "正在压缩上下文（{0}）",
	"compaction aborted": "压缩已中止",
	"compaction failed: {0}": "压缩失败：{0}",
	"context compacted": "上下文已压缩",
};
