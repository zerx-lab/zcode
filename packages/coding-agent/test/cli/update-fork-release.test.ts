import { describe, expect, it } from "bun:test";
import { APP_NAME, VERSION } from "@oh-my-pi/pi-utils";
import { BRAND_RELEASE_ITERATION, BRAND_RELEASE_TAG_PREFIX, BRAND_REPO } from "@oh-my-pi/pi-utils/brand";
import { updateViaBinaryAt } from "../../src/cli/update-cli";
import {
	compareForkReleases,
	ensureForkBinaryTarget,
	formatForkReleaseTag,
	getLatestForkRelease,
	localForkRelease,
	parseForkReleaseTag,
} from "../../src/cli/update-fork-release";

describe("fork release tag parsing", () => {
	it("round-trips zcode-v<version>-z<n> tags", () => {
		const tag = formatForkReleaseTag("17.2.12", 2);
		expect(tag).toBe(`${BRAND_RELEASE_TAG_PREFIX}17.2.12-z2`);
		expect(parseForkReleaseTag(tag)).toEqual({ tag, version: "17.2.12", iteration: 2 });
	});

	it("treats a tag without -z suffix as iteration 0", () => {
		expect(parseForkReleaseTag(`${BRAND_RELEASE_TAG_PREFIX}17.2.12`)).toEqual({
			tag: `${BRAND_RELEASE_TAG_PREFIX}17.2.12`,
			version: "17.2.12",
			iteration: 0,
		});
	});

	it("rejects upstream v* tags and malformed version segments", () => {
		expect(parseForkReleaseTag("v17.2.12")).toBeUndefined();
		expect(parseForkReleaseTag(`${BRAND_RELEASE_TAG_PREFIX}17.2`)).toBeUndefined();
		expect(parseForkReleaseTag(`${BRAND_RELEASE_TAG_PREFIX}17.2.12-z`)).toBeUndefined();
		expect(parseForkReleaseTag(`${BRAND_RELEASE_TAG_PREFIX}17.2.12-rc1`)).toBeUndefined();
	});

	it("reflects the build's own version and release iteration", () => {
		expect(localForkRelease()).toEqual({
			tag: `${BRAND_RELEASE_TAG_PREFIX}${VERSION}-z${BRAND_RELEASE_ITERATION}`,
			version: VERSION,
			iteration: BRAND_RELEASE_ITERATION,
		});
	});
});

describe("fork release ordering", () => {
	const rel = (version: string, iteration: number) => ({
		tag: formatForkReleaseTag(version, iteration),
		version,
		iteration,
	});

	it("orders by upstream base version first", () => {
		expect(compareForkReleases(rel("17.2.13", 1), rel("17.2.12", 9))).toBeGreaterThan(0);
		expect(compareForkReleases(rel("17.2.12", 9), rel("17.3.0", 1))).toBeLessThan(0);
	});

	it("detects same-version -zN hotfix iterations", () => {
		expect(compareForkReleases(rel("17.2.12", 3), rel("17.2.12", 2))).toBeGreaterThan(0);
		expect(compareForkReleases(rel("17.2.12", 2), rel("17.2.12", 2))).toBe(0);
	});
});

describe("fork release discovery", () => {
	it("queries the fork repo's latest release and parses its tag", async () => {
		let requested: string | undefined;
		const release = await getLatestForkRelease(async input => {
			requested = String(input);
			return Response.json({ tag_name: `${BRAND_RELEASE_TAG_PREFIX}18.0.0-z1` });
		});

		expect(requested).toBe(`https://api.github.com/repos/${BRAND_REPO}/releases/latest`);
		expect(release).toEqual({ tag: `${BRAND_RELEASE_TAG_PREFIX}18.0.0-z1`, version: "18.0.0", iteration: 1 });
	});

	it("sends the GitHub token and surfaces anonymous rate limits with guidance", async () => {
		const authorizations: Array<string | null> = [];
		await getLatestForkRelease(async (_input, init) => {
			authorizations.push(new Headers(init?.headers).get("Authorization"));
			return Response.json({ tag_name: `${BRAND_RELEASE_TAG_PREFIX}18.0.0-z1` });
		}, "test-token");
		expect(authorizations).toEqual(["Bearer test-token"]);

		await expect(
			getLatestForkRelease(async () => new Response(null, { status: 403, statusText: "rate limited" }), ""),
		).rejects.toThrow("retry later or set GITHUB_TOKEN or GH_TOKEN");
	});

	it("fails loudly when the latest release is not a fork release", async () => {
		await expect(getLatestForkRelease(async () => Response.json({ tag_name: "v18.0.0" }), "t")).rejects.toThrow(
			`does not match ${BRAND_RELEASE_TAG_PREFIX}<version>-z<n>`,
		);
		await expect(getLatestForkRelease(async () => Response.json({ id: 1 }), "t")).rejects.toThrow("missing tag_name");
	});
});

describe("fork update channel enforcement", () => {
	it("allows only the binary method and points other installs at the install script", () => {
		expect(ensureForkBinaryTarget({ method: "binary" })).toBeUndefined();
		for (const method of ["brew", "mise", "bun", "npm"]) {
			expect(() => ensureForkBinaryTarget({ method })).toThrow(
				`"${method}" installs are not supported: zcode ships only GitHub release binaries`,
			);
		}
	});
});

describe("fork release asset lookup", () => {
	it("resolves the binary asset from the fork tag, not the upstream v<version> tag", async () => {
		// 回归 zcode-v* 通道:资产必须按完整 fork tag 查询与下载,
		// `v<版本>` tag 在 fork 仓库不存在(上游语义),线上表现为 404。
		const tag = `${BRAND_RELEASE_TAG_PREFIX}17.2.12-z3`;
		const binaryName = `${APP_NAME}-linux-x64`;
		const requests: string[] = [];
		const fetchImpl = async (input: string | URL | Request): Promise<Response> => {
			requests.push(String(input));
			throw new Error("stop after metadata request");
		};

		await expect(
			updateViaBinaryAt("/nonexistent/target", "17.2.12", {
				binaryName,
				fetchImpl,
				releaseTag: tag,
				githubToken: "t",
			}),
		).rejects.toThrow("stop after metadata request");
		expect(requests).toEqual([`https://api.github.com/repos/${BRAND_REPO}/releases/tags/${encodeURIComponent(tag)}`]);
	});
});
