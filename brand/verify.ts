#!/usr/bin/env bun
/**
 * 品牌运行时门禁（见 `docs/fork/sync-strategy.md`）。
 *
 * 刻意**不扫二进制字符串**：npm 包名 `@oh-my-pi/*` 与上游注释里的品牌字样是
 * 刻意保留的（改名 = 数千行 import 的永久冲突面），字符串扫描只会产出噪声。
 * 这里断言的全是**用户可见行为**：rebase 把某处接线弄丢了，这些断言会红。
 *
 * 跑法：`bun brand/verify.ts`（`brand/sync.sh` 第 4 步自动执行）。
 * `--skip-cli` 跳过唯一需要 native addon 的那项（源码入口 `--version`），供发布
 * 流水线在 addon 构建**之前**先把坏 tag / 坏树拦下来 —— 其余五项全是纯 import。
 */
import * as fs from "node:fs/promises";
import * as os from "node:os";
import * as path from "node:path";
import { InternalUrlRouter } from "../packages/coding-agent/src/internal-urls/router";
import { PI_LOGO } from "../packages/coding-agent/src/modes/components/welcome";
import { ZCODE_LOGO } from "../packages/coding-agent/src/modes/components/brand-logo";
import { BRAND_APP_NAME, BRAND_CONFIG_DIR_NAME, BRAND_DOCS_SCHEME, BRAND_ENV_PREFIX } from "../packages/utils/src/brand";
import { getConfigRootDir } from "../packages/utils/src/dirs";
import { parseEnvFile } from "../packages/utils/src/env";
import { auditOverlay } from "./apply";

const repoRoot = path.join(import.meta.dir, "..");
const failures: string[] = [];
const skipCli = process.argv.includes("--skip-cli");

function check(name: string, ok: boolean, detail: string): void {
	if (ok) {
		console.log(`  ok    ${name} — ${detail}`);
		return;
	}
	console.log(`  FAIL  ${name} — ${detail}`);
	failures.push(name);
}

// 1) CLI 身份：源码入口的 --version 是 fork 产品名。
if (skipCli) {
	console.log("  skip  cli --version — --skip-cli（native addon 尚未就绪）");
} else {
	const home = await fs.mkdtemp(path.join(os.tmpdir(), "zcode-verify-"));
	try {
		const proc = Bun.spawnSync(["bun", path.join("packages", "coding-agent", "src", "cli.ts"), "--version"], {
			cwd: repoRoot,
			env: { ...Bun.env, HOME: home, USERPROFILE: home },
		});
		const out = proc.stdout.toString().trim();
		check("cli --version", out.startsWith(`${BRAND_APP_NAME}/`), out || `exit ${proc.exitCode}`);
	} finally {
		await fs.rm(home, { recursive: true, force: true });
	}
}

// 2) 配置根目录：所有 XDG/profile 路径都从这里派生。
{
	const root = getConfigRootDir();
	check("config root dir", path.basename(root) === BRAND_CONFIG_DIR_NAME, root);
}

// 3) 内部 scheme：新 scheme 为主，`omp` 别名必须同时存活 —— 上游 ~20 行测试与
//    docs/tools/*.md 全靠这条别名免改。
{
	const router = InternalUrlRouter.instance();
	check("primary docs scheme", router.canHandle(`${BRAND_DOCS_SCHEME}://docs`), `${BRAND_DOCS_SCHEME}://docs`);
	check("legacy scheme alias", router.canHandle("omp://docs"), "omp://docs");
}

// 4) env 前缀：fork 前缀镜像到内部 PI_*，旧前缀保留兼容。
{
	const dir = await fs.mkdtemp(path.join(os.tmpdir(), "zcode-verify-env-"));
	try {
		const file = path.join(dir, ".env");
		await Bun.write(file, `${BRAND_ENV_PREFIX}MODEL=x\nOMP_SMOL=y\n`);
		const parsed = parseEnvFile(file);
		check("fork env prefix", parsed.PI_MODEL === "x", `${BRAND_ENV_PREFIX}MODEL -> PI_MODEL=${parsed.PI_MODEL}`);
		check("legacy env prefix", parsed.PI_SMOL === "y", `OMP_SMOL -> PI_SMOL=${parsed.PI_SMOL}`);
	} finally {
		await fs.rm(dir, { recursive: true, force: true });
	}
}

// 5) 终端字标：welcome / splash / outro / wizard header 四个消费点都读 PI_LOGO，
//    这里同时钉住它的值来源与被消费方代码逼出来的形状约束。
{
	check("logo wired to fork mark", PI_LOGO === ZCODE_LOGO, `${PI_LOGO.length} rows`);
	const widths = new Set(PI_LOGO.map(row => row.length));
	check("logo rows equal width", PI_LOGO.length === 5 && widths.size === 1, `rows=${PI_LOGO.length} widths=${[...widths]}`);
	const illegal = [...PI_LOGO.join("")].filter(ch => !"█▀▄ ".includes(ch));
	check("logo uses full blocks only", illegal.length === 0, illegal.length === 0 ? "█▀▄ + space" : illegal.join(""));
}

// 6) Overlay：期望态按分支分档判定（`stack` 处处必须渲染态，`release` 档只在
//    release 分支上应用），判据与失败文案都在 apply.ts 里，见 auditOverlay。
{
	const audit = await auditOverlay();
	check("overlay expected state", audit.ok, audit.detail);
}

if (failures.length > 0) {
	console.error(`\nbrand/verify.ts: ${failures.length} 项失败: ${failures.join(", ")}`);
	process.exit(1);
}
console.log("\nbrand/verify.ts: 全部通过");
