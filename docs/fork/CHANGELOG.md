# Fork 变更记录

上游 `packages/*/CHANGELOG.md` 属上游所有，fork 侧改动只记在这里。

## [Unreleased]

### Added

- zcode 品牌字标：终端块字符版 `packages/coding-agent/src/modes/components/brand-logo.ts`（`ZCODE_LOGO`，12×5，仅用 `█ ▀ ▄` 以保证 setup splash 2x 放大不发虚）；SVG 版 `brand/logo/zcode-mark.svg`（品牌渐变）与 `brand/logo/zcode-mark-mono.svg`（深底单色）。

### Changed

- `welcome.ts` 的 `PI_LOGO` 改为从 `brand-logo.ts` 取值（1 行 import + 1 行赋值）。名字保持 `PI_LOGO`，welcome 盒 / setup splash / outro / wizard header 四个消费点零改动。
