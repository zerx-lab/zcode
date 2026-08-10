#!/usr/bin/env bun
/**
 * 从 `BRAND_RAMP`（`packages/utils/src/brand.ts`，全站唯一色值真源）生成品牌
 * SVG 字标。
 *
 * 跑法：`bun brand/gen-logo.ts`。改了色板就重跑一次；忘了跑会被
 * `packages/utils/test/brand-ramp.test.ts` 拦下来。
 *
 * mono 版不含品牌色（深底纯白，用于单色场景），因此不随色板变化。
 */
import * as path from "node:path";
import { BRAND_RAMP } from "../packages/utils/src/brand";

/** Z 字标轮廓，viewBox 120×90。终端块字符版在 `brand-logo.ts`，两者形状一致。 */
const MARK_PATH = "M10 8 H110 V22 L42 68 H110 V82 H10 V68 L78 22 H10 Z";
const OUT_DIR = path.join(import.meta.dir, "logo");

/** 首尾贴边、中间等距；两位小数并去掉前导 0，和手写 SVG 的书写习惯一致。 */
export function rampStops(ramp: readonly string[]): string[] {
	return ramp.map((hex, i) => {
		const raw = (i / (ramp.length - 1)).toFixed(2);
		const offset = raw === "0.00" ? "0" : raw === "1.00" ? "1" : raw.replace(/^0/, "").replace(/0$/, "");
		return `<stop offset="${offset}" stop-color="${hex}"/>`;
	});
}

export function renderMarkSvg(ramp: readonly string[]): string {
	const stops = rampStops(ramp)
		.map(stop => `      ${stop}`)
		.join("\n");
	return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 120 90" width="120" height="90">
  <defs>
    <linearGradient id="zg" x1="0" y1="1" x2="1" y2="0">
${stops}
    </linearGradient>
  </defs>
  <path d="${MARK_PATH}" fill="url(#zg)"/>
</svg>
`;
}

export function renderMonoSvg(): string {
	return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 120 90" width="120" height="90">
  <path d="${MARK_PATH}" fill="#fafafa"/>
</svg>
`;
}

if (import.meta.main) {
	await Bun.write(path.join(OUT_DIR, "zcode-mark.svg"), renderMarkSvg(BRAND_RAMP));
	await Bun.write(path.join(OUT_DIR, "zcode-mark-mono.svg"), renderMonoSvg());
	console.log(`generated zcode-mark.svg + zcode-mark-mono.svg from BRAND_RAMP (${BRAND_RAMP.join(" ")})`);
}
