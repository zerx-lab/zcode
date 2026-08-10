import * as fs from "node:fs";
import * as path from "node:path";

/**
 * Fork 品牌单一值源（见 docs/fork/sync-strategy.md）。
 *
 * 上游不存在此文件 → rebase 永不冲突。所有品牌字面量只允许从这里取值；
 * 需要新增品牌常量时加在这里，不要在调用点硬编码。
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

/** 配置目录名，相对 $HOME（上游 ".omp"）。 */
export const BRAND_CONFIG_DIR_NAME = ".zcode";

/** 内部文档 scheme（上游 "omp"，即 omp://）。旧 scheme 保留为隐藏别名。 */
export const BRAND_DOCS_SCHEME = "zcode";

/** 用户侧环境变量前缀（上游 "OMP_"）。旧前缀保留兼容；内部一律读 PI_*。 */
export const BRAND_ENV_PREFIX = "ZCODE_";

/** GitHub 仓库（发布/更新通道）。 */
export const BRAND_REPO = "zerx-lab/oh-my-pi";

/**
 * 项目级配置兼容目录（只读发现，不作为写入目标）。
 * zcode 同时发现 `.zcode/` 与这里列出的目录；上游 tracked 的 `.omp/{commands,skills}`
 * 及用户已有的 `.omp/` 项目配置借此免迁移、免镜像地继续生效。
 */
export const BRAND_COMPAT_PROJECT_CONFIG_DIRS: readonly string[] = [".omp"];

/** 项目级配置目录探测顺序：原生目录优先，compat 目录兜底（读走全表，写只走首项）。 */
export const BRAND_PROJECT_CONFIG_DIR_NAMES: readonly string[] = [
	BRAND_CONFIG_DIR_NAME,
	...BRAND_COMPAT_PROJECT_CONFIG_DIRS,
];

/**
 * 用户级配置兼容目录（只读发现，不作为写入目标），相对 `$HOME`。
 * 用户从上游 omp 切到 zcode 时，`~/.omp/agent/{rules,skills,commands,agents,…}` 借此
 * 继续生效、免迁移。写路径仍然只落 native `.zcode`——sessions / settings / auth 两个
 * 品牌各自一份，绝不能混写。
 */
export const BRAND_COMPAT_USER_CONFIG_DIRS: readonly string[] = [".omp"];

/**
 * 粘性解析项目级配置文件（cwd 必传，供 dirs.ts 使用，避免模块循环）：
 * 返回首个已存在的候选（native 优先，compat 兜底），都不存在时返回 native 路径。
 * 读写共用同一解析，避免"定义在 .omp、状态写进 .zcode"的数据分家。
 */
export function resolveProjectConfigFileIn(cwd: string, filename: string): string {
	const candidates = BRAND_PROJECT_CONFIG_DIR_NAMES.map(dirName => path.join(cwd, dirName, filename));
	return candidates.find(candidate => fs.existsSync(candidate)) ?? candidates[0];
}

/** 在 `dir` 下探测首个已存在的项目配置目录名（.zcode 优先，.omp 兜底）；无则 null。 */
export function findExistingProjectConfigDirName(dir: string): string | null {
	for (const dirName of BRAND_PROJECT_CONFIG_DIR_NAMES) {
		try {
			if (fs.statSync(path.join(dir, dirName)).isDirectory()) return dirName;
		} catch {
			// keep probing
		}
	}
	return null;
}
