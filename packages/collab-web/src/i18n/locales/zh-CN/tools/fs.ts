import type { Dict } from "../../../types";

/** Filesystem tool renderers: `read`, `write`, `edit`/`apply_patch`, `glob`, `grep`, `ast_edit`, `ast_grep`. */
export const toolFs: Dict = {
	// read.tsx
	resolved: "解析路径",
	"corrected from": "纠正自",
	"{0} conflicts": "{0} 处冲突",
	"{0} elided spans": "{0} 处省略",
	truncated: "已截断",

	// write.tsx
	"{0} lines": "{0} 行",
	"made executable": "已设为可执行",
	"expected string": "应为字符串",
	diagnostics: "诊断信息",

	// edit.tsx
	"+{0} more": "另外 {0} 个",
	"{0} ops": "{0} 个操作",
	failed: "失败",
	create: "新建",
	delete: "删除",
	input: "输入",
	edit: "编辑",

	// glob.tsx
	"limit {0}": "上限 {0}",
	"timeout {0}s": "超时 {0} 秒",
	"{0} files": "{0} 个文件",
	"in {0}": "位于 {0}",
	"truncated at {0}": "已截断至 {0}",
	"skipped missing:": "跳过缺失：",

	// grep.tsx
	in: "在",
	"{0} matches": "{0} 处匹配",

	// ast-edit.tsx
	"{0} replacements": "{0} 处替换",
	limit: "已达上限",
	pattern: "模式",
	replacement: "替换",
	"deletion — matched code is removed": "删除 —— 匹配到的代码将被移除",
	"searched {0}": "已搜索 {0}",
	"limit reached": "已达上限",
	"limit reached; narrow path": "已达上限，请缩小路径范围",
	"parse issues ({0})": "解析问题（{0}）",

	// ast-grep.tsx
	"pattern {0}": "模式 {0}",
	path: "路径",
	paths: "路径",
	scope: "范围",
	"parse issues ({0} total)": "解析问题（共 {0} 个）",
	"parse issues": "解析问题",
};
