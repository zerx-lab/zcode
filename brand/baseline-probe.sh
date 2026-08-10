#!/bin/sh
# brand/baseline.sh 信号行过滤的夹具。改过 SIGNAL_RE / norm_args 后跑这两条：
#
#   bash brand/baseline.sh sh "$PWD/brand/baseline-probe.sh"
#     → 期望「两侧一致」。建模 cargo 冷/热 target：错误集相同，只有路径和耗时不同。
#       若报差异，说明过滤放过了噪声 —— 下次 rebase 会把上游预存问题误判成 fork 回归。
#
#   bash brand/baseline.sh sh "$PWD/brand/baseline-probe.sh" --regression
#     → 期望只报出 `> error: fork-only regression in brand.ts` 这一行。
#       若报「一致」，说明过滤把真错误也吃了 —— 比噪声误判更危险。
#
#   bash brand/baseline.sh sh "$PWD/brand/baseline-probe.sh" --exit-drift
#     → 期望「输出一致但退出码不同」并 exit 1。建模信号正则没覆盖的失败形态：
#       输出看不出差别，只有退出码在说话。若判「一致」，退出码防护失效。
#
#   bash brand/baseline.sh true
#     → 期望「无任何输出 … 差分无效」并 exit 2。建模命令没真正跑起来
#       （worktree 失效/可执行缺失），两侧空输出绝不能算绿。
#
# 用绝对路径：基线 worktree 里没有这个未跟踪文件的副本。
echo "   Compiling pi-walker v0.1.0 ($(pwd))"
echo "error: this function could be const"
if [ "$1" = "--regression" ] && ! pwd | grep -q omp-baseline; then
	echo "error: fork-only regression in brand.ts"
fi
echo "    Finished \`dev\` profile in 0.$(date +%N)s"
if [ "$1" = "--exit-drift" ] && ! pwd | grep -q omp-baseline; then
	exit 1
fi
