#!/usr/bin/env bun
/**
 * Overlay 渲染器（策略见 `docs/fork/sync-strategy.md`）。
 *
 * Overlay = "落在上游已有路径上、内容 100% 由本 fork 决定" 的产物。它们**不进
 * 补丁栈**：`brand/sync.sh` 每次同步都 `git branch -f release zcode` 重建
 * `release`，再跑本脚本重新生成 —— 结构上不可能参与三方合并，也就不可能冲突。
 *
 * 三种生成方式：
 * - `copy`：整文件由 fork 拥有（icon/hero），直接落盘；
 * - `render`：fork 拥有内容，但色值/名字必须从 `brand.ts` 取，`{{TOKEN}}` 插值；
 * - `rewrite`：上游文件仍是主体（install 脚本），只按锚点做定量替换。
 *   每条规则带 `min` 命中下限，上游改写锚点即**报错**而不是静默产出未品牌化的
 *   安装脚本 —— 这正是 rewrite 优于「整文件模板」的原因：上游对 install 脚本的
 *   改进自动跟随，只有锚点消失才需要人工介入。
 *
 * 用法：
 *   bun brand/apply.ts            # 写盘
 *   bun brand/apply.ts --check    # 只比对，有漂移则退出码 1（CI / verify 用）
 */
import * as path from "node:path";
import {
	BRAND_APP_NAME,
	BRAND_COLOR,
	BRAND_CONFIG_DIR_NAME,
	BRAND_DISPLAY_NAME,
	BRAND_DOCS_SCHEME,
	BRAND_ENV_PREFIX,
	BRAND_RAMP,
	BRAND_REPO,
} from "../packages/utils/src/brand";

/** 上游仓库；overlay 里所有"回指原项目"的链接都从这里取。 */
export const UPSTREAM_REPO = "can1357/oh-my-pi";

/** 面向用户的分支：overlay 产物只存在于这里，install 一行流因此指向它。 */
export const RELEASE_BRANCH = "release";

/** 发布 tag 前缀。上游 CI 只认 `v*`，用独立前缀避免两套发布流互相触发。 */
export const RELEASE_TAG_PREFIX = "zcode-v";

const repoRoot = path.join(import.meta.dir, "..");

/** `{{TOKEN}}` 插值表：overlay 里不允许出现第二份品牌字面量。 */
export const TOKENS: Record<string, string> = {
	APP_NAME: BRAND_APP_NAME,
	DISPLAY_NAME: BRAND_DISPLAY_NAME,
	CONFIG_DIR_NAME: BRAND_CONFIG_DIR_NAME,
	DOCS_SCHEME: BRAND_DOCS_SCHEME,
	ENV_PREFIX: BRAND_ENV_PREFIX,
	REPO: BRAND_REPO,
	UPSTREAM_REPO,
	RELEASE_BRANCH,
	COLOR: BRAND_COLOR,
	RAMP_0: BRAND_RAMP[0],
	RAMP_1: BRAND_RAMP[1],
	RAMP_2: BRAND_RAMP[2],
	RAMP_3: BRAND_RAMP[3],
	RAMP_4: BRAND_RAMP[4],
};

interface RewriteRule {
	find: string | RegExp;
	replace: string;
	/** 命中下限；实际命中数低于它说明上游改了锚点，必须人工裁决。 */
	min?: number;
}

type Entry =
	| { kind: "copy"; from: string; to: string }
	| { kind: "render"; from: string; to: string }
	| { kind: "rewrite"; to: string; rules: RewriteRule[] };

/**
 * install 脚本的共同改写：仓库、一行流分支、资产/命令名。
 *
 * `\bomp\b` 同时覆盖 `omp-windows-x64.exe`、`INSTALL_DIR/omp`、`Run 'omp'`。
 * npm 包名 `@oh-my-pi/pi-coding-agent` 刻意不改（fork 不发 npm），它不含 `\bomp\b`，
 * 也不含 `can1357/oh-my-pi`，因此不会被这两条规则误伤。
 */
