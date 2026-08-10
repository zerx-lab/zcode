/**
 * 产出自包含部署目录 `dist/`：
 *
 * - `dist/server.js`            —— 服务端 bundle（`bun dist/server.js` 直接跑）
 * - `dist/public/web/`          —— collab-web 访客 UI（根路径）
 * - `dist/public/dash/`         —— dashboard SPA（/dash/）
 * - `dist/public/share-viewer.html` —— share viewer（GET /s/<id>）
 *
 * share viewer 生成内联复刻了 packages/coding-agent/scripts/generate-share-viewer.ts
 * （上游脚本用 `new URL(...).pathname` 读文件，在 Windows 上得到 `/C:/...` 而失败；
 * 模板函数本身经 resolveBundledHtmlAssetPath 已是跨平台的）。
 */
import * as fs from "node:fs/promises";
import * as path from "node:path";
import { generateThemeStyles, getTemplate } from "@oh-my-pi/pi-coding-agent/export/html/index";
import { BRAND_APP_NAME, BRAND_DISPLAY_NAME } from "@oh-my-pi/pi-utils/brand-consts";
import { compile } from "@tailwindcss/node";
import { $ } from "bun";

const pkgDir = path.resolve(import.meta.dir, "..");
const repoRoot = path.resolve(pkgDir, "../..");
const outDir = path.join(pkgDir, "dist");
const publicDir = path.join(outDir, "public");

