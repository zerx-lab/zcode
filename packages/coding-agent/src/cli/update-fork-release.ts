/**
 * fork 发布通道解析（fork 新文件，零 rebase 冲突面）。
 *
 * zcode 不发 npm（包名与上游撞名），唯一分发通道是 GitHub Releases，
 * tag 形如 `zcode-v<上游版本>-z<迭代>`（前缀真源 BRAND_RELEASE_TAG_PREFIX）。
 * 上游 update-cli 的「npm 查版本 + `v<版本>` tag 找资产」在 fork 上两环全断：
 * npm latest 是上游 oh-my-pi 的版本，`v*` tag 在 fork 仓库根本不存在。
 *
 * 本模块提供 fork 侧的版本发现与比较；update-cli.ts 只留最小接线
 * （见 docs/fork/sync-strategy.md 的冲突面表）。
 */
import { $env, compareVersions, VERSION } from "@oh-my-pi/pi-utils";
import { BRAND_RELEASE_ITERATION, BRAND_RELEASE_TAG_PREFIX, BRAND_REPO } from "@oh-my-pi/pi-utils/brand";
import { isTimeoutError, withTimeoutSignal } from "../utils/fetch-timeout";

const GITHUB_API = "https://api.github.com";
const RELEASE_METADATA_TIMEOUT_MS = 30_000;

/** 一次 fork 发布：完整 tag + 拆出来的 (上游基版本, 迭代号)。 */
export interface ForkRelease {
	tag: string;
	version: string;
	iteration: number;
}

type Fetch = (input: string | URL | Request, init?: RequestInit) => Promise<Response>;

/** 拼 `zcode-v<version>-z<iteration>`。 */
export function formatForkReleaseTag(version: string, iteration: number): string {
	return `${BRAND_RELEASE_TAG_PREFIX}${version}-z${iteration}`;
}

/**
 * 解析发布 tag；不带 `-z<N>` 后缀的 tag 按迭代 0 处理（容错，正常发布都带）。
 * 非本 fork 前缀或版本段不合法 → undefined。
 */
export function parseForkReleaseTag(tag: string): ForkRelease | undefined {
	if (!tag.startsWith(BRAND_RELEASE_TAG_PREFIX)) return undefined;
	const rest = tag.slice(BRAND_RELEASE_TAG_PREFIX.length);
	const match = /^(\d+\.\d+\.\d+)(?:-z(\d+))?$/.exec(rest);
	if (!match) return undefined;
	return {
		tag,
		version: match[1],
		iteration: match[2] !== undefined ? Number.parseInt(match[2], 10) : 0,
	};
}

/** 当前构建自身对应的发布（版本来自 package.json，迭代号来自 brand 常量）。 */
export function localForkRelease(): ForkRelease {
	return {
		tag: formatForkReleaseTag(VERSION, BRAND_RELEASE_ITERATION),
		version: VERSION,
		iteration: BRAND_RELEASE_ITERATION,
	};
}

/** 先比上游基版本，同版本再比 -zN 迭代号。 */
export function compareForkReleases(a: ForkRelease, b: ForkRelease): number {
	return compareVersions(a.version, b.version) || a.iteration - b.iteration;
}

/**
 * 取 fork 仓库最新 release（`GET /releases/latest`，只返回非 draft / 非 prerelease）。
 * tag 解析失败视为发布通道异常，直接报错而不是静默当作「已最新」。
 */
export async function getLatestForkRelease(
	fetchImpl: Fetch = fetch,
	githubToken: string | undefined = $env.GITHUB_TOKEN || $env.GH_TOKEN,
): Promise<ForkRelease> {
	const headers: Record<string, string> = {
		Accept: "application/vnd.github+json",
		"X-GitHub-Api-Version": "2022-11-28",
	};
	if (githubToken) headers.Authorization = `Bearer ${githubToken}`;

	let response: Response;
	try {
		response = await fetchImpl(`${GITHUB_API}/repos/${BRAND_REPO}/releases/latest`, {
			headers,
			signal: withTimeoutSignal(RELEASE_METADATA_TIMEOUT_MS),
		});
	} catch (err) {
		if (isTimeoutError(err)) {
			throw new Error("Timed out fetching release info after 30s", { cause: err });
		}
		throw err;
	}
	if ((response.status === 403 && !githubToken) || response.status === 429) {
		throw new Error(
			"GitHub API rate limit exceeded while fetching release metadata; retry later or set GITHUB_TOKEN or GH_TOKEN",
		);
	}
	if (!response.ok) {
		throw new Error(`Failed to fetch release info: ${response.statusText}`);
	}

	const data = (await response.json()) as { tag_name?: unknown };
	if (typeof data.tag_name !== "string") {
		throw new Error("Invalid GitHub release metadata: missing tag_name");
	}
	const release = parseForkReleaseTag(data.tag_name);
	if (!release) {
		throw new Error(
			`Latest GitHub release tag "${data.tag_name}" does not match ${BRAND_RELEASE_TAG_PREFIX}<version>-z<n>`,
		);
	}
	return release;
}

/**
 * fork 只有「GitHub Release 二进制」一条更新通道：不发 npm（bun/npm 会装成上游
 * oh-my-pi），无 Homebrew tap，mise 的 github 后端也认不出 `zcode-v*` tag。
 * 非 binary 方法一律拒绝并指路重装，绝不能放行到上游的包管理器分支。
 */
export function ensureForkBinaryTarget(target: { method: string }): void {
	if (target.method === "binary") return;
	throw new Error(
		`"${target.method}" installs are not supported: zcode ships only GitHub release binaries. ` +
			`Reinstall via the install script (https://github.com/${BRAND_REPO}#readme)`,
	);
}
