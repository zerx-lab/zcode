/**
 * Fork 品牌层：把上游 `oauth.html` 回调页的品牌面（文档标题、`.brand` 头部字标、
 * 页面的品牌配色）换成 zcode 的（见 docs/fork/sync-strategy.md）。
 *
 * 上游不存在此文件 → rebase 永不冲突；`callback-server.ts` 只接线 2 行。
 * 替换按**结构**匹配（`<title>` 元素、`.brand` 容器、`</head>`）而不是按上游文案
 * 字面量，上游重排页面内容时仍能命中。真的漂移到匹配不上时只 warn 并原样返回：
 * 登录流程本身与品牌无关，绝不能因为换字标而挂掉回调页。
 */
import { logger } from "@oh-my-pi/pi-utils";
import { BRAND_COLOR, BRAND_DISPLAY_NAME, BRAND_RAMP } from "@oh-my-pi/pi-utils/brand";

const TITLE_ELEMENT = /<title>[\s\S]*?<\/title>/;
/** `.brand` 头部整块（内部只有 svg + span，无嵌套 div）。 */
const BRAND_BLOCK = /<div class="brand">[\s\S]*?<\/div>/;
const HEAD_CLOSE = /<\/head>/;

/** 把 {@link BRAND_RAMP} 铺成 SVG 渐变 stop，首尾贴边、中间等距。 */
const GRADIENT_STOPS_SVG = BRAND_RAMP.map((hex, i) => {
	const offset = (i / (BRAND_RAMP.length - 1)).toFixed(2).replace(/^0/, "").replace(/\.00$/, "");
	return `<stop offset="${offset || "0"}" stop-color="${hex}" />`;
}).join("\n\t\t\t\t\t\t\t");

/**
 * zcode Z 字标，路径与 `brand/logo/zcode-mark.svg` 同源，配色由 {@link BRAND_RAMP}
 * 派生——和终端 logo 是同一份色值，不存副本。内联 `style` 覆写上游
 * `.brand svg { width: 22px; height: 22px }` 的方形约束——Z 是 4:3 的。
 */
const BRAND_BLOCK_HTML = `<div class="brand">
				<svg viewBox="0 0 120 90" aria-hidden="true" style="width: 28px; height: 21px">
					<defs>
						<linearGradient id="zcode-grad" x1="0" y1="1" x2="1" y2="0">
							${GRADIENT_STOPS_SVG}
						</linearGradient>
					</defs>
					<path fill="url(#zcode-grad)" d="M10 8 H110 V22 L42 68 H110 V82 H10 V68 L78 22 H10 Z" />
				</svg>
				<span class="wordmark">${BRAND_DISPLAY_NAME}</span>
			</div>`;

/**
 * 页面配色覆盖，追加在上游 `<style>` 之后（同特异性，后来者胜），因此上游那份
 * 样式表一行都不用动。改掉的只有品牌色：`--magenta`/`--iris` 的粉紫收敛到品牌蓝，
 * body 的两团径向光晕跟着变蓝（上游把颜色硬编码在 `background` 里、不走变量，
 * 只能整条重写；`RRGGBBAA` 八位 hex 承载透明度），wordmark 从中性白改成品牌蓝。
 *
 * 中性色、`--success`/`--error` 信号色不碰——成功/失败态必须仍然一眼可辨。
 */
const BRAND_STYLE_HTML = `<style>
			:root {
				--magenta: ${BRAND_RAMP[0]};
				--iris: ${BRAND_RAMP[1]};
				--cyan: ${BRAND_COLOR};
			}
			body {
				background:
					radial-gradient(ellipse 80vw 60vh at 18% -10%, ${BRAND_RAMP[1]}2e, transparent 60%),
					radial-gradient(ellipse 70vw 70vh at 110% 30%, ${BRAND_COLOR}17, transparent 60%),
					var(--bg);
			}
			.brand .wordmark {
				color: ${BRAND_COLOR};
			}
		</style>
	</head>`;

function replaceOnce(html: string, pattern: RegExp, replacement: string, what: string): string {
	if (!pattern.test(html)) {
		logger.warn("OAuth callback page brand anchor missing; upstream template drifted", { anchor: what });
		return html;
	}
	return html.replace(pattern, () => replacement);
}

/** 给上游回调页模板套上 zcode 品牌；输入输出都是完整 HTML 文本。 */
export function brandOAuthPage(html: string): string {
	let out = replaceOnce(html, TITLE_ELEMENT, `<title>${BRAND_DISPLAY_NAME} · authentication</title>`, "<title>");
	out = replaceOnce(out, BRAND_BLOCK, BRAND_BLOCK_HTML, ".brand");
	return replaceOnce(out, HEAD_CLOSE, BRAND_STYLE_HTML, "</head>");
}
