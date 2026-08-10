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

# 删除前必须先「认领」：要么是本仓的有效基线 worktree（is_our_worktree），要么
# 带本脚本自己的所有权标记 .baseline-lock（覆盖「装过但 .git 反链已坏」的半残态）。
# 刻意不用 worktree 注册表当判据：注册表只记路径，用户删掉 worktree 后在原路径
# 放别的东西时注册项还在，按它认领就是在删无关数据。两者都不满足一律拒绝，
# 交人工处理。
is_claimed_baseline() {
	[[ $(realpath "$baseline_dir" 2>/dev/null) != "$(realpath "$repo_root")" ]] || return 1
	is_our_worktree && return 0
	[[ -e $baseline_dir/.baseline-lock ]]
}

remove_baseline() {
	# prune 只在目录已不存在时做（清掉残留注册）；目录还在时先别动注册表 ——
	# 注册状态正是下面认领判据的一半。
	[[ -e $baseline_dir ]] || { git worktree prune; return 0; }
	# 空目录随手清掉：rmdir 只删得动空目录，天然无破坏性。
	if [[ -d $baseline_dir && ! -L $baseline_dir ]] && rmdir "$baseline_dir" 2>/dev/null; then
		return 0
	fi
	if ! is_claimed_baseline; then
		echo "拒绝删除 $baseline_dir：既不是本仓的有效基线 worktree，也没有 .baseline-lock 所有权标记。" >&2
		echo "该路径可能已被挪作他用；请人工确认内容后自行删除（或移走）再重跑。" >&2
		return 1
	fi
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

# $baseline_dir 必须是「本仓的一个 *链接* worktree 的根」，而不是恰好能 git 起来
# 的别的目录。三条判据缺一不可：
#   1. git-common-dir 指回本仓 .git —— 挡「向上爬进别的仓库」（陈旧 .git 文件、
#      删剩的空壳都会这样，那时 checkout --detach 会改写别人的 HEAD）；
#   2. canonical toplevel == canonical $baseline_dir —— 必须是 worktree 根本身，
#      不是某个 checkout 里的子目录；
#   3. canonical toplevel != 本仓根 —— 挡「junction/symlink 指回主 checkout」，
#      否则下面的 reset --hard + clean -fd 会直接毁掉真工作树。
is_our_worktree() {
	local common top
	common=$(git -C "$baseline_dir" rev-parse --path-format=absolute --git-common-dir 2>/dev/null) || return 1
	[[ $common == "$repo_root/.git" ]] || return 1
	top=$(git -C "$baseline_dir" rev-parse --path-format=absolute --show-toplevel 2>/dev/null) || return 1
	top=$(realpath "$top" 2>/dev/null) || return 1
	[[ $top == "$(realpath "$baseline_dir" 2>/dev/null)" && $top != "$(realpath "$repo_root")" ]]
}

if [[ ${1-} == "--clean" ]]; then
	remove_baseline
	echo "基线 worktree 已删除: $baseline_dir"
	exit 0
fi

if [[ ${1-} == "--filter" ]]; then
	SIGNAL_RE=$2
	shift 2
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
if is_our_worktree; then
	git -C "$baseline_dir" checkout -q --detach upstream/main
	git -C "$baseline_dir" reset -q --hard upstream/main
	git -C "$baseline_dir" clean -qfd -e node_modules -e .baseline-lock -e 'packages/natives/native/*.node'
else
	if [[ -e $baseline_dir ]]; then remove_baseline; fi
	git worktree add -q -f --detach "$baseline_dir" upstream/main
fi
# 差分的前提是基线侧真的站在 upstream/main 上。worktree 半失效时上面的分支可能
# 静默走偏，这里最后把一次口径钉死，不满足就拒绝出结论。
if [[ $(git -C "$baseline_dir" rev-parse HEAD) != $(git rev-parse upstream/main) ]]; then
	echo "错误: 基线 worktree HEAD 不在 upstream/main，差分口径已失效（试试 --clean 后重跑）" >&2
	exit 2
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

# 两侧的路径、耗时、编译进度必然不同（基线 worktree 的 target/ 是冷的），直接比
# 全量输出等于保证误判。先把这些抹平，再只留「信号行」。
norm_args=(-e 's/ \[[0-9.]*ms\]$//' -e 's/[0-9][0-9]*\(\.[0-9][0-9]*\)\{0,1\}\(ms\|us\|ns\|s\)\([^a-zA-Z0-9]\|$\)/<T>\3/g')
for p in "$repo_root" "$baseline_dir"; do
	norm_args+=(-e "s|$p|<ROOT>|g")
	for conv in "wslpath -w" "wslpath -m" "cygpath -w" "cygpath -m"; do
		if command -v "${conv%% *}" >/dev/null 2>&1; then
			w=$($conv "$p" 2>/dev/null) || continue
			[[ -n $w ]] && norm_args+=(-e "s|$(printf '%s' "$w" | sed 's|\\\\|\\\\\\\\|g')|<ROOT>|g")
		fi
	done
done

# 信号行：bun test 的失败行，或编译器/运行时的错误行。两者都没有才退回全量比对，
# 并明确警告结论可能是噪声。--filter 可覆盖。
SIGNAL_RE=${SIGNAL_RE:-'^\(fail\)|^error|^warning:|^[[:space:]]*error\[|panicked at|^FAIL '}

run_side() {
	local label=$1 dir=$2
	shift 2
	echo "=== $label: $* ===" >&2
	local status=0
	(cd "$dir" && "$@") >"$out_dir/$label.log" 2>&1 || status=$?
	echo "$status" >"$out_dir/$label.status"
	# 空输出 = 命令根本没跑起来（目录坏了 / 可执行缺失）。这时两侧会「一致地
	# 什么都没有」，绝不能算绿 —— ../omp-baseline 失效时就是这样假绿过。
	if [[ ! -s "$out_dir/$label.log" ]]; then
		echo "错误: $label 侧无任何输出（exit=$status），命令没有真正执行，差分无效" >&2
		exit 2
	fi
	# 126/127 = 命令不可执行/不存在。两侧会「一致地起不来」，同样不能算绿。
	if [[ $status == 126 || $status == 127 ]]; then
		echo "错误: $label 侧命令没起来（exit=$status），检查命令拼写/PATH 后重跑" >&2
		exit 2
	fi
	sed "${norm_args[@]}" "$out_dir/$label.log" >"$out_dir/$label.norm"
	grep -E "$SIGNAL_RE" "$out_dir/$label.norm" | sort >"$out_dir/$label.fails" || true
	if [[ ! -s "$out_dir/$label.fails" ]]; then
		noisy=1
		sort "$out_dir/$label.norm" >"$out_dir/$label.fails"
	fi
}
noisy=0

run_side zcode "$repo_root" "$@"
run_side upstream "$baseline_dir" "$@"

u_status=$(<"$out_dir/upstream.status")
z_status=$(<"$out_dir/zcode.status")
mode="信号行（$SIGNAL_RE）"
if ((noisy)); then mode="全量输出（两侧都没有信号行）"; fi
echo
echo "比对口径: $mode；退出码: upstream=$u_status / zcode=$z_status"
if diff -q "$out_dir/upstream.fails" "$out_dir/zcode.fails" >/dev/null; then
	# 行集一致但退出码不同 = 过滤把真实差异吃掉了（信号正则没覆盖这条命令的
	# 失败形态）。这正是假绿的另一半，宁可误报也不放行。
	if [[ $u_status != "$z_status" ]]; then
		echo "输出一致但退出码不同 —— 过滤可能吃掉了真实差异，用 --filter '<正则>' 指定这条命令的错误行特征后重跑再下结论。"
		exit 1
	fi
	# 没有信号行 + 非零退出 = 命令多半根本没跑成（路径/参数错），两侧只是
	# 「一致地起不来」。这种「一致」不许当同步依据。
	if ((noisy)) && [[ $u_status != 0 || $z_status != 0 ]]; then
		echo "两侧输出一致，但无信号行且退出码非零 —— 命令可能没真正执行；核对命令与路径，或用 --filter '<正则>' 后重跑。"
		exit 2
	fi
	echo "两侧一致（$(wc -l <"$out_dir/zcode.fails" | tr -d ' ') 行，两侧 exit=$u_status）—— 无 fork 引入的回归。"
	echo "仍在报错的话，那是上游在本机这个平台上的预存问题，不阻塞同步。"
	exit 0
fi

echo "差异（< 仅上游有 / > 仅 zcode 有 = fork 引入，必须修）："
diff "$out_dir/upstream.fails" "$out_dir/zcode.fails" || true
if ((noisy)); then
	echo
	echo "注意: 走的是全量比对，路径/耗时已抹平但仍可能混入噪声。" >&2
	echo "用 --filter '<正则>' 指定这条命令的错误行特征后重跑再下结论。" >&2
fi
exit 1
