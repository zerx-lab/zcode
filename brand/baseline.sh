#!/usr/bin/env bash
# 基线差分：把同一条命令在 zcode 和 upstream/main 上各跑一遍，比对失败集。
#
#   bash brand/baseline.sh bun test test/discovery/ test/marketplace/
#   bash brand/baseline.sh --clean            # 删掉基线 worktree
#
# 存在的理由：rebase 后 `bun check` / `bun test` 报错时，第一个要回答的问题永远是
# 「这是 fork 引入的，还是上游本来就在本机这个平台上挂」。凭 diff 猜会猜错 —— 本机
# 上 crates/pi-walker 的 clippy 和 31 个 coding-agent 测试都是上游预存的 Windows
# 问题，与补丁栈零交集。唯一可靠的判据是在同一台机器上跑同一条命令做差分。
#
# 两个必须踩过才知道的坑，都已内建：
#   1. 新 worktree 没有 node_modules，要 bun install；
#   2. 新 worktree 没有 packages/natives/native/*.node，不拷过去的话每个测试文件都
#      会以 "Cannot find module pi_natives..." 秒失败，看起来像真回归。
set -euo pipefail

repo_root=$(git rev-parse --show-toplevel)
cd "$repo_root"
baseline_dir=$(cd .. && pwd)/omp-baseline

remove_baseline() {
	git worktree remove --force "$baseline_dir" 2>/dev/null || git worktree prune
	if [[ -e $baseline_dir ]]; then rm -rf "$baseline_dir" 2>/dev/null || true; fi
	# Windows: node_modules 里的 workspace 链接是 junction，POSIX rm 删不掉
	# （Permission denied）。必须交给 cmd.exe，且路径要转成 Windows 形式 ——
	# 直接把 /mnt/c/... 的斜杠翻过来会得到 \mnt\c\...，rmdir 找不到。
	if [[ -e $baseline_dir ]] && command -v cmd.exe >/dev/null 2>&1; then
		local win
		win=$(command -v wslpath >/dev/null 2>&1 && wslpath -w "$baseline_dir" || cygpath -w "$baseline_dir")
		cmd.exe /c "rmdir /s /q \"$win\"" >/dev/null 2>&1 || true
	fi
	if [[ -e $baseline_dir ]]; then
		echo "无法删除 $baseline_dir，请手工清理后重试" >&2
		return 1
	fi
}

if [[ ${1-} == "--clean" ]]; then
	remove_baseline
	echo "基线 worktree 已删除: $baseline_dir"
	exit 0
fi

if [[ $# -eq 0 ]]; then
	sed -n '2,8p' "$0" >&2
	exit 2
fi

lock_stamp="$baseline_dir/.baseline-lock"

# 网络抖动不该毁掉一次差分：抓不到就用本地已有的 upstream/main 继续，只是提醒。
git fetch upstream --no-tags -q ||
	echo "警告: fetch upstream 失败，使用本地已有的 upstream/main（$(git rev-parse --short upstream/main)）" >&2
# 只有真是 worktree 才复用；残留的空目录/半删干净的目录先清掉，否则 worktree add
# 会被占位路径挡住。
if git -C "$baseline_dir" rev-parse --git-dir >/dev/null 2>&1; then
	git -C "$baseline_dir" checkout -q --detach upstream/main
	git -C "$baseline_dir" reset -q --hard upstream/main
	git -C "$baseline_dir" clean -qfd -e node_modules -e .baseline-lock -e 'packages/natives/native/*.node'
else
	if [[ -e $baseline_dir ]]; then remove_baseline; fi
	git worktree add -q -f --detach "$baseline_dir" upstream/main
fi

echo "=== 基线 worktree ($(git -C "$baseline_dir" rev-parse --short HEAD)) ==="
# bun install 只在首次或 lockfile 变了时跑（~30s）；此外本脚本不需要 bun，
# 待测命令自己的依赖由调用方负责。
lock_now=$(git -C "$baseline_dir" rev-parse HEAD:bun.lock 2>/dev/null || echo none)
if [[ ! -d "$baseline_dir/node_modules" || $(cat "$lock_stamp" 2>/dev/null) != "$lock_now" ]]; then
	if ! command -v bun >/dev/null 2>&1; then
		echo "基线 worktree 需要 bun install，但 PATH 上没有 bun；换一个装了 bun 的 shell" >&2
		exit 2
	fi
	echo "--- bun install（首次或 lockfile 变更）---"
	(cd "$baseline_dir" && bun install --silent)
	printf '%s' "$lock_now" >"$lock_stamp"
fi
# 上游 CI 产物不在仓库里，本地构建好的 addon 直接复用；缺了的话基线侧每个用到原生
# 依赖的测试都会以 "Cannot find module pi_natives..." 秒挂，伪装成全量回归。
cp packages/natives/native/*.node "$baseline_dir/packages/natives/native/" 2>/dev/null ||
	echo "警告: 本地没有 .node，基线侧原生依赖测试会全挂（先跑 bun --cwd=packages/natives run build）" >&2

out_dir=$(mktemp -d)
trap 'rm -rf "$out_dir"' EXIT

run_side() {
	local label=$1 dir=$2
	shift 2
	echo "=== $label: $* ===" >&2
	(cd "$dir" && "$@") >"$out_dir/$label.log" 2>&1 || true
	# bun test 的失败行；非 test 命令没有该行，退化为全量输出比对
	sed -e 's/ \[[0-9.]*ms\]$//' "$out_dir/$label.log" | sed -n '/^(fail)/p' | sort >"$out_dir/$label.fails"
	if [[ ! -s "$out_dir/$label.fails" ]]; then
		sort "$out_dir/$label.log" >"$out_dir/$label.fails"
	fi
}

run_side zcode "$repo_root" "$@"
run_side upstream "$baseline_dir" "$@"

echo
if diff -q "$out_dir/upstream.fails" "$out_dir/zcode.fails" >/dev/null; then
	echo "两侧输出一致（$(wc -l <"$out_dir/zcode.fails" | tr -d ' ') 行）—— 无 fork 引入的回归。"
	echo "仍在报错的话，那是上游在本机这个平台上的预存问题，不阻塞同步。"
	exit 0
fi

echo "差异（< 仅上游有 / > 仅 zcode 有 = fork 引入，必须修）："
diff "$out_dir/upstream.fails" "$out_dir/zcode.fails" || true
exit 1
