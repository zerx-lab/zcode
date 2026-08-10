#!/usr/bin/env bash
# brand/hooks/pre-commit 的行为探针。用法: bash brand/hooks/selftest.sh
#
# 两个方向都必须测：漏报（populated 产物混过去）和误报（纯占位形态被拒）。
# 误报方向不是理论风险 —— mupdf 占位文件的说明注释里就写着不带分号的
# `with { type: "file" }`，判据一旦少了分号，占位形态每次都会被拒，而且提示的
# gen:mupdf:reset 写回来的还是同一份注释，用户无从解除。
set -euo pipefail
repo_root=$(git rev-parse --show-toplevel)
cd "$repo_root"
hooks_dir="$repo_root/brand/hooks"
work=$(mktemp -d)
fixtures="$work/.fixtures"
mkdir -p "$fixtures"
# 探针只读工作区、只写临时 repo，不改任何 checked-in 文件。
trap 'rm -rf "$work"' EXIT

NATIVE_STUB=packages/natives/native/embedded-addon.js
MUPDF_STUB=packages/coding-agent/src/utils/mupdf-wasm-embed.ts
STATS_STUB=packages/stats/src/embedded-client.generated.txt
ARCHIVE=packages/natives/native/embedded-addons.linux-x64.tar.gz

git init -q "$work"
git -C "$work" config user.email probe@example.com
git -C "$work" config user.name probe
git -C "$work" config core.hooksPath "$hooks_dir"
# 真实占位形态直接取自工作区，探针因此跟着上游 stub 文案走，不会各写一份。
for stub in "$NATIVE_STUB" "$MUPDF_STUB" "$STATS_STUB"; do
	mkdir -p "$work/$(dirname "$stub")"
	cp "$repo_root/$stub" "$work/$stub"
done
git -C "$work" add -A
git -C "$work" commit -q -m base --no-verify

failures=0
# $1 accept|reject, $2 用例名, $3 相对路径, $4 内容
probe() {
	local want=$1 name=$2 file=$3 body=$4 out status
	mkdir -p "$work/$(dirname "$file")"
	printf '%s' "$body" >"$work/$file"
	# 哨兵保证每轮都有可提交的改动：占位形态与 base 逐字节相同时，空提交会以
	# 非零码退出，会被误读成 hook 拒绝。
	printf '%s' "$name" >"$work/probe-nonce.txt"
	git -C "$work" add -A -f -- "$file" probe-nonce.txt
	set +e
	out=$(git -C "$work" commit -q -m "probe: $name" 2>&1)
	status=$?
	set -e
	if [[ $status -eq 0 ]]; then
		git -C "$work" reset -q --hard HEAD~1
	else
		git -C "$work" reset -q HEAD -- "$file" probe-nonce.txt
		git -C "$work" checkout -q -- "$file" 2>/dev/null || rm -f "$work/$file"
		rm -f "$work/probe-nonce.txt"
	fi
	local got=accept
	[[ $status -ne 0 ]] && got=reject
	if [[ $got == "$want" ]]; then
		printf 'ok   %-38s %s\n' "$name" "$got"
	else
		printf 'FAIL %-38s want=%s got=%s\n%s\n' "$name" "$want" "$got" "$out" >&2
		failures=$((failures + 1))
	fi
}

# populated 夹具从生成器的 generated 模板里现抽 import 行，不留手写快照。
#
# 手写快照的问题：上游一改 generated 模板的 import 写法（去分号、拆 with 属性），
# pre-commit 的 marker 会静默失配（漏报），而手写夹具还是旧写法照样被拒 —— 两个
# 方向同时假绿。抽模板则夹具跟着上游走：写法一变，夹具就不再命中 marker，
# `*-populated` 用例立刻从 reject 翻成 accept 报错。
#
# 不直接跑 `gen:native` / `gen:mupdf`：那要求探针所在 shell 的 PATH 上有 bun
# （WSL / 裸 Git Bash 常常没有），拿不到夹具就只能 SKIP，等于在最需要覆盖的
# 环境里退化成假绿。抽模板零依赖。
# $1 生成器源文件, $2 夹具落点
extract_generated_import() {
	local src=$1 dest=$2 line
	# 只认模板里顶格的 import 行；占位模板里那句同名注释以 `//` 开头，不会命中。
	# 故意不要求分号 —— 上游去掉分号时要让夹具照样抽出来，由 probe 报出失配。
	line=$(sed -n 's/^\(import .*with { type: "file".*\)$/\1/p' "$repo_root/$src" | sed -n 1p)
	if [[ -z $line ]]; then
		printf 'FAIL %-38s 无法从 %s 抽出 generated import 行\n' "$(basename "$dest")-fixture" "$src" >&2
		failures=$((failures + 1))
		return 0
	fi
	# 模板里的 ${JSON.stringify(...)} 插值换成字面路径，其余原样保留。
	printf '%s\n' "$line" | sed 's/\${[^}]*}/".\/build-artifact"/g' >"$dest"
}

extract_generated_import packages/natives/scripts/embed-native.ts "$fixtures/native"
extract_generated_import packages/coding-agent/scripts/embed-mupdf-wasm.ts "$fixtures/mupdf"

if [[ -s "$fixtures/native" ]]; then
	probe reject native-populated "$NATIVE_STUB" "$(cat "$fixtures/native")
export const embeddedAddon = { platformTag: \"linux-x64\", version: \"0.0.0\", files: [] };"
fi
if [[ -s "$fixtures/mupdf" ]]; then
	probe reject mupdf-populated "$MUPDF_STUB" "$(cat "$fixtures/mupdf")
export function loadEmbeddedMupdfWasm(): Uint8Array | undefined { return readFileSync(wasmPath); }"
fi
probe reject stats-populated "$STATS_STUB" "ZmFrZS1iYXNlNjQ="
probe reject archive-staged "$ARCHIVE" "not a real archive"
# 回归：占位形态原文（mupdf 那份注释里含不带分号的同一串）必须放行。
probe accept native-stub-verbatim "$NATIVE_STUB" "$(cat "$repo_root/$NATIVE_STUB")"
probe accept mupdf-stub-verbatim "$MUPDF_STUB" "$(cat "$repo_root/$MUPDF_STUB")"
probe accept stats-stub-empty "$STATS_STUB" ""
probe accept unrelated-source "packages/coding-agent/src/probe.ts" "export const x = 1;"

if ((failures)); then
	printf '\n%d 个用例失败\n' "$failures" >&2
	exit 1
fi
printf '\npre-commit 探针全绿\n'
