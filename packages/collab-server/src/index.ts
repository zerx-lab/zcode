import * as path from "node:path";
import pkg from "../package.json";
import { createApp } from "./app";
import { AdminSessions } from "./auth";
import { loadConfig, type ServerConfig } from "./config";
import { RelayHub, type RelaySocketData, ROOM_PATH_RE } from "./relay";
import { ShareStore } from "./share-store";

const SWEEP_INTERVAL_MS = 60 * 60 * 1000;
/** 密封帧远小于 1MB（客户端 shrink 保证）；8MB 上限只防滥用，不卡正常流量。 */
const MAX_WS_PAYLOAD_BYTES = 8 * 1024 * 1024;

export interface CollabServer {
	url: string;
	hub: RelayHub;
	store: ShareStore;
	/** 通知全部房间关闭、停服、关库。幂等。 */
	stop(): void;
}

export function startServer(cfg: ServerConfig): CollabServer {
	const hub = new RelayHub(cfg.maxGuests);
	const dbPath = cfg.dataDir === ":memory:" ? ":memory:" : path.join(cfg.dataDir, "shares.sqlite");
	const store = new ShareStore(dbPath, cfg.shareTtlDays * 86_400_000);
	const sessions = new AdminSessions();
	const app = createApp({ cfg, hub, store, sessions, version: pkg.version, startedAt: new Date() });

	const server = Bun.serve({
		port: cfg.port,
		hostname: cfg.hostname,
		fetch(req, srv): Response | Promise<Response> | undefined {
			const url = new URL(req.url);
			const match = ROOM_PATH_RE.exec(url.pathname);
			if (match) {
				const role = url.searchParams.get("role");
				if (role !== "host" && role !== "guest") return new Response("not found", { status: 404 });
				const data: RelaySocketData = { roomId: match[1], role, peerId: 0 };
				if (srv.upgrade(req, { data })) return undefined;
				return new Response("websocket upgrade required", { status: 426 });
			}
			return app.fetch(req);
		},
		websocket: { ...hub.handlers, maxPayloadLength: MAX_WS_PAYLOAD_BYTES },
	});

	const sweeper = setInterval(() => store.sweep(), SWEEP_INTERVAL_MS);
	// 启动时先清一轮过期 blob，避免长停机后的陈尸。
	store.sweep();

	let stopped = false;
	return {
		url: `http://${server.hostname}:${server.port}`,
		hub,
		store,
		stop(): void {
			if (stopped) return;
			stopped = true;
			clearInterval(sweeper);
			hub.closeAll();
			server.stop(true);
			store.close();
		},
	};
}

/** CLI 入口；内嵌构建的生成入口（dist/embed/entry.ts）注册资产归档后也调它。 */
export function main(): void {
	const cfg: ServerConfig = loadConfig();
	const instance = startServer(cfg);
	const shutdown = (): void => {
		instance.stop();
		process.exit(0);
	};
	process.on("SIGINT", shutdown);
	process.on("SIGTERM", shutdown);
	console.log(`collab-server v${pkg.version} listening on ${instance.url}`);
	console.log(`  relay      ws://<host>/r/<roomId>?role=host|guest`);
	console.log(`  share      POST /s · GET /s/<id> · GET /s/<id>/raw`);
	console.log(`  dashboard  /dash/`);
	console.log(`  guest UI   /  (collab-web: ${cfg.webDir ?? "未构建"})`);
	if (!cfg.adminPassword) {
		console.log("  admin      已禁用（在 .env 设置 COLLAB_ADMIN_PASSWORD / COLLAB_ADMIN_USER 启用登录）");
	}
}

if (import.meta.main) main();
