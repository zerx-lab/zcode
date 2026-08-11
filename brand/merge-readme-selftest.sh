#!/usr/bin/env bash
# brand/merge-readme.sh 的行为探针。用法: bash brand/merge-readme-selftest.sh
#
# 这条路径平时一次都不跑：只有上游改了 README 且 rebase 撞上时 git 才会调它。
# 等真出事再发现 driver 坏了，代价是一次手工解冲突，外加一次"顺手把上游 README
# 合进补丁栈"的机会——README 的唯一性就是这么丢的。探针在临时 repo 里造一次真实
# 的 rebase 冲突，断言产物 = brand/README.md 的渲染结果。
#
# 三个方向都要测：
#   1. 命中：冲突被 driver 解成重渲染（fork 侧内容都不是它，证明是渲染不是取 theirs）
#   2. 负对照：不注册 driver 时确实会冲突（否则第 1 项可能只是 git 自己合并成功了）
#   3. 回退：模板不在工作树里时取 theirs，而不是留下半成品
set -euo pipefail
repo_root=$(git rev-parse --show-toplevel)
cd "$repo_root"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
failures=0
note() { printf '%s\n' "$1" >&2; }

expected="$work/expected-readme"
bun brand/apply.ts --render README.md >"$expected"

# 临时 repo 只装 driver 需要的最小集合：driver 本体、overlay 模板与它的品牌真源。
# 全部直接拷真文件，探针因此跟着实现走，不留手写快照。刻意**不**放版本内的
# .gitattributes —— 挂载点在 .git/info/attributes，由 brand/git-setup.sh 写入，
# 场景 1 绿就同时证明了这条挂载真的生效。
scaffold() {
	local dir=$1
	mkdir -p "$dir/brand" "$dir/packages/utils/src"
	cp brand/merge-readme.sh brand/git-setup.sh brand/apply.ts brand/README.md "$dir/brand/"
	cp packages/utils/src/brand.ts packages/utils/src/brand-consts.ts "$dir/packages/utils/src/"
	git init -q -b upstream "$dir"
	git -C "$dir" config user.email probe@example.com
	git -C "$dir" config user.name probe
}

# 造一次 upstream 与补丁栈都改了 README 的 rebase。$1=repo 目录，$2=补丁栈侧内容。
# 回声 rebase 的退出码，README 留在工作树里供调用方断言。
replay_conflict() {
	local dir=$1 fork_side=$2
	(
		cd "$dir"
		printf 'upstream readme\nbase line\n' >README.md
		git add -A
		git commit -qm base
		git branch fork
		printf 'upstream readme (changed by upstream)\nbase line\n' >README.md
		git commit -qam upstream
		git checkout -q fork
		printf '%s' "$fork_side" >README.md
		git commit -qam 'fork readme'
		git rebase upstream >/dev/null 2>&1
	)
}

# 1) 命中：driver 已注册 → 无冲突，且产物是重渲染结果而不是 fork 侧的哨兵内容。
hit="$work/hit"
scaffold "$hit"
(cd "$hit" && bash brand/git-setup.sh >/dev/null)
if replay_conflict "$hit" 'fork side sentinel — 不该出现在结果里'; then
	if cmp -s "$hit/README.md" "$expected"; then
		printf '  ok    driver 命中 — rebase 冲突解成 brand/README.md 重渲染\n'
	else
		note "  FAIL  driver 命中 — README 不等于渲染产物"
		failures=$((failures + 1))
	fi
else
	note "  FAIL  driver 命中 — rebase 未能自动完成（driver 没被调用或退出非零）"
	(cd "$hit" && git rebase --abort >/dev/null 2>&1 || true)
	failures=$((failures + 1))
fi

# 2) 负对照：不注册 driver（.gitattributes 的挂载点在，driver 未定义 → 默认三方合并）
#    必须冲突。这一项翻绿说明第 1 项证明不了任何事。
ctl="$work/control"
scaffold "$ctl"
if replay_conflict "$ctl" 'fork side sentinel'; then
	note "  FAIL  负对照 — 没有 driver 也自动合并了，第 1 项失去意义"
	failures=$((failures + 1))
else
	printf '  ok    负对照 — 无 driver 时 rebase 确实冲突\n'
	(cd "$ctl" && git rebase --abort >/dev/null 2>&1 || true)
fi

# 3) 回退：模板不在工作树里（重放序列还没轮到引入它的那个提交）→ 取 theirs。
fb="$work/fallback"
mkdir -p "$fb"
(
	cd "$fb"
	git init -q .
	printf 'ours\n' >a
	printf 'theirs\n' >b
	bash "$repo_root/brand/merge-readme.sh" a b
)
if [[ "$(cat "$fb/a")" == theirs ]]; then
	printf '  ok    回退 — 无 brand/README.md 时取补丁栈侧(%%B)\n'
else
	note "  FAIL  回退 — 期望 theirs，实得 $(cat "$fb/a")"
	failures=$((failures + 1))
fi

if ((failures)); then
	note ""
	note "merge driver 探针 $failures 项失败"
	exit 1
fi
printf '\nmerge driver 探针全绿\n'
