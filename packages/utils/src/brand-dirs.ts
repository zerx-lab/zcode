/**
 * Fork 自有的项目级配置目录探测助手（见 docs/fork/sync-strategy.md 与
 * .omp/rules/fork-low-conflict.md）。
 *
 * 上游不存在此文件 → rebase 永不冲突。读路径遍历候选表（native 优先、
 * compat 兜底）；有状态的项目配置文件读写共用 `resolveProjectConfigFile`
 * 粘性解析。原语（cwd 必传）在 `./brand`，这里补 `getProjectDir` 默认值。
 */
import * as path from "node:path";
import {
	BRAND_COMPAT_USER_CONFIG_DIRS,
	BRAND_CONFIG_DIR_NAME,
	BRAND_PROJECT_CONFIG_DIR_NAMES,
	resolveProjectConfigFileIn,
} from "./brand";
import { getAgentDir, getConfigRootDir, getProjectDir } from "./dirs";

export { findExistingProjectConfigDirName } from "./brand";

/** Project-local config dir candidates in probe order (native first, compat fallback). */
export function getProjectAgentDirCandidates(cwd: string = getProjectDir()): string[] {
	return BRAND_PROJECT_CONFIG_DIR_NAMES.map(dirName => path.join(cwd, dirName));
}

/** 粘性解析项目级配置文件；语义见 {@link resolveProjectConfigFileIn}。 */
export function resolveProjectConfigFile(filename: string, cwd: string = getProjectDir()): string {
	return resolveProjectConfigFileIn(cwd, filename);
}

/**
 * 用户级 agent 目录候选，探测顺序 native 优先、compat 兜底。
 *
 * 与项目级同语义：调用点遍历**全表并合并**（每个存在的候选都参与发现），不是
 * "native 存在就不看 compat" —— 那样 `~/.zcode/agent` 一旦有任何内容，用户留在
 * `~/.omp/agent/rules` 的规则就会静默消失。同名条目由 capability 层按 name 去重，
 * native 胜出。
 *
 * 只在**默认布局**下镜像：`agentDir === <configRoot>/agent`，且 configRoot 形如
 * `<home>/<品牌目录>[/profiles/<name>]`。判据是精确结构而不是"路径长得像"——
 * `PI_CODING_AGENT_DIR=$HOME/.zcode/custom` 仍落在 configRoot 之下，按形状猜就会凭空
 * 镜像出 `$HOME/.omp/custom`、加载用户从没指定过的配置，所以显式覆盖只返回 native。
 *
 * XDG 不影响这里：`dirs.ts` 的 `configRoot`/`agentDir` 始终是 `<home>/<品牌目录>`，
 * XDG 只重定向 data/state/cache 分类子目录（sessions、db、缓存）。声明式配置
 * （`rules/`、`RULES.md`、`AGENTS.md`、`SYSTEM.md`）本来就直接挂在 agentDir 下，
 * 上游 omp 在同布局下也是如此，所以 `<home>/.omp/agent` 正是它们的兼容位置。
 */
export function getUserAgentDirCandidates(): string[] {
	const native = getAgentDir();
	const configRoot = getConfigRootDir();
	if (native !== path.join(configRoot, "agent")) return [native];

	// 品牌段就地替换，而不是拿 `os.homedir()` 去 relative：dirs.ts 在模块加载时把 home
	// 锚死（`RESOLVER_HOME`，注释写明"stay stable across test mocks of os.homedir()"），
	// 任何 mock 过 homedir 的调用方都会让两者不一致、compat 候选静默消失。configRoot
	// 形如 `<home>/.zcode` 或 `<home>/.zcode/profiles/<name>`——profile 尾巴要原样带上，
	// 否则命名 profile 会去读另一个 profile 的配置。
	const marker = path.sep + BRAND_CONFIG_DIR_NAME;
	const brandAt = configRoot.lastIndexOf(marker);
	if (brandAt < 0) return [native];
	const head = configRoot.slice(0, brandAt);
	const tail = configRoot.slice(brandAt + marker.length);
	// 命中必须是完整路径段，不能是 `.zcode-backup` 这种前缀撞车。
	if (tail !== "" && !tail.startsWith(path.sep)) return [native];

	return [
		native,
		...BRAND_COMPAT_USER_CONFIG_DIRS.map(dirName => path.join(head + path.sep + dirName + tail, "agent")),
	];
}
