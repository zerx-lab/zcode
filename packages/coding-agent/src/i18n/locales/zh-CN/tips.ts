import type { TextDict } from "../../types";

/**
 * 欢迎页 tips 译文，键为 `tips.txt` 里的英文原文（已剥离 `[NEW]` 标记）。
 *
 * 上游改动某条 tip 的原文即自动回退英文显示，绝不会出现与新原文对不上的旧译文；
 * 同步后按 `tips.txt` 的差异补键即可。
 */
export const ZH_CN_TIPS: TextDict = {
	"Please use nerdfont 😭.": "求你了，装个 Nerd Font 吧 😭",
	"Tired of typing \"keep going\"? Just send a '.'": "懒得反复打 “keep going”？发一个 '.' 就行",
	"You can /btw to ask a side question": "用 /btw 可以插问一个题外问题",
	"Use /tan to fork the current conversation into a background agent": "用 /tan 把当前对话分叉成一个后台智能体",
	"Ctrl+D can be used to exit, but with your draft saved!": "Ctrl+D 可以退出，而且草稿会被保存下来",
	"Find out which model you emotionally abuse the most with `omp stats`":
		"用 `omp stats` 看看你精神虐待得最狠的是哪个模型",
	"Try task isolation to create CoW worktrees": "试试任务隔离，会创建写时复制（CoW）工作树",
	"Need a cheap nested model call? Use `completion(x...)`. Have a big batch of tasks? Ask clanker to use it!":
		"需要一次便宜的嵌套模型调用？用 `completion(x...)`。任务批量很大？让 clanker 去用它",
	"Spaghetti code? Try complaining with /omfg": "面条代码？用 /omfg 吐槽一下",
	"Did you know? Each kitty/tmux/cmux/zellij/wezterm split keeps its own session — `omp -c` resumes the right one":
		"知道吗？kitty/tmux/cmux/zellij/wezterm 的每个分屏都各自持有会话 —— `omp -c` 会恢复对应的那个",
	"Drop the word `ultrathink` in your message for harder multi-step reasoning — watch it glow rainbow as you type":
		"在消息里写下 `ultrathink` 触发更硬核的多步推理 —— 边打字边看它泛起彩虹光",
	"Say `orchestrate` in your message to drive a multi-phase task with parallel subagents — watch it glow as you type":
		"在消息里说 `orchestrate`，用并行子智能体驱动多阶段任务 —— 边打字边看它发光",
	"Say `workflowz` in your message to drive the task with parallel subagents in eval — watch it glow as you type":
		"在消息里说 `workflowz`，在 eval 里用并行子智能体驱动任务 —— 边打字边看它发光",
	"Log in to several accounts of the same provider — `/login` again — and omp load-balances across them automatically":
		"同一个服务商可以登录多个账号 —— 再跑一次 `/login` —— omp 会自动在它们之间做负载均衡",
	"Run `omp auth-broker serve` once and every machine pulls live tokens over the wire — refresh keys never leave the host; `omp auth-gateway` fronts it as a drop-in proxy any OpenAI-compatible client can hit":
		"跑一次 `omp auth-broker serve`，所有机器都能通过网络实时取 token —— refresh key 永不离开宿主机；`omp auth-gateway` 在它前面充当代理，任何 OpenAI 兼容客户端都能直接接入",
	"Press alt+p (or /switch) to switch provider, and ctrl+p to cycle role models smol -> slow -> etc":
		"按 alt+p（或 /switch）切换服务商，按 ctrl+p 在 smol -> slow 等角色模型间轮换",
	"Press ctrl+r to search your prompt history and reuse a past message": "按 ctrl+r 搜索历史输入，复用过去发过的消息",
	"`/force read` pins the next turn to one specific tool when the model keeps reaching for the wrong one":
		"模型老是抓错工具时，用 `/force read` 把下一轮钉死在指定工具上",
	"`/copy code` grabs the last code block to your clipboard — `/copy cmd` grabs the last shell/python command":
		"`/copy code` 把最后一个代码块复制到剪贴板 —— `/copy cmd` 复制最后一条 shell/python 命令",
	"`/shake` rips heavy tool results out of context to reclaim tokens without a full /compact — `/shake images` drops just images":
		"`/shake` 把笨重的工具结果从上下文里抖出去回收 token，不必整体 /compact —— `/shake images` 只丢图片",
	"Pair up live: `/collab` shares your session through an end-to-end encrypted relay link — a teammate runs `/join <link>` to watch tool calls stream and prompt the agent from their own omp":
		"实时结对：`/collab` 通过端到端加密的中继链接分享会话 —— 队友执行 `/join <link>` 就能看到工具调用流式刷出，并从自己的 omp 里给智能体发指令",
	"Press ← ← to drill into a running or finished agent and inspect its tool calls and transcript":
		"连按两次 ← 可以钻进运行中或已结束的智能体，查看它的工具调用与会话记录",
	"Hit a Codex rate limit? `/usage reset` spends a saved reset credit to immediately restore your quota":
		"撞上 Codex 限流？`/usage reset` 消耗一次存下的重置额度，立刻恢复配额",
	"No native tool_calling? Inference provider botches parsing them? `PI_DIALECT=glm|kimi|anthropic…` rolls it locally for them!":
		"没有原生 tool_calling？推理服务商把解析搞砸了？`PI_DIALECT=glm|kimi|anthropic…` 在本地替它们把这活干了",
	"Turn on `/advisor` to attach a second model that reviews every turn and quietly injects advice":
		"打开 `/advisor`，挂一个第二模型逐轮复查，并悄悄注入建议",
	"Try starting your prompt with a ->, and writing a list (1. Do X, 2. Do Y)":
		"试试用 -> 开头写提示词，再列个清单（1. 做 X，2. 做 Y）",
	"Press shift+tab to cycle through reasoning effort levels": "按 shift+tab 在推理强度档位之间轮换",
};
