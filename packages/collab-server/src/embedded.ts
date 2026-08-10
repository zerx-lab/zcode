import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { BRAND_APP_NAME } from "@oh-my-pi/pi-utils/brand-consts";

/**
 * 单文件二进制的前端资产内嵌（先例：packages/stats 的 embedded-client）。
 *
 * `--compile-linux` 生成的入口把 `dist/public` 的归档（base64(gzip(JSON
 * manifest)))）经 {@link setEmbeddedPublicArchive} 注册进来；启动时
 * {@link materializeEmbeddedPublic} 释放到 tmp 下按内容 hash 命名的目录并复用
 * （`.complete` 标记保证半途中断的释放不会被当成完整资产）。释放成目录而不是
 * 内存 Map，是为了让 config/app 的静态目录探测与服务代码零改动。
 */

/** manifest：相对路径（posix 分隔）→ base64 文件内容。 */
export type EmbeddedManifest = Record<string, string>;

let archive: string | null = null;

export function setEmbeddedPublicArchive(base64: string): void {
	archive = base64;
}

const COMPLETE_MARKER = ".complete";

/** 非内嵌构建（archive 未注册）返回 null。 */
export function materializeEmbeddedPublic(): string | null {
	if (!archive) return null;
	const dir = path.join(os.tmpdir(), `${BRAND_APP_NAME}-collab-public-${Bun.hash(archive).toString(16)}`);
	const marker = path.join(dir, COMPLETE_MARKER);
	if (fs.existsSync(marker)) return dir;
	const manifest = JSON.parse(
		new TextDecoder().decode(Bun.gunzipSync(Buffer.from(archive, "base64"))),
	) as EmbeddedManifest;
	for (const [relPath, contentBase64] of Object.entries(manifest)) {
		const target = path.join(dir, relPath);
		fs.mkdirSync(path.dirname(target), { recursive: true });
		fs.writeFileSync(target, Buffer.from(contentBase64, "base64"));
	}
	fs.writeFileSync(marker, "");
	return dir;
}
