#!/usr/bin/env bun
/**
 * 发布 tag 一致性门禁（fork 新文件，zcode-release.yml 调用）。
 *
 * 断言 `<tag> === zcode-v<package.json 版本>-z<BRAND_RELEASE_ITERATION>`。
 * BRAND_RELEASE_ITERATION 被编进二进制供 `zcode update` 比较迭代号；
 * 打 tag 忘了 bump 常量会让已装用户检测不到（或重复检测）这次发布，
 * 所以在流水线里直接 fail 掉，而不是靠人肉记得。
 *
 * 用法：bun brand/check-release-tag.ts <tag>
 */
import * as path from "node:path";
import { BRAND_RELEASE_ITERATION, BRAND_RELEASE_TAG_PREFIX } from "../packages/utils/src/brand";

const tag = process.argv[2];
if (!tag) {
	console.error("usage: bun brand/check-release-tag.ts <tag>");
	process.exit(2);
}

const pkgPath = path.join(import.meta.dir, "..", "packages", "coding-agent", "package.json");
const { version } = (await Bun.file(pkgPath).json()) as { version: string };

const expected = `${BRAND_RELEASE_TAG_PREFIX}${version}-z${BRAND_RELEASE_ITERATION}`;
if (tag !== expected) {
	console.error(
		`Release tag mismatch:\n` +
			`  tag:      ${tag}\n` +
			`  expected: ${expected}  (package.json ${version} + BRAND_RELEASE_ITERATION ${BRAND_RELEASE_ITERATION})\n` +
			`打 tag 前先把 packages/utils/src/brand.ts 的 BRAND_RELEASE_ITERATION 与 tag 对齐。`,
	);
	process.exit(1);
}
console.log(`Release tag OK: ${tag}`);
