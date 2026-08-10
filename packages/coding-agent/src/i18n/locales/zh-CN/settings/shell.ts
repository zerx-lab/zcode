import type { SettingTextDict } from "../../../types";

/** 设置面板「Shell」页词条。键为 setting path。 */
export const SHELL_SETTINGS: SettingTextDict = {
	"bash.enabled": {
		label: "Bash",
		description: "启用 bash 工具以执行 shell 命令",
	},
	"bash.autoBackground.enabled": {
		label: "Bash 自动后台化",
		description: "自动将长时间运行的 bash 命令转入后台，稍后再交付结果",
	},
	"bash.patterns": {
		label: "Bash 审批规则",
		description: "有序的 bash 命令审批规则。每条包含 match 与 approval 字段，仅支持 `*` 通配符。",
	},
	"bashInterceptor.enabled": {
		label: "Bash 拦截器",
		description: "拦截已有专用工具覆盖的 shell 命令",
	},
	"bash.direnv": {
		label: "direnv 自动加载",
		description:
			"将仓库 direnv/devenv 的 `.envrc` 自动加载进 bash 会话，使 devenv 工具与环境变量无需手动 `direnv exec` 即可生效。遵循 direnv 的白名单：未经 `direnv allow` 的 `.envrc` 永远不会被执行",
	},
	"bash.direnvLoadTimeoutMs": {
		label: "direnv 加载超时（毫秒）",
		description:
			"等待首次 `direnv export` 的最长时间（冷启动的 devenv shell 可能较慢）；超时后会话将在无 direnv 环境的情况下运行",
	},
	"shellMinimizer.enabled": {
		label: "Shell 输出精简",
		description: "在返回给智能体前压缩冗长的 shell 输出（git、npm、cargo 等）",
	},
	"shellMinimizer.sourceOutlineLevel": {
		label: "Shell 精简源码大纲",
		description: "cat/read 源文件时的大纲模式：default 或 aggressive",
	},
	"eval.py": {
		label: "Python Eval 后端",
		description: "允许 eval 工具将 Python 单元分派给 IPython 内核",
	},
	"eval.js": {
		label: "JavaScript Eval 后端",
		description: "允许 eval 工具将 JavaScript 单元分派给进程内运行时",
	},
	"eval.rb": {
		label: "Ruby Eval 后端",
		description: "允许 eval 工具将 Ruby 单元分派给常驻 Ruby 内核",
	},
	"eval.jl": {
		label: "Julia Eval 后端",
		description: "允许 eval 工具将 Julia 单元分派给常驻 Julia 内核",
	},
	"python.kernelMode": {
		label: "Python 内核模式",
		description: "在多次 eval 调用之间保持 IPython 内核存活，还是每次都重新启动",
	},
	"python.interpreter": {
		label: "Python 解释器",
		description: "指定确切 Python 可执行文件的路径（可选）。设置后将跳过自动 Python 运行时发现。",
	},
	"ruby.interpreter": {
		label: "Ruby 解释器",
		description: "指定确切 Ruby 可执行文件的路径（可选）。设置后将跳过自动 Ruby 运行时发现。",
	},
	"julia.interpreter": {
		label: "Julia 解释器",
		description: "指定确切 Julia 可执行文件的路径（可选）。设置后将跳过自动 Julia 运行时发现。",
	},
};
