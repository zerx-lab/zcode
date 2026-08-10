import { describe, expect, it } from "bun:test";
import { BRAND_COLOR, BRAND_RAMP, BRAND_RAMP_RGB } from "@oh-my-pi/pi-utils/brand";
import { renderMarkSvg, renderMonoSvg } from "../../../brand/gen-logo";

/**
 * `BRAND_RAMP` 是全站唯一色值真源。终端 logo 与 OAuth 回调页在运行时从它派生，
 * 只有 `brand/logo/*.svg` 是落盘产物——这里守住那道缝：改了色板却忘了重跑
 * `bun brand/gen-logo.ts`，SVG 就会停在旧色上，而没人会立刻看见。
 */
describe("BRAND_RAMP", () => {
	it("derives RGB stops from the hex ramp without a hand-written copy", () => {
		expect(BRAND_RAMP_RGB).toHaveLength(BRAND_RAMP.length);
		expect(BRAND_RAMP_RGB[0]).toEqual([0x1a, 0x68, 0xa6]);
		expect(BRAND_RAMP_RGB.at(-1)).toEqual([0xb0, 0xeb, 0xff]);
		expect(BRAND_COLOR).toBe(BRAND_RAMP[2]);
	});

	it("stays a single blue hue so the mark never reads as a two-color gradient", () => {
		const hues = BRAND_RAMP_RGB.map(([r, g, b]) => {
			const max = Math.max(r, g, b);
			const min = Math.min(r, g, b);
			// 品牌色恒以蓝为最大分量；真退化成灰（delta 0）时下面的区间断言会失败。
			return (60 * (4 + (r - g) / (max - min)) + 360) % 360;
		});
		for (const hue of hues) expect(hue).toBeGreaterThan(185);
		for (const hue of hues) expect(hue).toBeLessThan(215);
	});

	it("keeps the shipped SVG assets in sync with the ramp", async () => {
		const dir = new URL("../../../brand/logo/", import.meta.url);
		expect(await Bun.file(new URL("zcode-mark.svg", dir)).text()).toBe(renderMarkSvg(BRAND_RAMP));
		expect(await Bun.file(new URL("zcode-mark-mono.svg", dir)).text()).toBe(renderMonoSvg());
	});
});
