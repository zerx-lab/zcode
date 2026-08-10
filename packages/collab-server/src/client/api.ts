import { BRAND_APP_NAME } from "@oh-my-pi/pi-utils/brand-consts";

/** localStorage key：登录换发的管理会话 token（非明文凭据），按 fork 应用名命名空间。 */
const ADMIN_SESSION_STORAGE_KEY = `${BRAND_APP_NAME}-collab-dash-admin-session`;

export interface CollabStatus {
	ok: true;
	version: string;
	uptimeMs: number;
	startedAt: string;
	rooms: { count: number; guests: number };
	shares: { count: number; bytes: number };
	limits: { shareMaxBytes: number; maxGuests: number; shareTtlDays: number };
}

export interface AdminRoom {
	id: string;
	guests: number;
	createdAt: string;
}

export interface AdminShare {
	id: string;
	bytes: number;
	createdAt: string;
	expiresAt: string | null;
	downloads: number;
}

export interface AdminSession {
	token: string;
	expiresAt: string;
}

/** Thrown for any non-2xx API response; carries the HTTP status so callers can branch on 401/403. */
export class ApiError extends Error {
	readonly status: number;

	constructor(status: number, message: string) {
		super(message);
		this.name = "ApiError";
		this.status = status;
	}
}

export function getSessionToken(): string | null {
	return localStorage.getItem(ADMIN_SESSION_STORAGE_KEY);
}

export function setSessionToken(token: string | null): void {
	if (token && token.length > 0) localStorage.setItem(ADMIN_SESSION_STORAGE_KEY, token);
	else localStorage.removeItem(ADMIN_SESSION_STORAGE_KEY);
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
	const token = getSessionToken();
	const headers = new Headers(init?.headers);
	if (token) headers.set("Authorization", `Bearer ${token}`);

	const res = await fetch(path, { ...init, headers });
	if (!res.ok) {
		let message = res.statusText || `HTTP ${res.status}`;
		try {
			const body = (await res.json()) as { error?: unknown };
			if (typeof body.error === "string" && body.error.length > 0) message = body.error;
		} catch {
			// response body isn't JSON — keep the statusText fallback
		}
		throw new ApiError(res.status, message);
	}
	return (await res.json()) as T;
}

/** 用 .env 配置的管理员账号密码登录；成功后把会话 token 落到 localStorage。 */
export async function login(username: string, password: string): Promise<AdminSession> {
	const session = await request<AdminSession>("/api/admin/login", {
		method: "POST",
		headers: { "content-type": "application/json" },
		body: JSON.stringify({ username, password }),
	});
	setSessionToken(session.token);
	return session;
}

/** 撤销服务端会话并清掉本地 token；网络失败也保证本地登出。 */
export async function logout(): Promise<void> {
	try {
		await request<{ ok: true }>("/api/admin/logout", { method: "POST" });
	} finally {
		setSessionToken(null);
	}
}

export function fetchStatus(): Promise<CollabStatus> {
	return request<CollabStatus>("/api/status");
}

export function fetchRooms(): Promise<{ rooms: AdminRoom[] }> {
	return request<{ rooms: AdminRoom[] }>("/api/admin/rooms");
}

export function fetchShares(): Promise<{ shares: AdminShare[] }> {
	return request<{ shares: AdminShare[] }>("/api/admin/shares");
}

export function deleteShare(id: string): Promise<{ ok: true }> {
	return request<{ ok: true }>(`/api/admin/shares/${encodeURIComponent(id)}`, { method: "DELETE" });
}
