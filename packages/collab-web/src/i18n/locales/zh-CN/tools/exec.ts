import type { Dict } from "../../../types";

/** `bash`, `eval`, `debug`, `lsp`, `browser` tool-render dictionary. */
export const toolExec: Dict = {
	// bash.tsx
	"wall {0}ms": "耗时 {0}ms",
	"wall {0}s": "耗时 {0}s",
	"requested timeout {0}s clamped": "请求超时 {0}s（已限制）",
	"job {0}": "任务 {0}",
	"artifact {0}": "产物 {0}",
	"exit {0}": "退出 {0}",

	// eval.tsx
	"{0} cells": "{0} 个单元格",
	error: "错误",
	"error (exit {0})": "错误（退出 {0}）",
	display: "显示",

	// debug.tsx
	request: "请求",
	session: "会话",
	status: "状态",
	"stop reason": "停止原因",
	frame: "帧",
	location: "位置",
	"exit code": "退出码",
	configuration: "配置",
	"pending configurationDone — set breakpoints, then continue": "等待 configurationDone —— 先设置断点，然后继续执行",

	// lsp.tsx
	workspace: "工作区",
	"line {0}": "第 {0} 行",
	server: "服务器",
	"{0} errors": "{0} 个错误",
	"{0} warnings": "{0} 个警告",
	"{0} references": "{0} 处引用",
	"… {0} more": "… 还有 {0} 项",
	yes: "是",
	no: "否",

	// browser.tsx
	"all tabs": "全部标签页",
	"tab {0}": "标签页 {0}",
	"connected {0}": "已连接 {0}",
	"spawned {0}": "已启动 {0}",
};
