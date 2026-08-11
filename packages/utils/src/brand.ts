import * as fs from "node:fs";
import * as path from "node:path";

/**
 * Fork 品牌单一值源（见 docs/fork/sync-strategy.md）。
 *
 * 上游不存在此文件 → rebase 永不冲突。所有品牌字面量只允许从这里取值；
 * 需要新增品牌常量时加在这里，不要在调用点硬编码。
 *
 * 纯常量（名称、色板等，无 node:* 依赖）住在 brand-consts.ts —— 浏览器 bundle
 * （如 stats 仪表盘客户端）只 import 那边；本文件全量再导出，服务端调用点不变。
 */

export * from "./brand-consts";

/** 配置目录名，相对 $HOME（上游 ".omp"）。 */
export const BRAND_CONFIG_DIR_NAME = ".zcode";

/** 内部文档 scheme（上游 "omp"，即 omp://）。旧 scheme 保留为隐藏别名。 */
export const BRAND_DOCS_SCHEME = "zcode";

/** 用户侧环境变量前缀（上游 "OMP_"）。旧前缀保留兼容；内部一律读 PI_*。 */
export const BRAND_ENV_PREFIX = "ZCODE_";

/** GitHub 仓库（发布/更新通道）。 */
export const BRAND_REPO = "zerx-lab/zcode";

/**
 * 发布 tag 前缀（上游 CI 只认 `v*`，独立前缀避免两套发布流互相触发）。
 * 完整 tag 形如 `zcode-v<上游版本>-z<迭代>`；brand/apply.ts 与 update 通道共用此真源。
 */
export const BRAND_RELEASE_TAG_PREFIX = "zcode-v";

/**
 * 本构建对应的发布迭代号（`zcode-v<版本>-z<N>` 里的 N）。
 * 打 tag 前必须与 tag 一致——`brand/check-release-tag.ts` 在发布流水线里强制校验，
 * 忘 bump 会直接 fail 掉 release。`zcode update` 用它与远端 tag 比较，
 * 以便同一上游版本内的 -zN 热修也能被检测到。
 */
export const BRAND_RELEASE_ITERATION = 3;

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
