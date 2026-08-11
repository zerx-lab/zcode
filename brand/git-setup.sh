#!/usr/bin/env bash
# 同步所需的仓库级 git 配置（幂等）。brand/sync.sh 与 .github/workflows/sync-upstream.yml
# 共用这一份 —— 两处各写一份 config 必然漂移，而漂移的表现是"CI 上 rebase 结果与
# 本地不同"这种最难查的一类。
#
# 用法: bash brand/git-setup.sh
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

# 提交门禁：拦截被中断的 binary build 留下的 populated 占位文件（brand/hooks/pre-commit）
git config core.hooksPath brand/hooks

# rerere：记录补丁栈冲突解法，本地解完后推到 refs/brand/rr-cache 供 CI 重放
git config rerere.enabled true
git config rerere.autoupdate true

# README 冲突固定解成"用 brand/README.md 重渲染"，上游 README 的内容永不并入。
git config merge.zcode-readme.name "zcode README（重渲染，不三方合并）"
git config merge.zcode-readme.driver "bash brand/merge-readme.sh %A %B"

# 挂载点走 .git/info/attributes 而**不是**版本内的 .gitattributes：后者是补丁栈里的
# 一行，rebase 重放到它自身的 commit 之前，属性还没进工作树——恰好是最需要 driver 的
# 那几个早期提交上失效（docs/fork/sync-strategy.md 当初据此禁掉 merge driver）。
# 本地文件与重放位置无关，全程有效；代价是它不随 clone 走，所以必须由本脚本装。
attributes="$(git rev-parse --git-common-dir)/info/attributes"
mount="README.md merge=zcode-readme"
mkdir -p "$(dirname "$attributes")"
if ! grep -qxF "$mount" "$attributes" 2>/dev/null; then
	printf '# zcode fork: 见 brand/merge-readme.sh（由 brand/git-setup.sh 写入）\n%s\n' "$mount" >>"$attributes"
fi
