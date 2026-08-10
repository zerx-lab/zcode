import type { TextDict } from "../../types";

/**
 * 界面固定文案译文，键为英文原文。
 *
 * 只收录**接线点实际取词**的字符串（`t()` / `tf()` 的实参），不做泛化收集：
 * 键与调用点一一对应，上游改了原文即回退英文，不会出现幽灵词条。
 * `tf()` 的模板保留 `{0}` 占位符，译文可自由调整参数位置。
 */
export const ZH_CN_CHROME: TextDict = {
	// ── 设置面板：框架与导航 ────────────────────────────────────────────
	Settings: "设置",
	Plugins: "插件",
	"Preview:": "预览：",
	"(preview not available)": "（预览不可用）",
	"No matching settings": "没有匹配的设置项",
	"1 match": "1 项匹配",
	"{0} matches": "{0} 项匹配",

	// ── 设置面板：底部按键提示 ──────────────────────────────────────────
	"Enter to change · Tab to jump tabs · Esc to exit search": "Enter 修改 · Tab 跳转标签页 · Esc 退出搜索",
	"Tab to switch tabs · Esc to close": "Tab 切换标签页 · Esc 关闭",
	"↑/↓ to jump sections · Tab/Enter to settings · ←/→ to switch tabs · Esc to close":
		"↑/↓ 跳转分组 · Tab/Enter 进入设置项 · ←/→ 切换标签页 · Esc 关闭",
	"Tab to jump sections · ←/→ to switch tabs": "Tab 跳转分组 · ←/→ 切换标签页",
	"Tab to switch tabs": "Tab 切换标签页",
	"Enter/Space to change · {0} · Type to search · Esc to close": "Enter/空格 修改 · {0} · 输入即搜索 · Esc 关闭",

	// ── 设置面板：子菜单 ────────────────────────────────────────────────
	"Enter to save · Esc to cancel · Clear field to unset": "Enter 保存 · Esc 取消 · 清空输入框以恢复未设置",
	"Enter to select · Esc to go back": "Enter 选择 · Esc 返回",
	"Enter/Space to toggle · Esc to go back": "Enter/空格 切换 · Esc 返回",
	"Enter/Space to toggle · ←/→ move · 1-9 place at position · Esc to go back":
		"Enter/空格 切换 · ←/→ 移动 · 1-9 放到指定位置 · Esc 返回",
	default: "默认",
	none: "无",

	// ── 设置面板：服务商并发上限 ────────────────────────────────────────
	"Max In-Flight Requests": "最大并发请求数",
	"Max In-Flight Requests: {0}": "最大并发请求数：{0}",
	"Select a provider, enter a positive number to cap concurrent LLM requests, or clear it for unlimited.":
		"选择一个服务商，输入正整数以限制并发 LLM 请求数，清空则表示不限。",
	"Enter a positive number. Decimals round down. Clear the field to make this provider unlimited.":
		"输入一个正数。小数向下取整。清空输入框表示该服务商不限并发。",
	"Limit must be a positive number.": "上限必须是正数。",
	"Enter to edit provider · Esc to go back": "Enter 编辑服务商 · Esc 返回",
	"Clear all limits": "清除所有上限",
	"Make every provider unlimited": "让所有服务商都不限并发",
	Unlimited: "不限",
	"Limit: {0}": "上限：{0}",
	"Invalid record JSON for {0}": "{0} 的 JSON 记录格式无效",

	// ── 欢迎页 ──────────────────────────────────────────────────────────
	"Welcome back!": "欢迎回来！",
	Tip: "提示",
	Tips: "提示",
	" for prompt actions": " 提示词动作",
	" for commands": " 命令",
	" to run bash": " 执行 bash",
	" to run python": " 执行 python",
	"LSP Servers": "LSP 服务",
	"Recent sessions": "最近会话",
	"No recent sessions": "暂无最近会话",
	"No LSP servers": "暂无 LSP 服务",

	// ── slash 补全的动态状态描述 ────────────────────────────────────────
	// 这些由 `getTuiAutocompleteDescription` 在每次补全时按会话状态拼出，
	// 绕过静态 description，所以必须单独登记。`{0}` 是运行时值（模型 id、
	// 百分比、计数），不翻译。
	"Model: none selected": "模型：未选择",
	"Model: {0}": "模型：{0}",
	"Plan: on": "计划模式：开",
	"Plan: on ({0})": "计划模式：开（{0}）",
	"Plan: off": "计划模式：关",
	"Plan: disabled in settings": "计划模式：已在设置中禁用",
	"Plan: blocked by goal mode": "计划模式：被目标模式阻断",
	"Plan review: available": "计划复审：可用",
	"Plan review: plan mode inactive": "计划复审：计划模式未启用",
	"Vibe: on": "Vibe 模式：开",
	"Vibe: off": "Vibe 模式：关",
	"Vibe: blocked by plan mode": "Vibe 模式：被计划模式阻断",
	"Vibe: blocked by goal mode": "Vibe 模式：被目标模式阻断",
	"Goal: off": "目标模式：关",
	"Goal: {0} ({1})": "目标模式：{0}（{1}）",
	"Goal: disabled in settings": "目标模式：已在设置中禁用",
	"Goal: blocked by plan mode": "目标模式：被计划模式阻断",
	"Loop: off": "循环模式：关",
	"Loop: paused": "循环模式：已暂停",
	"Loop: on ({0})": "循环模式：开（{0}）",
	"Loop: on (repeating prompt)": "循环模式：开（重复同一提示词）",
	"Loop: on (waiting for next prompt)": "循环模式：开（等待下一条提示词）",
	"Fast: {0}": "快速模式：{0}",
	"Computer: {0}": "计算机控制：{0}",
	"Vision: {0}": "视觉：{0}",
	"Advisor: off": "顾问：关",
	"Advisor: on ({0})": "顾问：开（{0}）",
	"Advisor: on ({0} advisors)": "顾问：开（{0} 个顾问）",
	"Advisor: configured, no model": "顾问：已配置，但未指定模型",
	"Collab: off": "协作：关",
	"Collab: guest": "协作：访客",
	"Collab: read-only guest": "协作：只读访客",
	"Collab: hosting ({0} guests)": "协作：主持中（{0} 位访客）",
	"Leave collab: hosting": "退出协作：当前为主持方",
	"Leave collab: guest": "退出协作：当前为访客",
	"Leave collab: not in collab": "退出协作：当前不在协作中",
	"Browser: disabled": "浏览器：已禁用",
	"Browser: headless": "浏览器：无头模式",
	"Browser: visible": "浏览器：可见模式",
	"Force: no active tools": "强制工具：当前无可用工具",
	"Force: {0} active tools": "强制工具：{0} 个可用工具",
	"Fresh: ready": "重置流状态：就绪",
	"Fresh: unavailable while streaming": "重置流状态：流式输出中不可用",
	"Clear: drop context, keep session": "清空上下文：丢弃上下文，保留会话",
	"Clear: unavailable while streaming": "清空上下文：流式输出中不可用",
	"Compact: context unavailable": "压缩：上下文信息不可用",
	"Compact: context {0}% used": "压缩：上下文已用 {0}%",
	"Context: unavailable": "上下文：不可用",
	"Context: {0}% ({1}/{2})": "上下文：{0}%（{1}/{2}）",
	"Todos: none": "待办：无",
	"Todos: {0} open ({1} in progress, {2} done)": "待办：{0} 项未完成（{1} 项进行中，{2} 项已完成）",
	"Jobs: none": "后台任务：无",
	"Jobs: {0} running, {1} recent": "后台任务：{0} 个运行中，{1} 个最近完成",
	"Tools: none available": "工具：无可用工具",
	"Tools: {0} active / {1} available": "工具：{0} 个启用 / 共 {1} 个",
	"Login: choose provider": "登录：选择服务商",
	"Login: waiting for {0} callback": "登录：等待 {0} 回调",

	// `/loop` 的限额描述（`describeLoopLimit*`），也用于状态栏提示。
	// 英文单复数各占一条键以保住回退时的语法；中文两条同形。
	"{0} iteration": "{0} 次",
	"{0} iterations": "{0} 次",
	"{0} of {1} iteration remaining": "剩余 {0}/{1} 次",
	"{0} of {1} iterations remaining": "剩余 {0}/{1} 次",
	"{0} limit": "限制 {0}",
	"{0} hour": "{0} 小时",
	"{0} hours": "{0} 小时",
	"{0} minute": "{0} 分钟",
	"{0} minutes": "{0} 分钟",
	"{0} second": "{0} 秒",
	"{0} seconds": "{0} 秒",
	// 上面若干条描述里内嵌的状态词（`t(value)` 取词，未命中即回退原文）。
	on: "开",
	off: "关",
	auto: "自动",
	active: "进行中",
	paused: "已暂停",
	"budget-limited": "预算受限",
	complete: "已完成",
	dropped: "已放弃",
};
