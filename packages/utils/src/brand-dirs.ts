/**
 * Fork 自有的项目级配置目录探测助手（见 docs/fork/sync-strategy.md 与
 * .omp/rules/fork-low-conflict.md）。
 *
 * 上游不存在此文件 → rebase 永不冲突。读路径遍历候选表（native 优先、
 * compat 兜底）；有状态的项目配置文件读写共用 `resolveProjectConfigFile`
 * 粘性解析。原语（cwd 必传）在 `./brand`，这里补 `getProjectDir` 默认值。
 */
import * as path from "node:path";
import { BRAND_PROJECT_CONFIG_DIR_NAMES, resolveProjectConfigFileIn } from "./brand";
import { getProjectDir } from "./dirs";

export { findExistingProjectConfigDirName } from "./brand";

/** Project-local config dir candidates in probe order (native first, compat fallback). */
export function getProjectAgentDirCandidates(cwd: string = getProjectDir()): string[] {
	return BRAND_PROJECT_CONFIG_DIR_NAMES.map(dirName => path.join(cwd, dirName));
}

/** 粘性解析项目级配置文件；语义见 {@link resolveProjectConfigFileIn}。 */
export function resolveProjectConfigFile(filename: string, cwd: string = getProjectDir()): string {
	return resolveProjectConfigFileIn(cwd, filename);
}
