#!/usr/bin/env bun
/**
 * Overlay 渲染器（策略见 `docs/fork/sync-strategy.md`）。
 *
 * Overlay = "落在上游已有路径上、内容 100% 由本 fork 决定" 的产物，按归属分两档：
 *
 * - `stack`：**补丁栈上就必须是渲染态**。目前只有根 `README.md` —— 默认分支是
 *   zcode，GitHub 首页与一行流安装说明都从它读，等到 `release` 才品牌化就晚了。
 *   代价是它进补丁栈、参与 rebase：`.gitattributes` 把它挂到 `zcode-readme`
 *   merge driver（`brand/merge-readme.sh`）上，冲突固定解成「用 `brand/README.md`
 *   重渲染」，上游 README 的内容永不并入。
 * - `release`：只存在于派生分支 `release`。`brand/sync.sh` 每次同步都
 *   `git branch -f release zcode` 重建再重新生成 —— 结构上不可能参与三方合并。
 *   它们在补丁栈上必须**缺席**：混进去会让发布门禁红（zcode-v17.2.12-z3 就是
 *   这么挂的），所以写盘和审计都按当前分支分档。
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
 *   bun brand/apply.ts                  # 按当前分支写盘（补丁栈上只写 stack 档）
 *   bun brand/apply.ts --check          # 审计期望态，不符则退出码 1（CI / verify 用）
 *   bun brand/apply.ts --render <目标>  # 单个目标渲染到 stdout（README merge driver 用）
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
	BRAND_RELEASE_TAG_PREFIX,
	BRAND_REPO,
} from "../packages/utils/src/brand";

/** 上游仓库；overlay 里所有"回指原项目"的链接都从这里取。 */
export const UPSTREAM_REPO = "can1357/oh-my-pi";

/** 面向用户的分支：overlay 产物只存在于这里，install 一行流因此指向它。 */
export const RELEASE_BRANCH = "release";

/** 发布 tag 前缀。真源在 brand.ts（update 通道同用）；此处仅转发给 overlay 消费方。 */
export const RELEASE_TAG_PREFIX = BRAND_RELEASE_TAG_PREFIX;

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

/** 目标归属；语义见文件头。 */
export type Scope = "stack" | "release";

