import { timingSafeEqual } from "node:crypto";

/** 管理会话有效期：24 小时；服务重启即全部失效（内存态，够运维面板用）。 */
const SESSION_TTL_MS = 24 * 60 * 60 * 1000;
const SESSION_TOKEN_BYTES = 32;

export interface AdminCredentials {
	username: string;
	password: string;
}

/** sha256 后 timingSafeEqual：恒时比较且不泄漏长度。 */
function digestEquals(a: string, b: string): boolean {
	const ha = new Bun.CryptoHasher("sha256").update(a).digest();
	const hb = new Bun.CryptoHasher("sha256").update(b).digest();
	return timingSafeEqual(ha, hb);
}

export function credentialsMatch(given: AdminCredentials, expected: AdminCredentials): boolean {
	// 独立求值再合取，避免短路引入时序差
	const userOk = digestEquals(given.username, expected.username);
	const passOk = digestEquals(given.password, expected.password);
	return userOk && passOk;
}

/** 登录换发的内存会话 token 表。 */
export class AdminSessions {
	#sessions = new Map<string, number>();
	#ttlMs: number;

	constructor(ttlMs = SESSION_TTL_MS) {
		this.#ttlMs = ttlMs;
	}

	issue(): { token: string; expiresAt: string } {
		this.#prune();
		const bytes = new Uint8Array(SESSION_TOKEN_BYTES);
		crypto.getRandomValues(bytes);
		const token = Buffer.from(bytes).toString("base64url");
		const expiresAt = Date.now() + this.#ttlMs;
		this.#sessions.set(token, expiresAt);
		return { token, expiresAt: new Date(expiresAt).toISOString() };
	}

	verify(token: string): boolean {
		const expiresAt = this.#sessions.get(token);
		if (expiresAt === undefined) return false;
		if (expiresAt < Date.now()) {
			this.#sessions.delete(token);
			return false;
		}
		return true;
	}

	revoke(token: string): void {
		this.#sessions.delete(token);
	}

	#prune(): void {
		const now = Date.now();
		for (const [token, expiresAt] of this.#sessions) {
			if (expiresAt < now) this.#sessions.delete(token);
		}
	}
}