function installScriptRules(extra: RewriteRule[]): RewriteRule[] {
	return [
		{ find: new RegExp(`${UPSTREAM_REPO}/main/scripts/`, "g"), replace: `${BRAND_REPO}/${RELEASE_BRANCH}/scripts/` },
		{ find: new RegExp(UPSTREAM_REPO, "g"), replace: BRAND_REPO },
		{ find: /\bomp\b/g, replace: BRAND_APP_NAME, min: 5 },
		{ find: /\bOMP Coding Agent Installer\b/g, replace: `${BRAND_DISPLAY_NAME} installer` },
		...extra,
	];
}

/**
 * fork 只有 GitHub Releases 一条通道，install 脚本的 bun/源码分支必须整条拆掉：
 *
 * - `bun install -g @oh-my-pi/pi-coding-agent` 装的是**上游** npm 包，不是本 fork；
 * - 换成克隆后 `bun install -g <clone>/packages/coding-agent` 同样装不上——成员
 *   package.json 的依赖全走 `catalog:` 协议，脱离 workspace 根解析不了
 *   （bun 报 `@oh-my-pi/pi-ai@catalog: failed to resolve`），而且源码跑还缺一个
 *   要 Rust/bazel 现编的 native addon。上游默认走 npm 所以碰不到这条路，
 *   fork 的默认路径恰好撞上：这是 zcode-v17.2.12-z1 一行流安装失败的根因。
 *
 * 因此：默认装预编译二进制；`--ref` / `-Ref` 语义收窄为 release tag；
 * `--source` / `-Source` 明确拒绝并指向 README 的源码跑法。上游那几段函数体保留原样
 * （不可达即可），避免为删代码再增加一批 rebase 锚点。
 */
const NO_SOURCE_NOTE = `${BRAND_APP_NAME} installs prebuilt binaries only: @oh-my-pi/* on npm is the upstream package.`;
const NO_SOURCE_HINT = "Re-run without the source flag for the binary, or clone the repo and run 'bun run setup'.";

