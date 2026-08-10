import { Database } from "bun:sqlite";
import * as fs from "node:fs";
import * as path from "node:path";

/**
 * share blob 存储：客户端 `POST <share.serverUrl>` 上传密封字节
 * （`[12B IV][AES-256-GCM(gzip(JSON))]`，密钥只在链接 fragment 里），
 * 服务端只见密文。id 契约（见 export/share.ts 与 share-loader.js）：
 * - 形如 `[A-Za-z0-9_-]{10,64}`；
 * - 绝不能是纯小写 hex（20-64 位），viewer 用 hex 形状把 id 路由到 GitHub gist。
 */
export interface ShareMeta {
	id: string;
	bytes: number;
	createdAt: string;
	expiresAt: string | null;
	downloads: number;
}

export type ShareGetResult = { kind: "ok"; blob: Uint8Array } | { kind: "missing" } | { kind: "gone" };

/** viewer 把纯 hex id 当 gist；生成时规避（22 位 base64url 撞上的概率 ~1e-13，防御一行）。 */
const GIST_SHAPE_RE = /^[0-9a-f]{20,64}$/;
const SHARE_ID_BYTES = 16;

interface ShareRow {
	id: string;
	blob: Uint8Array;
	bytes: number;
	created_at: number;
	expires_at: number | null;
	downloads: number;
}

function generateShareId(): string {
	for (;;) {
		const bytes = new Uint8Array(SHARE_ID_BYTES);
		crypto.getRandomValues(bytes);
		const id = Buffer.from(bytes).toString("base64url");
		if (!GIST_SHAPE_RE.test(id)) return id;
	}
}

export class ShareStore {
	#db: Database;
	#ttlMs: number;

	/** @param ttlMs 0 = 永不过期 */
	constructor(dbPath: string, ttlMs: number) {
		if (dbPath !== ":memory:") {
			fs.mkdirSync(path.dirname(dbPath), { recursive: true });
		}
		this.#db = new Database(dbPath, { create: true });
		this.#db.exec("PRAGMA journal_mode = WAL;");
		this.#db.exec(`
			CREATE TABLE IF NOT EXISTS shares (
				id TEXT PRIMARY KEY,
				blob BLOB NOT NULL,
				bytes INTEGER NOT NULL,
				created_at INTEGER NOT NULL,
				expires_at INTEGER,
				downloads INTEGER NOT NULL DEFAULT 0
			);
		`);
		this.#ttlMs = ttlMs;
	}

	put(blob: Uint8Array): string {
		const now = Date.now();
		const expiresAt = this.#ttlMs > 0 ? now + this.#ttlMs : null;
		for (;;) {
			const id = generateShareId();
			try {
				this.#db
					.query("INSERT INTO shares (id, blob, bytes, created_at, expires_at) VALUES (?, ?, ?, ?, ?)")
					.run(id, blob, blob.byteLength, now, expiresAt);
				return id;
			} catch (err) {
				// 128 位随机 id 撞主键仅在理论上可能；重试即可。
				if (err instanceof Error && err.message.includes("UNIQUE")) continue;
				throw err;
			}
		}
	}

	/** 命中时自增下载计数；过期返回 gone（sweep 清掉后退化为 missing，viewer 文案一致）。 */
	get(id: string): ShareGetResult {
		const row = this.#db.query<ShareRow, [string]>("SELECT * FROM shares WHERE id = ?").get(id);
		if (!row) return { kind: "missing" };
		if (row.expires_at !== null && row.expires_at < Date.now()) return { kind: "gone" };
		this.#db.query("UPDATE shares SET downloads = downloads + 1 WHERE id = ?").run(id);
		return { kind: "ok", blob: row.blob };
	}

	delete(id: string): boolean {
		return this.#db.query("DELETE FROM shares WHERE id = ?").run(id).changes > 0;
	}

	list(limit = 500): ShareMeta[] {
		const rows = this.#db
			.query<Omit<ShareRow, "blob">, [number]>(
				"SELECT id, bytes, created_at, expires_at, downloads FROM shares ORDER BY created_at DESC LIMIT ?",
			)
			.all(limit);
		return rows.map(row => ({
			id: row.id,
			bytes: row.bytes,
			createdAt: new Date(row.created_at).toISOString(),
			expiresAt: row.expires_at === null ? null : new Date(row.expires_at).toISOString(),
			downloads: row.downloads,
		}));
	}

	stats(): { count: number; bytes: number } {
		const row = this.#db
			.query<{ count: number; bytes: number | null }, []>(
				"SELECT COUNT(*) AS count, SUM(bytes) AS bytes FROM shares",
			)
			.get();
		return { count: row?.count ?? 0, bytes: row?.bytes ?? 0 };
	}

	/** 删除已过期的行；返回删除数。 */
	sweep(): number {
		return this.#db.query("DELETE FROM shares WHERE expires_at IS NOT NULL AND expires_at < ?").run(Date.now())
			.changes;
	}

	close(): void {
		this.#db.close();
	}
}
