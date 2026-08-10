/**
 * Fork 品牌**纯常量**（无 node:* 依赖，浏览器 bundle 可直接 import）。
 *
 * 从 brand.ts 拆出的原因：stats 仪表盘等 web 客户端也要取品牌值，而 brand.ts
 * 顶层 import 了 node:fs/node:path（项目配置目录探测），不能进浏览器 bundle。
 * brand.ts `export * from "./brand-consts"` 全量再导出，服务端调用点不变；
 * 浏览器侧一律 import `@oh-my-pi/pi-utils/brand-consts`。
 *
 * 上游不存在此文件 → rebase 永不冲突。所有品牌字面量只允许从这里/brand.ts 取值。
 */

/** CLI / 二进制名（上游 "omp"）。 */
export const BRAND_APP_NAME = "zcode";

/** 用户可见产品名（上游 "Oh My Pi" / "Oh-My-Pi"）。 */
export const BRAND_DISPLAY_NAME = "zcode";

/** 状态栏 / 终端标题使用的紧凑品牌字标（上游 "π"）。 */
export const BRAND_MARK = "Z";

/**
 * 品牌色板：**全站唯一色值真源**。单色相青蓝（hue ≈ 200°），只走明度/饱和度变化。
 *
 * 消费方全部从这里派生，没有第二份副本：
 * - 终端 logo / setup splash 水面 —— `brand-logo.ts` 的 `BRAND_GRADIENT_STOPS`（RGB 派生）；
 * - OAuth 回调页字标与页面配色 —— `packages/ai/src/registry/oauth/oauth-brand.ts`；
 * - stats 仪表盘图表分类色 —— `packages/stats/src/client/components/chart-shared.tsx`；
 * - `brand/logo/zcode-mark.svg` —— 由 `bun brand/gen-logo.ts` 生成，
 *   `packages/utils/test/brand-ramp.test.ts` 守住「改了色板忘了重跑脚本」。
 *
 * 档数保持 5：`gradientEscape` 按 `t * (stops.length - 1)` 分段插值，档数变化会
 * 改变 splash 水面与 logo 的色带节奏。
 */
export const BRAND_RAMP: readonly string[] = [
	"#1a68a6", // deep azure
	"#2396d6",
	"#3dc7ff", // brand blue
	"#7ad9ff",
	"#b0ebff", // ice
];

/** 品牌主色：状态栏 accent、wordmark 等需要单一颜色时取这一档。 */
export const BRAND_COLOR = BRAND_RAMP[2];

/** {@link BRAND_RAMP} 的 RGB 派生（模块加载时解析一次，避免第二份手写副本）。 */
export const BRAND_RAMP_RGB: ReadonlyArray<readonly [number, number, number]> = BRAND_RAMP.map(hex => [
	Number.parseInt(hex.slice(1, 3), 16),
	Number.parseInt(hex.slice(3, 5), 16),
	Number.parseInt(hex.slice(5, 7), 16),
]);

/** 256-color 兜底梯度（终端无 truecolor 时），同样是从深到浅的蓝。 */
export const BRAND_RAMP_256: readonly number[] = [24, 25, 31, 39, 45, 81, 153];
