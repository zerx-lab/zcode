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
