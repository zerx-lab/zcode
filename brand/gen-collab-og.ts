#!/usr/bin/env bun
/**
 * 从 `brand/assets/collab-og.html` 生成 `packages/collab-web/public/og-image.png`
 * （collab-web 的 og:image / twitter:image）。
 *
 * 跑法：`bun brand/gen-collab-og.ts`。改了模板就重跑并提交 PNG —— 光栅化需要
 * Chromium，所以 PNG 是 checked-in 产物（同 `gen-hero.ts` 的取舍）。
 *
 * 复用 coding-agent 的 Chromium 解析（系统 Chrome → puppeteer 缓存 → 按需下载）。
 */
import * as path from "node:path";
import { ensureChromiumExecutable, loadPuppeteer } from "../packages/coding-agent/src/tools/browser/launch";

const CARD = { width: 1200, height: 630 } as const;
const template = path.join(import.meta.dir, "assets", "collab-og.html");
const out = path.join(import.meta.dir, "..", "packages", "collab-web", "public", "og-image.png");

const executablePath = await ensureChromiumExecutable();
if (!executablePath) throw new Error("no Chromium available; set PUPPETEER_EXECUTABLE_PATH");

const puppeteer = await loadPuppeteer();
const browser = await puppeteer.launch({
	headless: true,
	executablePath,
	defaultViewport: { ...CARD, deviceScaleFactor: 1 },
});
try {
	const page = await browser.newPage();
	await page.goto(`file://${template.replaceAll("\\", "/")}`, { waitUntil: "networkidle0" });
	const card = await page.$("#card");
	if (!card) throw new Error("collab-og.html has no #card element");
	await Bun.write(out, await card.screenshot({ type: "png" }));
	console.log(`wrote ${out} (${CARD.width}x${CARD.height})`);
} finally {
	await browser.close();
}
