import { BRAND_RAMP_256, BRAND_RAMP_RGB } from "@oh-my-pi/pi-utils/brand";

/**
 * Fork 品牌终端字标（见 docs/fork/sync-strategy.md）。
 *
 * 上游不存在此文件 → rebase 永不冲突。`welcome.ts` 只保留一行 re-export，
 * 四个消费点（welcome 盒 / setup splash / outro / wizard header）零改动。
 *
 * 约束（改动前必读）：
 * - 每行等宽、5 行。`gradientLogo` 按 `x + (rows-1-y)` 算对角渐变，行宽不齐会让
 *   渐变错位；空格视为透明、不着色。
 * - setup splash 的 `LARGE_LOGO` 把每个非空格字符横向复制、每行纵向复制。只用
 *   全块 `█ ▀ ▄`：半格/四分块（`▐ ▌ ▗ ▛`）加倍后会变成 `▐▐`、`▗▗` 这类发虚边缘。
 * - 宽度贴着上游的 12：welcome 是两栏布局，更宽会挤压右栏。
 */
export const ZCODE_LOGO = ["████████████", "       ▄███▀", "    ▄███▀   ", " ▄███▀      ", "████████████"];

/**
 * 上游 `welcome.ts` 的 `GRADIENT_STOPS` / `GRADIENT_RAMP_256` 从这两个名字取值，
 * 于是「五档粉紫→薄荷」的上游调色板收缩成两行接线。四个消费点（welcome 盒 /
 * setup splash / outro / wizard header）与 `gradientEscape` 的插值、shine 高光
 * 逻辑零改动——变的只有色相。
 *
 * 色值本身不在这里：真源是 `@oh-my-pi/pi-utils/brand` 的 `BRAND_RAMP`。
 */
export const BRAND_GRADIENT_STOPS = BRAND_RAMP_RGB;
export const BRAND_GRADIENT_RAMP_256 = BRAND_RAMP_256;
