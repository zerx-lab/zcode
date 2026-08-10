import * as fs from "node:fs";
import * as path from "node:path";
import { materializeEmbeddedPublic } from "./embedded";

/**
 * 环境变量驱动的服务配置。所有键都带 `COLLAB_` 前缀，避免与宿主机其他服务冲突。
 *
 * 静态资源目录按「部署产物优先、仓库内开发兜底」的顺序探测：
 * - 部署布局：`dist/server.js` + `dist/public/{web,dash,share-viewer.html}`
 * - 仓库开发：`bun src/index.ts` 时回退到 `../dist/public/*`（先跑 `bun run build`）
 */
export interface ServerConfig {
	port: number;
	hostname: string;
	/** SQLite 数据目录（share blob 存储）。 */
	dataDir: string;
	/** 管理员用户名（`.env` COLLAB_ADMIN_USER，默认 "admin"）。 */
	adminUser: string;
	/** 管理员密码（`.env` COLLAB_ADMIN_PASSWORD）；未设置时管理功能整体禁用（403）。 */
	adminPassword: string | null;
	/** share blob 保存天数；0 = 永不过期。 */
	shareTtlDays: number;
	/** 单房间访客上限，超出以 4029 拒绝。 */
	maxGuests: number;
	/** share blob 尺寸上限（字节），与客户端 SERVER_MAX_SEALED_BYTES 对齐。 */
	shareMaxBytes: number;
	/** collab-web 访客 UI 静态目录；null = 未构建。 */
	webDir: string | null;
	/** dashboard SPA 静态目录；null = 未构建。 */
	dashDir: string | null;
	/** share viewer 单文件 HTML；null = 未构建。 */
	shareViewerPath: string | null;
}

function intEnv(name: string, fallback: number): number {
	const raw = Bun.env[name];
	if (raw === undefined || raw.trim() === "") return fallback;
	const value = Number(raw);
	if (!Number.isFinite(value) || value < 0) {
		throw new Error(`invalid ${name}: ${raw}`);
	}
	return value;
}

function firstExisting(candidates: readonly string[]): string | null {
	for (const candidate of candidates) {
		try {
			if (fs.existsSync(candidate)) return candidate;
		} catch {
			// keep probing
		}
	}
	return null;
}

export function loadConfig(): ServerConfig {
	// 四种运行形态的资产探测锚点（env 覆盖 > 内嵌 > 磁盘）：
	// - 单文件二进制：资产内嵌在归档里，启动时释放到 tmp（embedded.ts）。
	//   内嵌排最前——二进制旁若残留旧版 public/ 不能反超自带资产。
	// - 二进制 + 旁挂 public/：import.meta.dir 落在 /$bunfs 虚拟卷，
	//   只有 process.execPath 可靠。
	// - dist/server.js：import.meta.dir = dist/ → ./public。
	// - 仓库源码 dev：src/ → ../dist/public（先跑 bun run build）或 collab-web 直连。
	const embeddedDir = materializeEmbeddedPublic();
	const execDir = path.dirname(process.execPath);
	const here = import.meta.dir;
	const dataDir = Bun.env.COLLAB_DATA_DIR ?? path.join(process.cwd(), "data");
	const candidates = (relPath: string): string[] => [
		...(embeddedDir ? [path.join(embeddedDir, relPath)] : []),
		path.join(execDir, "public", relPath),
		path.join(here, "public", relPath),
		path.join(here, "../dist/public", relPath),
	];
	const webDir =
		Bun.env.COLLAB_WEB_DIR ?? firstExisting([...candidates("web"), path.join(here, "../../collab-web/dist")]);
	const dashDir = Bun.env.COLLAB_DASH_DIR ?? firstExisting(candidates("dash"));
	const shareViewerPath = Bun.env.COLLAB_SHARE_VIEWER ?? firstExisting(candidates("share-viewer.html"));
	return {
		port: intEnv("COLLAB_PORT", 8790),
		hostname: Bun.env.COLLAB_HOSTNAME ?? "0.0.0.0",
		dataDir,
		adminUser: Bun.env.COLLAB_ADMIN_USER?.trim() || "admin",
		adminPassword: Bun.env.COLLAB_ADMIN_PASSWORD || null,
		shareTtlDays: intEnv("COLLAB_SHARE_TTL_DAYS", 30),
		maxGuests: intEnv("COLLAB_MAX_GUESTS", 32),
		shareMaxBytes: intEnv("COLLAB_MAX_SHARE_BYTES", 1_000_000),
		webDir,
		dashDir,
		shareViewerPath,
	};
}
