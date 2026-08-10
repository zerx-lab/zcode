#!/usr/bin/env bash
# Fork 同步脚本。策略详见 docs/fork/sync-strategy.md。
# 用法: bash brand/sync.sh
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

# 提交门禁（幂等）：拦截被中断的 binary build 留下的 populated 占位文件
git config core.hooksPath brand/hooks

# rerere：记录冲突解法，供 CI (sync-upstream.yml) 自动重放
git config rerere.enabled true
git config rerere.autoupdate true

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

# 3) release 为派生物：丢弃重建，overlay 永不参与三方合并。
# 全程结束必须回到 zcode —— 停在 release 上的话，之后按 sync-upstream.md 做的
# changelog / --fixup 提交会落在派生分支上，下次 `git branch -f release zcode`
# 无声丢掉。失败退出也要回，因为要修的东西都在 zcode。
trap 'git checkout -q zcode 2>/dev/null || true' EXIT
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
bun check || {
	echo "bun check 失败。先做基线差分再改代码，别靠读 diff 猜是不是 fork 引入的：" >&2
	echo "  bash brand/baseline.sh bun run check:ts" >&2
	echo "  bash brand/baseline.sh --filter '^error' bun run check:rs" >&2
	exit 1
}

# 5) 推进漂移基线 + 发布 rerere 解法给 CI
git update-ref refs/brand/last-sync upstream/main
if [[ -d .git/rr-cache && -n $(ls -A .git/rr-cache 2>/dev/null) ]]; then
	gitdir=$(git rev-parse --absolute-git-dir)
	rr_tree=$(
		export GIT_DIR=$gitdir GIT_WORK_TREE=$gitdir/rr-cache GIT_INDEX_FILE=$gitdir/rr-pub-index
		git -C "$GIT_WORK_TREE" add -A && git write-tree
	)
	rm -f "$gitdir/rr-pub-index"
	if [[ -n $rr_tree ]]; then
		rr_commit=$(git commit-tree "$rr_tree" -m "chore(brand): rerere cache snapshot")
		git update-ref refs/brand/rr-cache "$rr_commit"
		git push origin +refs/brand/rr-cache:refs/brand/rr-cache
	fi
fi
echo "=== 同步完成: zcode @ $(git rev-parse --short zcode), baseline @ $(git rev-parse --short refs/brand/last-sync) ==="
echo "（HEAD 已回到 zcode；release 是派生物，别在上面提交）"
