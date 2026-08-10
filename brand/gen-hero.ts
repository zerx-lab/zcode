#!/usr/bin/env bun
/**
 * 从 `brand/assets/banner.html` 生成 `brand/assets/hero.png`（README 首图）。
 *
 * 跑法：`bun brand/gen-hero.ts`。改了 banner 或 `BRAND_RAMP` 就重跑一次并提交
 * PNG —— 光栅化需要 Chromium，不能放进 `brand/apply.ts` 的同步路径，所以 PNG 是
 * checked-in 产物，`apply.ts` 只负责把它拷到 `assets/hero.png`。
 *
 * 复用 coding-agent 的 Chromium 解析（系统 Chrome → puppeteer 缓存 → 按需下载），
 * 不自己写第二套浏览器发现逻辑。
 */
import * as fs from "node:fs/promises";
import * as os from "node:os";
import * as path from "node:path";
import { ensureChromiumExecutable, loadPuppeteer } from "../packages/coding-agent/src/tools/browser/launch";
import { render } from "./apply";

const brandDir = import.meta.dir;
const CARD = { width: 1200, height: 630 } as const;

const executablePath = await ensureChromiumExecutable();
if (!executablePath) throw new Error("no Chromium available; set PUPPETEER_EXECUTABLE_PATH");

// 截图页必须和 icon.svg 同目录（banner 用 <img src="./icon.svg"> 引真源字标）。
const stageDir = await fs.mkdtemp(path.join(os.tmpdir(), "zcode-hero-"));
try {
	await Bun.write(
		path.join(stageDir, "banner.html"),
		render(await Bun.file(path.join(brandDir, "assets", "banner.html")).text()),
	);
	await Bun.write(path.join(stageDir, "icon.svg"), Bun.file(path.join(brandDir, "logo", "zcode-mark.svg")));

	const puppeteer = await loadPuppeteer();
	const browser = await puppeteer.launch({
		headless: true,
		executablePath,
		defaultViewport: { ...CARD, deviceScaleFactor: 2 },
	});
	try {
		const page = await browser.newPage();
		await page.goto(`file://${path.join(stageDir, "banner.html").replaceAll("\\", "/")}`, {
			waitUntil: "networkidle0",
		});
		const card = await page.$("#card");
		if (!card) throw new Error("banner.html has no #card element");
		const out = path.join(brandDir, "assets", "hero.png");
		await Bun.write(out, await card.screenshot({ type: "png" }));
		console.log(`wrote ${out} (${CARD.width}x${CARD.height} @2x)`);
	} finally {
		await browser.close();
	}
} finally {
	await fs.rm(stageDir, { recursive: true, force: true });
}