/** 与 packages/stats/build.ts 相同的 className 扫描。 */
async function extractTailwindClasses(dir: string): Promise<Set<string>> {
	const classes = new Set<string>();
	const classPattern = /className\s*=\s*["'`]([^"'`]+)["'`]/g;
	async function scanDir(currentDir: string): Promise<void> {
		const entries = await fs.readdir(currentDir, { withFileTypes: true });
		for (const entry of entries) {
			const fullPath = path.join(currentDir, entry.name);
			if (entry.isDirectory()) {
				await scanDir(fullPath);
			} else if (entry.isFile() && /\.(tsx|ts|jsx|js)$/.test(entry.name)) {
				const content = await Bun.file(fullPath).text();
				for (const match of content.matchAll(classPattern)) {
					for (const cls of match[1].split(/\s+/)) {
						if (cls) classes.add(cls);
					}
				}
			}
		}
	}
	await scanDir(dir);
	return classes;
}

await fs.rm(outDir, { recursive: true, force: true });

// ── 1. collab-web 访客 UI ────────────────────────────────────────────────
console.log("building collab-web guest UI ...");
const collabWebDir = path.join(repoRoot, "packages/collab-web");
await $`bun run build`.cwd(collabWebDir);
await fs.cp(path.join(collabWebDir, "dist"), path.join(publicDir, "web"), { recursive: true });

// ── 2. share viewer ─────────────────────────────────────────────────────
// 上游 web 调色板是品牌粉/紫（--accent #ed4abf、#945ff9/#b281d6 家族）；
// 按 oauth-brand 先例在主题 <style> 之后追加同选择器覆盖（后来者胜），
// 把主题面收敛到 fork 蓝（#1677ff，面/字色相 307→259）。语法高亮与
// 信号色（ok/err/warn）不动——那是可读性调色板，不是主题色。
const BLUE_COMMON = [
	"--accent: #1677ff",
	"--borderAccent: #4096ff",
	"--customMessageLabel: #4096ff",
	"--thinkingLow: #4096ff",
	"--thinkingMedium: #69b1ff",
	"--statusLineUntracked: #4096ff",
	"--statusLineOutput: #69b1ff",
	"--statusLineCost: #69b1ff",
].join("; ");
const BLUE_DARK = `${BLUE_COMMON}; --accent-muted: #1677ff2e; --bg: #0b0f16; --bg-raised: #111722; --bg-inset: #060910; --bg-overlay: #19212e; --fg: #e3e7ec; --fg-muted: #9aa4b1; --fg-faint: #68717c;`;
const BLUE_LIGHT = `${BLUE_COMMON}; --accent-muted: #1677ff24; --bg: oklch(0.985 0.004 259); --bg-inset: oklch(0.95 0.006 259); --bg-overlay: oklch(0.965 0.006 259); --fg: oklch(0.26 0.03 259); --fg-muted: oklch(0.46 0.03 259); --fg-faint: oklch(0.58 0.025 259);`;
const blueOverrides = [
	`:root, :root[data-theme="dark"] { ${BLUE_DARK} }`,
	`:root[data-theme="light"] { ${BLUE_LIGHT} }`,
	`@media (prefers-color-scheme: light) { :root:not([data-theme]) { ${BLUE_LIGHT} } }`,
].join("\n");

console.log("generating share viewer ...");
const loaderJs = await Bun.file(path.join(repoRoot, "packages/coding-agent/src/export/html/share-loader.js")).text();
const themeStyles = await generateThemeStyles("web");
const viewer = getTemplate()
	.replace("<theme-vars/>", () => `<style>${themeStyles}</style>\n  <style>${blueOverrides}</style>`)
	.replace("<title>Session Export</title>", () => `<title>${BRAND_DISPLAY_NAME} session</title>`)
	.replace("{{SESSION_DATA}}</script>", () => `</script>\n  <script>${loaderJs}</script>`);
if (viewer.includes("{{SESSION_DATA}}")) throw new Error("session-data placeholder survived substitution");
if (!viewer.includes("__OMP_SESSION_DATA__")) throw new Error("share loader not injected");
await Bun.write(path.join(publicDir, "share-viewer.html"), viewer);

// ── 3. dashboard SPA ────────────────────────────────────────────────────
console.log("building dashboard SPA ...");
const webSrcDir = path.join(pkgDir, "src/client");
const dashDir = path.join(publicDir, "dash");
const sourceCss = await Bun.file(path.join(webSrcDir, "styles.css")).text();
const candidates = await extractTailwindClasses(webSrcDir);
const compiler = await compile(sourceCss, { base: webSrcDir, onDependency: () => {} });
// 固定文件名 + immutable 缓存会让浏览器咬死旧 bundle；产物一律内容 hash 命名。
const cssOut = compiler.build([...candidates]);
const cssName = `styles-${Bun.hash(cssOut).toString(16).slice(0, 10)}.css`;
await Bun.write(path.join(dashDir, cssName), cssOut);

const dashResult = await Bun.build({
	entrypoints: [path.join(webSrcDir, "main.tsx")],
	outdir: dashDir,
	minify: true,
	naming: "[name]-[hash].[ext]",
});
if (!dashResult.success) {
	for (const message of dashResult.logs) console.error(message);
	process.exit(1);
}
const jsArtifact = dashResult.outputs.find(output => output.path.endsWith(".js"));
if (!jsArtifact) throw new Error("dashboard bundle produced no .js artifact");
const jsName = path.basename(jsArtifact.path);

const themeKey = `${BRAND_APP_NAME}-collab-dash-theme`;
const dashHtml = `<!DOCTYPE html>
<html lang="zh-CN">
<head>
	<meta charset="UTF-8">
	<meta name="viewport" content="width=device-width, initial-scale=1.0">
	<title>${BRAND_DISPLAY_NAME} collab 控制台</title>
	<script>
		(function () {
			try {
				var stored = localStorage.getItem(${JSON.stringify(themeKey)});
				var system = matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
				var theme = stored === "light" || stored === "dark" ? stored : system;
				document.documentElement.dataset.theme = theme;
				document.documentElement.style.colorScheme = theme;
			} catch (e) {}
		})();
	</script>
	<link rel="stylesheet" href="/dash/${cssName}">
</head>
<body>
	<div id="root"></div>
	<script src="/dash/${jsName}" type="module"></script>
</body>
</html>`;
await Bun.write(path.join(dashDir, "index.html"), dashHtml);

// ── 4. 服务端 bundle ─────────────────────────────────────────────────────
console.log("bundling server ...");
const serverResult = await Bun.build({
	entrypoints: [path.join(pkgDir, "src/index.ts")],
	outdir: outDir,
	target: "bun",
	naming: "server.[ext]",
	minify: false,
});
if (!serverResult.success) {
	for (const message of serverResult.logs) console.error(message);
	process.exit(1);
}

// ── 5. Linux x64 单文件二进制（可选，--compile-linux）──────────────────
// **自包含**：dist/public 打成 base64(gzip(JSON manifest)) 内嵌进二进制，
// 生成入口注册归档后调 main()，启动时释放到 tmp（src/embedded.ts）——
// 部署只需上传这一个文件。目标版本钉根 package.json 的 packageManager：
// 本机若跑 canary bun，裸 `bun-linux-x64` 会解析成 canary 版本号而无产物
// 可下载（实测报 "not available for download"）；COLLAB_COMPILE_TARGET 可覆盖。
if (Bun.argv.includes("--compile-linux")) {
	console.log("compiling self-contained linux x64 binary ...");
	const manifest: Record<string, string> = {};
	const collect = async (dir: string, prefix: string): Promise<void> => {
		for (const entry of await fs.readdir(dir, { withFileTypes: true })) {
			const full = path.join(dir, entry.name);
			const rel = prefix ? `${prefix}/${entry.name}` : entry.name;
			if (entry.isDirectory()) await collect(full, rel);
			else manifest[rel] = Buffer.from(await Bun.file(full).arrayBuffer()).toString("base64");
		}
	};
	await collect(publicDir, "");
	const archive = Buffer.from(Bun.gzipSync(new TextEncoder().encode(JSON.stringify(manifest)))).toString("base64");
	const embedDir = path.join(outDir, "embed");
	await Bun.write(path.join(embedDir, "public-archive.txt"), archive);
	await Bun.write(
		path.join(embedDir, "entry.ts"),
		[
			'import archive from "./public-archive.txt" with { type: "text" };',
			'import { setEmbeddedPublicArchive } from "../../src/embedded";',
			'import { main } from "../../src/index";',
			"setEmbeddedPublicArchive(archive);",
			"main();",
			"",
		].join("\n"),
	);

	const rootPkg = (await Bun.file(path.join(repoRoot, "package.json")).json()) as { packageManager?: string };
	const bunPin = rootPkg.packageManager?.match(/^bun@(\d+\.\d+\.\d+)$/)?.[1];
	const target = Bun.env.COLLAB_COMPILE_TARGET ?? (bunPin ? `bun-linux-x64-v${bunPin}` : "bun-linux-x64");
	const outfile = path.join(outDir, "collab-server");
	await $`bun build ${path.join(embedDir, "entry.ts")} --compile --target=${target} --outfile ${outfile}`.cwd(pkgDir);
	await fs.rm(embedDir, { recursive: true, force: true });
}

console.log(`build complete → ${outDir}`);
console.log(
	"deploy: 单文件 `dist/collab-server`（Linux x64，资产已内嵌）直接上传即可；或拷贝 dist/ 后 `bun dist/server.js`（TLS/wss 由反代终结）",
);
