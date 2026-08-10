import * as path from "node:path";
import type { Context } from "hono";
import { Hono } from "hono";
import { type AdminSessions, credentialsMatch } from "./auth";
import type { ServerConfig } from "./config";
import type { RelayHub } from "./relay";
import type { ShareStore } from "./share-store";

/** 上传返回的 id 与 URL path 段共用的形状（见 export/share.ts uploadToServer 校验）。 */
const SHARE_ID_RE = /^[A-Za-z0-9_-]{10,64}$/;

export interface AppDeps {
	cfg: ServerConfig;
	hub: RelayHub;
	store: ShareStore;
	sessions: AdminSessions;
	version: string;
	startedAt: Date;
}

function bearerToken(c: Context): string {
	const header = c.req.header("authorization") ?? "";
	return header.startsWith("Bearer ") ? header.slice("Bearer ".length) : "";
}

/** 静态文件响应：HTML 不缓存（SPA 入口会更新），带 hash 的资源永久缓存。 */
async function serveFile(filePath: string): Promise<Response | null> {
	const file = Bun.file(filePath);
	if (!(await file.exists())) return null;
	const cacheControl = filePath.endsWith(".html") ? "no-cache" : "public, max-age=31536000, immutable";
	return new Response(file, { headers: { "cache-control": cacheControl } });
}

/** 目录内解析相对路径，拒绝逃逸；目录/空路径回落 index.html。 */
async function serveStatic(root: string, relPath: string): Promise<Response | null> {
	const cleaned = relPath.replace(/^\/+/, "");
	const resolved = path.resolve(root, cleaned === "" ? "index.html" : cleaned);
	if (resolved !== root && !resolved.startsWith(root + path.sep)) return null;
	const direct = await serveFile(resolved);
	if (direct) return direct;
	// SPA fallback：无扩展名的深链接回 index.html
	if (!path.basename(resolved).includes(".")) {
		return serveFile(path.join(root, "index.html"));
	}
	return null;
}

function missingAsset(what: string): Response {
	return new Response(`${what} 未构建：先在 packages/collab-server 运行 \`bun run build\``, {
		status: 503,
		headers: { "content-type": "text/plain; charset=utf-8" },
	});
}

export function createApp(deps: AppDeps): Hono {
	const { cfg, hub, store, sessions, version, startedAt } = deps;
	const app = new Hono();

	app.get("/healthz", c => c.json({ ok: true }));

	// ── share 存储（契约见 export/share.ts + share-loader.js）─────────────
	app.post("/s", async c => {
		const body = new Uint8Array(await c.req.arrayBuffer());
		if (body.byteLength === 0) return c.json({ error: "empty body" }, 400);
		if (body.byteLength > cfg.shareMaxBytes) {
			return c.json({ error: `blob exceeds ${cfg.shareMaxBytes} bytes` }, 413);
		}
		return c.json({ id: store.put(body) });
	});

	app.get("/s/:id", async c => {
		const id = c.req.param("id");
		if (!SHARE_ID_RE.test(id)) return c.notFound();
		if (!cfg.shareViewerPath) return missingAsset("share viewer");
		// viewer 页恒定返回；blob 缺失/过期由页内 loader 请求 /raw 后展示文案。
		const page = await serveFile(cfg.shareViewerPath);
		return page ?? missingAsset("share viewer");
	});

	app.get("/s/:id/raw", c => {
		const id = c.req.param("id");
		if (!SHARE_ID_RE.test(id)) return c.notFound();
		const result = store.get(id);
		if (result.kind === "missing") return c.body(null, 404);
		if (result.kind === "gone") return c.body(null, 410);
		return new Response(result.blob, {
			headers: {
				"content-type": "application/octet-stream",
				"cache-control": "private, max-age=3600",
			},
		});
	});

	// ── 公开状态 API（仅聚合计数，不暴露任何 id）──────────────────────────
	app.get("/api/status", c => {
		const relay = hub.metrics();
		const shares = store.stats();
		return c.json({
			ok: true,
			version,
			uptimeMs: Date.now() - startedAt.getTime(),
			startedAt: startedAt.toISOString(),
			rooms: { count: relay.rooms, guests: relay.guests },
			shares,
			limits: {
				shareMaxBytes: cfg.shareMaxBytes,
				maxGuests: cfg.maxGuests,
				shareTtlDays: cfg.shareTtlDays,
			},
		});
	});

	// ── 管理 API：room id 与 share id 都是能力凭证，必须登录门禁 ──────────
	// 凭据来自 .env（COLLAB_ADMIN_USER / COLLAB_ADMIN_PASSWORD，Bun 自动加载）；
	// 登录换发内存会话 token。login 注册在 use() 之前，不受会话门禁约束。
	app.post("/api/admin/login", async c => {
		if (!cfg.adminPassword) return c.json({ error: "admin disabled" }, 403);
		const body = (await c.req.json().catch(() => null)) as { username?: unknown; password?: unknown } | null;
		const username = typeof body?.username === "string" ? body.username : "";
		const password = typeof body?.password === "string" ? body.password : "";
		const expected = { username: cfg.adminUser, password: cfg.adminPassword };
		if (!credentialsMatch({ username, password }, expected)) {
			return c.json({ error: "invalid credentials" }, 401);
		}
		return c.json(sessions.issue());
	});

	app.use("/api/admin/*", async (c, next) => {
		if (!cfg.adminPassword) return c.json({ error: "admin disabled" }, 403);
		if (!sessions.verify(bearerToken(c))) return c.json({ error: "unauthorized" }, 401);
		await next();
	});

	app.post("/api/admin/logout", c => {
		sessions.revoke(bearerToken(c));
		return c.json({ ok: true });
	});

	app.get("/api/admin/rooms", c => c.json({ rooms: hub.listRooms() }));
	app.get("/api/admin/shares", c => c.json({ shares: store.list() }));
	app.delete("/api/admin/shares/:id", c => {
		const id = c.req.param("id");
		if (!SHARE_ID_RE.test(id) || !store.delete(id)) return c.notFound();
		return c.json({ ok: true });
	});

	// ── dashboard SPA（/dash 子路径）────────────────────────────────────
	app.get("/dash/*", async c => {
		if (!cfg.dashDir) return missingAsset("dashboard");
		const rel = c.req.path.slice("/dash".length);
		return (await serveStatic(cfg.dashDir, rel)) ?? c.notFound();
	});
	app.get("/dash", c => c.redirect("/dash/"));

	// ── collab-web 访客 UI（根路径 + SPA fallback，对齐上游生产布局）───────
	app.get("/*", async c => {
		if (!cfg.webDir) return missingAsset("collab-web 访客 UI");
		return (await serveStatic(cfg.webDir, c.req.path)) ?? c.notFound();
	});

	return app;
}
