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
 * 修上游的死按钮：`window.close()` 只能关掉**脚本自己 `window.open` 的**窗口。
 * OAuth 回调 tab 是授权站点 302 过来的顶层导航，浏览器一律拒绝关闭请求——上游那个
 * `onclick="window.close()"` 和 3 秒自动关闭因此都是无声失效的（既不抛异常也不
 * 提示，用户只看到点了没反应）。
 *
 * 关不掉是浏览器的硬约束，改不了；能修的是「无声」：先照常尝试关闭（tab 真的是
 * 脚本打开的场景仍然会关掉），200ms 后页面还活着就说明被拒，把文案与按钮切成
 * 明确的手动关闭指引。
 *
 * 脚本放在 `<head>`，靠 `DOMContentLoaded` 排到上游 body 末尾那段内联脚本之后
 * 执行，因此能读到它写好的 `success`/`error` 态，并用 `onclick` 属性赋值覆盖掉
 * 上游的内联 handler。
 */
const CLOSE_FALLBACK_SCRIPT = `<script>
			window.addEventListener("DOMContentLoaded", () => {
				const btn = document.querySelector(".btn");
				const message = document.getElementById("message");
				const combo = /mac/i.test(navigator.platform || navigator.userAgent) ? "\\u2318W" : "Ctrl+W";
				let hinted = false;
				const hint = () => {
					if (hinted) return;
					hinted = true;
					if (message) message.textContent = "Press " + combo + " to close this tab.";
					if (btn) {
						btn.disabled = true;
						btn.textContent = combo;
					}
				};
				const tryClose = () => {
					window.close();
					// 关成功的话这个回调根本不会跑——页面已经没了。
					setTimeout(hint, 200);
				};
				if (btn) btn.onclick = tryClose;
				// 上游在成功态 3s 时自己试了一次（同样会被拒）。排在它后面补一次，
				// 于是用户不点按钮也能看到该怎么关。
				if (document.getElementById("app")?.classList.contains("success")) setTimeout(tryClose, 3200);
			});
		</script>`;

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
			.btn[disabled] {
				opacity: 0.55;
				cursor: default;
			}
		</style>
		${CLOSE_FALLBACK_SCRIPT}
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
