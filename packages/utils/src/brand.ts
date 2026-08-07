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