const MANIFEST: Entry[] = [
	{ kind: "copy", from: "brand/logo/zcode-mark.svg", to: "assets/icon.svg" },
	{ kind: "copy", from: "brand/assets/hero.png", to: "assets/hero.png" },
	{ kind: "render", from: "brand/assets/banner.html", to: "assets/banner.html" },
	{ kind: "render", from: "brand/README.md", to: "README.md" },
	{
		kind: "rewrite",
		to: "scripts/install.sh",
		rules: installScriptRules([
			// 空 MODE 会落到上游"有 bun 就源码装"的默认分支。
			{ find: 'MODE=""\nREF=""', replace: 'MODE="binary"\nREF=""' },
			// 在参数解析处就拒绝，`case "$MODE"` 的 source 分支从此不可达。
			{
				find: '        --source)\n            MODE="source"\n            shift\n            ;;\n',
				replace: `        --source)\n            echo "${NO_SOURCE_NOTE}"\n            echo "${NO_SOURCE_HINT}"\n            exit 1\n            ;;\n`,
			},
			{
				find: "#   --source       Install via bun (installs bun if needed)\n",
				replace: "#   --source       (unsupported in this fork: prebuilt binaries only)\n",
			},
			{
				find: "#   --ref <ref>    Install specific tag/commit/branch\n",
				replace: "#   --ref <tag>    Install a specific release tag\n",
			},
			{
				find: '        echo "For branch/commit installs, use --source with --ref."\n',
				replace: `        echo "Release tags: https://github.com/${BRAND_REPO}/releases"\n`,
			},
		]),
	},
	{
		kind: "rewrite",
		to: "scripts/install.ps1",
		rules: installScriptRules([
			{
				find: '$ErrorActionPreference = "Stop"\n',
				replace:
					'$ErrorActionPreference = "Stop"\n\n' +
					"if ($Source) {\n" +
					`    Write-Host "${NO_SOURCE_NOTE}" -ForegroundColor Yellow\n` +
					`    Write-Host "${NO_SOURCE_HINT}"\n` +
					"    exit 1\n" +
					"}\n" +
					"$Binary = $true\n",
			},
			{ find: /^#   & \(\[scriptblock\]::Create\(\(irm [^\n]*\)\)\) -Source[^\n]*\n/gm, replace: "", min: 3 },
			{
				find: 'throw "Release tag not found: $Ref`nFor branch/commit installs, use -Source with -Ref."',
				replace: `throw "Release tag not found: $Ref\`nRelease tags: https://github.com/${BRAND_REPO}/releases"`,
			},
		]),
	},
];

export function render(template: string): string {
	return template.replace(/\{\{(\w+)\}\}/g, (all, key: string) => {
		const value = TOKENS[key];
		if (value === undefined) throw new Error(`unknown overlay token {{${key}}}`);
		return value;
	});
}

function rewrite(source: string, rules: RewriteRule[], target: string): string {
	// 已品牌化 → 幂等空转。`BRAND_REPO` 在上游 install 脚本里绝不出现，
	// 是可靠的 "已应用" 标记（apply 正常只跑在 zcode 的纯净树上）。
	if (source.includes(BRAND_REPO)) return source;

	let out = source;
	const misses: string[] = [];
	for (const rule of rules) {
		let hits = 0;
		if (typeof rule.find === "string") {
			const parts = out.split(rule.find);
			hits = parts.length - 1;
			out = parts.join(rule.replace);
		} else {
			if (!rule.find.global) throw new Error(`overlay rewrite regex must be global: ${String(rule.find)}`);
			hits = out.match(rule.find)?.length ?? 0;
			out = out.replace(rule.find, rule.replace);
		}
		if (hits < (rule.min ?? 1)) misses.push(`${String(rule.find)} (hits=${hits}, min=${rule.min ?? 1})`);
	}
	if (misses.length > 0) {
		throw new Error(
			`overlay rewrite anchors missing in ${target}:\n  ${misses.join("\n  ")}\n` +
				`上游改了锚点。人工裁决后更新 brand/apply.ts 的规则表。`,
		);
	}
	return out;
}

async function produce(entry: Entry): Promise<{ to: string; bytes: Uint8Array }> {
	const to = path.join(repoRoot, entry.to);
	if (entry.kind === "copy") {
		return { to, bytes: new Uint8Array(await Bun.file(path.join(repoRoot, entry.from)).arrayBuffer()) };
	}
	const encoder = new TextEncoder();
	if (entry.kind === "render") {
		return { to, bytes: encoder.encode(render(await Bun.file(path.join(repoRoot, entry.from)).text())) };
	}
	return { to, bytes: encoder.encode(rewrite(await Bun.file(to).text(), entry.rules, entry.to)) };
}

async function sameOnDisk(target: string, bytes: Uint8Array): Promise<boolean> {
	try {
		const current = new Uint8Array(await Bun.file(target).arrayBuffer());
		return current.length === bytes.length && Buffer.from(current).equals(Buffer.from(bytes));
	} catch {
		return false;
	}
}

async function main(): Promise<void> {
	const checkOnly = process.argv.includes("--check");
	const drifted: string[] = [];

	for (const entry of MANIFEST) {
		const { to, bytes } = await produce(entry);
		if (await sameOnDisk(to, bytes)) continue;
		drifted.push(entry.to);
		if (!checkOnly) await Bun.write(to, bytes);
	}

	if (checkOnly) {
		if (drifted.length > 0) {
			console.error(`overlay 未应用或已漂移:\n  ${drifted.join("\n  ")}\n运行 \`bun brand/apply.ts\``);
			process.exit(1);
		}
		console.log(`overlay 已是最新 (${MANIFEST.length} 个目标)`);
		return;
	}
	console.log(
		drifted.length === 0
			? `overlay 无变化 (${MANIFEST.length} 个目标)`
			: `overlay 已写入:\n  ${drifted.join("\n  ")}`,
	);
}

if (import.meta.main) await main();
