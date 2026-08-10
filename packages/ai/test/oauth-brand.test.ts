import { afterEach, describe, expect, it, spyOn } from "bun:test";
import { brandOAuthPage } from "@oh-my-pi/pi-ai/registry/oauth/oauth-brand";
import { logger } from "@oh-my-pi/pi-utils";
import { BRAND_COLOR, BRAND_DISPLAY_NAME, BRAND_RAMP } from "@oh-my-pi/pi-utils/brand";
import upstreamTemplate from "../src/registry/oauth/oauth.html" with { type: "text" };

/**
 * Fork 品牌层的漂移哨兵：`brandOAuthPage` 按结构（`<title>`、`.brand`）而不是按
 * 上游文案匹配，所以上游改回调页文案时它仍应命中。这里断言的是**函数在真实上游
 * 模板上的输出**：品牌换掉了、注入契约没坏、锚点丢失时降级而不是抛。
 */
describe("brandOAuthPage", () => {
	afterEach(() => {
		spyOn(logger, "warn").mockRestore();
	});

	const branded = brandOAuthPage(upstreamTemplate as unknown as string);

	it("replaces the upstream wordmark and pi mark with the fork brand", () => {
		expect(branded).toContain(`<title>${BRAND_DISPLAY_NAME} · authentication</title>`);
		expect(branded).toContain(`<span class="wordmark">${BRAND_DISPLAY_NAME}</span>`);
		expect(branded).toContain('fill="url(#zcode-grad)"');
		// 字标配色由 BRAND_RAMP 派生，不存副本：改色板这里必须自动跟随。
		for (const hex of BRAND_RAMP) expect(branded).toContain(`stop-color="${hex}"`);
		// 上游品牌面必须彻底消失，否则回调页会同时出现两套品牌。
		expect(branded).not.toMatch(/oh my pi/i);
		expect(branded).not.toContain('pi-grad"');
	});

	it("recolors the page chrome from the ramp, after the upstream stylesheet", () => {
		// 覆盖样式必须排在上游 <style> 之后，否则同特异性的 body 背景不会生效。
		const upstreamStyleEnd = branded.indexOf("</style>");
		expect(branded.indexOf(`--cyan: ${BRAND_COLOR}`)).toBeGreaterThan(upstreamStyleEnd);
		// 上游硬编码在 body background 里的品红光晕不走变量，只能整条重写——上游那行
		// 文本仍在（我们不动上游样式表），赢的是排在它后面的这条。
		expect(branded.indexOf(`${BRAND_RAMP[1]}2e`)).toBeGreaterThan(branded.indexOf("oklch(0.7 0.24 340 / 0.13)"));
		// 信号色不许被品牌覆盖顺手改掉：成功/失败态必须仍然一眼可辨。
		expect(branded).toContain("--success: oklch(0.78 0.16 150)");
		expect(branded).toContain("--error: oklch(0.66 0.22 25)");
	});

	it("ships the close fallback that overrides the upstream dead close button", () => {
		// 浏览器拒绝关闭非 script-opened 的 tab，上游 onclick 因此无声失效。
		// 我们必须**接管** handler（属性赋值覆盖内联属性），而不是并排再挂一个
		// addEventListener——那样上游那次失败的 close 仍会先跑。
		expect(branded).toContain("btn.onclick = tryClose");
		// 注入点在 </head> 之前；DOMContentLoaded 保证它排在上游 body 末尾的
		// 内联脚本之后，从而能读到已写好的 success/error 态。
		expect(branded.indexOf("DOMContentLoaded")).toBeLessThan(branded.indexOf("</head>"));
	});

	it("preserves the __OAUTH_STATE__ injection contract the callback server depends on", () => {
		// callback-server.ts 用 replaceAll 注入 JSON 状态；占位符没了页面就永远停在
		// "Authentication" 空白态。
		expect(branded).toContain("__OAUTH_STATE__");
		const rendered = branded.replaceAll("__OAUTH_STATE__", JSON.stringify({ ok: true, code: "c", state: "s" }));
		expect(rendered).not.toContain("__OAUTH_STATE__");
		expect(rendered).toContain('{"ok":true,"code":"c","state":"s"}');
	});

	it("degrades to the untouched page and warns when an anchor is gone", () => {
		const warn = spyOn(logger, "warn").mockImplementation(() => {});
		const drifted = "<html><body><p>redesigned upstream page</p></body></html>";

		expect(brandOAuthPage(drifted)).toBe(drifted);
		expect(warn).toHaveBeenCalledTimes(3);
		expect(warn.mock.calls.map(([, meta]) => (meta as { anchor: string }).anchor)).toEqual([
			"<title>",
			".brand",
			"</head>",
		]);
	});
});
