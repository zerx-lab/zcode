#!/usr/bin/env bash
# git merge driver：根 README.md 永不三方合并（策略见 docs/fork/sync-strategy.md）。
#
# 挂载点是 .git/info/attributes 里的 `README.md merge=zcode-readme`（**不是**版本内的
# .gitattributes：那一行自己也在补丁栈里，重放到它之前不生效）。挂载与 driver 注册
# 都由 brand/git-setup.sh 完成，brand/sync.sh 与 CI 的 sync-upstream.yml 都会调它。
# 行为探针：brand/merge-readme-selftest.sh。
#
# 为什么不是内置的 `merge=ours`：rebase 里 ours/theirs 是反的（ours = 正在重放到的
# 上游基，theirs = 补丁栈这侧），ours driver 会保留**上游** README。而 README 的真源
# 根本不是冲突双方中的任何一侧，是 brand/README.md 模板 —— 重渲染与方位无关，
# rebase / merge / cherry-pick 三种重放都收敛到同一份内容。
#
# 契约：$1 = %A（ours，同时是结果落点），$2 = %B（theirs）；git 保证 cwd 是工作树
# 顶层，两个路径都相对它。写好 $1 并 exit 0 即视为已解决；非零退出留下冲突标记。
set -uo pipefail

out=$1
theirs=$2

if [[ -f brand/README.md ]] && command -v bun >/dev/null 2>&1 &&
	bun brand/apply.ts --render README.md >"$out.zcode-merge" 2>/dev/null; then
	mv -- "$out.zcode-merge" "$out"
	exit 0
fi

# 回退有两种触发：重放序列还没轮到引入 brand/README.md 的那个提交（模板此刻不在
# 工作树里），或者环境里没有 bun。取 %B —— rebase 时它是补丁栈这侧，等价于"以
# zcode 为准"。本 fork 明文不做 upstream→zcode 的 merge（release 也是
# `git branch -f` 重建而非合并），所以不存在方位取反的场景。
rm -f -- "$out.zcode-merge"
cp -- "$theirs" "$out"
