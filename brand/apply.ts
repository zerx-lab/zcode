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

const MANIFEST: Entry[] = [
	{ kind: "copy", from: "brand/logo/zcode-mark.svg", to: "assets/icon.svg" },
	{ kind: "copy", from: "brand/assets/hero.png", to: "assets/hero.png" },
	{ kind: "render", from: "brand/assets/banner.html", to: "assets/banner.html" },
	{ kind: "render", from: "brand/README.md", to: "README.md" },
	{
		kind: "rewrite",
		to: "scripts/install.sh",
		// fork 不发 npm：无 --ref 的源码安装改为克隆 fork 的补丁栈分支，
		// 否则 `bun install -g @oh-my-pi/pi-coding-agent` 会装回上游。
		rules: installScriptRules([
			{
				find: "install_via_bun() {\n",
				replace: `install_via_bun() {\n    [ -n "$REF" ] || REF="zcode"\n`,
			},
		]),
	},
	{
		kind: "rewrite",
		to: "scripts/install.ps1",
		rules: installScriptRules([
			{
				find: "function Install-ViaBun {\n",
				replace: 'function Install-ViaBun {\n    if (-not $Ref) { $Ref = "zcode" }\n',
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