type Entry = { scope: Scope } & (
	| { kind: "copy"; from: string; to: string }
	| { kind: "render"; from: string; to: string }
	| { kind: "rewrite"; to: string; rules: RewriteRule[] }
);

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
	{ scope: "release", kind: "copy", from: "brand/logo/zcode-mark.svg", to: "assets/icon.svg" },
	{ scope: "release", kind: "copy", from: "brand/assets/hero.png", to: "assets/hero.png" },
	{ scope: "release", kind: "render", from: "brand/assets/banner.html", to: "assets/banner.html" },
	// 唯一的 stack 档：改它等于改补丁栈，不是改派生物。
	{ scope: "stack", kind: "render", from: "brand/README.md", to: "README.md" },
	{
		scope: "release",
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
		scope: "release",
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

/** 当前检出的分支名；detached HEAD（CI 按 tag checkout）返回空串，按补丁栈处理。 */
function currentBranch(): string {
	const proc = Bun.spawnSync(["git", "branch", "--show-current"], { cwd: repoRoot });
	return proc.success ? proc.stdout.toString().trim() : "";
}

/** 某一档里，磁盘内容与渲染产物逐字节一致的 / 不一致的目标。 */
async function statusOf(scope: Scope): Promise<{ applied: string[]; missing: string[] }> {
	const applied: string[] = [];
	const missing: string[] = [];
	for (const entry of MANIFEST) {
		if (entry.scope !== scope) continue;
		const { to, bytes } = await produce(entry);
		((await sameOnDisk(to, bytes)) ? applied : missing).push(entry.to);
	}
	return { applied, missing };
}

/**
 * 按当前分支判定**期望态**并审计。三条断言，两个分支上都有意义：
 *
 * - 任何分支：`stack` 档必须全部是渲染态 —— 这就是 README 的唯一性门禁，
 *   rebase 把上游 README 并进来会立刻红。
 * - `release`：`release` 档也必须全部应用。
 * - 其余分支：`release` 档必须全部缺席，出现一个就是产物误入补丁栈。
 *
 * 旧实现靠「README 里有没有 `BRAND_REPO`」嗅探分支，README 进补丁栈后这条嗅探
 * 恒真，于是把 `release` 档也拿到 zcode 上校验 —— zcode-v17.2.12-z3 的发布门禁
 * 就是这么红的。分支 + 分档判定不依赖任何文件内容启发式。
 */
export async function auditOverlay(): Promise<{ ok: boolean; detail: string }> {
	let stack: { applied: string[]; missing: string[] };
	let release: { applied: string[]; missing: string[] };
	try {
		stack = await statusOf("stack");
		release = await statusOf("release");
	} catch (err) {
		// rewrite 锚点消失时 produce 会抛：这本身就是要人工裁决的门禁失败。
		return { ok: false, detail: err instanceof Error ? err.message : String(err) };
	}

	if (stack.missing.length > 0) {
		return {
			ok: false,
			detail:
				`fork 独占文件未渲染或已漂移: ${stack.missing.join(", ")} —— 跑 \`bun brand/apply.ts\`。` +
				`README 的真源是 brand/README.md，永远重渲染，不与上游三方合并。`,
		};
	}
	if (currentBranch() === RELEASE_BRANCH) {
		return release.missing.length > 0
			? { ok: false, detail: `release overlay 未应用或已漂移: ${release.missing.join(", ")} —— 跑 \`bun brand/apply.ts\`` }
			: { ok: true, detail: `release 树：${stack.applied.length + release.applied.length} 个目标一致` };
	}
	return release.applied.length > 0
		? {
				ok: false,
				detail:
					`release 产物混入补丁栈: ${release.applied.join(", ")} —— ` +
					`这些只属于 release 分支，用 \`git checkout upstream/main -- <路径>\` 还原`,
			}
		: {
				ok: true,
				detail: `补丁栈树：stack ${stack.applied.length} 个一致，release ${release.missing.length} 个按预期缺席`,
			};
}

async function main(): Promise<void> {
	const renderAt = process.argv.indexOf("--render");
	if (renderAt >= 0) {
		const target = process.argv[renderAt + 1];
		const entry = MANIFEST.find(e => e.to === target);
		if (!entry) throw new Error(`--render: 未知 overlay 目标 ${target ?? "(缺参数)"}`);
		if (entry.kind === "rewrite") throw new Error(`--render 不支持 rewrite 目标 ${target}：它以磁盘上的上游文件为输入`);
		process.stdout.write((await produce(entry)).bytes);
		return;
	}

	if (process.argv.includes("--check")) {
		const audit = await auditOverlay();
		if (!audit.ok) {
			console.error(audit.detail);
			process.exit(1);
		}
		console.log(audit.detail);
		return;
	}

	// 写盘只覆盖当前分支该有的档：在 zcode 上误跑不会再把 release 产物撒进补丁栈。
	const scopes: Scope[] = currentBranch() === RELEASE_BRANCH ? ["stack", "release"] : ["stack"];
	const written: string[] = [];
	for (const entry of MANIFEST) {
		if (!scopes.includes(entry.scope)) continue;
		const { to, bytes } = await produce(entry);
		if (await sameOnDisk(to, bytes)) continue;
		written.push(entry.to);
		await Bun.write(to, bytes);
	}
	console.log(
		written.length === 0
			? `overlay 无变化 (${scopes.join("+")} 档)`
			: `overlay 已写入 (${scopes.join("+")} 档):\n  ${written.join("\n  ")}`,
	);
}

if (import.meta.main) await main();
