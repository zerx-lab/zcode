#!/usr/bin/env bash
# Fork 同步脚本。策略详见 docs/fork/sync-strategy.md。
# 用法: bash brand/sync.sh
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

# 提交门禁（幂等）：拦截被中断的 binary build 留下的 populated 占位文件
git config core.hooksPath brand/hooks

git fetch upstream --no-tags

# 首次建立漂移基线（可动 ref，全绿后才推进）
git rev-parse --verify -q refs/brand/last-sync >/dev/null ||
	git update-ref refs/brand/last-sync upstream/main

# 1) 漂移审查：rebase 之前，先看清上游新增的品牌硬编码候选
echo "=== 上游新增品牌硬编码候选 (refs/brand/last-sync..upstream/main) ==="
git diff --unified=0 refs/brand/last-sync..upstream/main -- '*.ts' '*.rs' |
	grep '^+' | grep -vE '@oh-my-pi/|oh-my-pi/(pi|omp)-' |
	grep -inE '"omp|oh-my-pi|Oh My Pi|can1357|\.omp/|omp://' ||
	echo "(无)"

# 2) 唯一冲突点：补丁栈重放（rerere 自动重放已知解法）
git checkout zcode
git rebase upstream/main || {
	echo "补丁栈冲突：解决后 git rebase --continue，再重跑本脚本" >&2
	exit 1
}

# 3) release 为派生物：丢弃重建，overlay 永不参与三方合并
git branch -f release zcode
git checkout release
if [[ -f brand/apply.ts ]]; then
	bun brand/apply.ts
	git add -A
	git diff --cached --quiet || git commit -m "chore(brand): overlay"
else
	echo "(brand/apply.ts 尚未实现，跳过 overlay)"
fi

# 4) 门禁：全绿才推进基线
if [[ -f brand/verify.ts ]]; then
	bun brand/verify.ts
else
	echo "(brand/verify.ts 尚未实现，跳过运行时门禁)"
fi
bash brand/hooks/selftest.sh
bun check

# 5) 推进漂移基线
git update-ref refs/brand/last-sync upstream/main
echo "=== 同步完成: zcode @ $(git rev-parse --short zcode), baseline @ $(git rev-parse --short refs/brand/last-sync) ==="
