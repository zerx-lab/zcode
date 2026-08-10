import { afterEach, describe, expect, it } from "bun:test";
import * as os from "node:os";
import * as path from "node:path";
import { ShareStore } from "../src/share-store";
import { startHarness, type TestHarness } from "./helpers";

/** 客户端 uploadToServer 对返回 id 的校验（export/share.ts:683）。 */
const CLIENT_ID_RE = /^[A-Za-z0-9_-]{10,64}$/;
/** viewer 用纯 hex 形状把 id 路由到 gist（share-loader.js:15）——服务端 id 必须避开。 */
const GIST_ID_RE = /^[0-9a-f]{20,64}$/;

let harness: TestHarness | null = null;

function h(): TestHarness {
	if (!harness) throw new Error("harness not started");
	return harness;
}

afterEach(() => {
	harness?.stop();
	harness = null;
});

async function upload(bytes: Uint8Array): Promise<{ status: number; id: string }> {
	const res = await fetch(`${h().httpUrl}/s`, {
		method: "POST",
		headers: { "content-type": "application/octet-stream" },
		body: bytes,
	});
	const body = res.ok ? ((await res.json()) as { id: string }) : { id: "" };
	return { status: res.status, id: body.id };
}

describe("collab-server share store contract", () => {
	it("accepts an upload and serves the exact bytes back at /s/<id>/raw", async () => {
		harness = startHarness();
		const blob = crypto.getRandomValues(new Uint8Array(4096));
		const { status, id } = await upload(blob);
		expect(status).toBe(200);
		expect(id).toMatch(CLIENT_ID_RE);
		expect(id).not.toMatch(GIST_ID_RE);

		const raw = await fetch(`${h().httpUrl}/s/${id}/raw`);
		expect(raw.status).toBe(200);
		expect(raw.headers.get("content-type")).toBe("application/octet-stream");
		expect(new Uint8Array(await raw.arrayBuffer())).toEqual(blob);
	});

	it("returns 404 for unknown ids, 400 for empty and 413 for oversized uploads", async () => {
		harness = startHarness({ shareMaxBytes: 128 });
		const missing = await fetch(`${h().httpUrl}/s/does-not-exist-0000/raw`);
		expect(missing.status).toBe(404);

		const empty = await fetch(`${h().httpUrl}/s`, { method: "POST", body: new Uint8Array(0) });
		expect(empty.status).toBe(400);

		const { status } = await upload(new Uint8Array(129));
		expect(status).toBe(413);
	});

	it("expires blobs: 410 while the row lingers, missing after sweep", async () => {
		const store = new ShareStore(":memory:", 10);
		const id = store.put(new Uint8Array([1, 2, 3]));
		expect(store.get(id).kind).toBe("ok");
		await Bun.sleep(20);
		expect(store.get(id).kind).toBe("gone");
		expect(store.sweep()).toBe(1);
		expect(store.get(id).kind).toBe("missing");
		store.close();
	});

	it("serves the viewer page for well-formed ids regardless of blob existence", async () => {
		const viewerPath = path.join(os.tmpdir(), `collab-server-viewer-${Date.now()}.html`);
		await Bun.write(viewerPath, "<!DOCTYPE html><title>viewer-marker</title>");
		harness = startHarness({ shareViewerPath: viewerPath });
		const res = await fetch(`${h().httpUrl}/s/some-share-id-123`);
		expect(res.status).toBe(200);
		expect(await res.text()).toContain("viewer-marker");
		// id 形状非法（含点号）→ 404 而非 viewer
		const bad = await fetch(`${h().httpUrl}/s/bad.id`);
		expect(bad.status).toBe(404);
	});
});

describe("collab-server status + admin API", () => {
	it("reports aggregate counts without exposing ids", async () => {
		harness = startHarness();
		await upload(new Uint8Array(64));
		const res = await fetch(`${h().httpUrl}/api/status`);
		expect(res.status).toBe(200);
		const body = (await res.json()) as {
			ok: boolean;
			rooms: { count: number; guests: number };
			shares: { count: number; bytes: number };
		};
		expect(body.ok).toBe(true);
		expect(body.rooms).toEqual({ count: 0, guests: 0 });
		expect(body.shares).toEqual({ count: 1, bytes: 64 });
	});

	it("disables admin endpoints without a configured password (403, including login)", async () => {
		harness = startHarness();
		const res = await fetch(`${h().httpUrl}/api/admin/shares`);
		expect(res.status).toBe(403);
		const login = await fetch(`${h().httpUrl}/api/admin/login`, {
			method: "POST",
			headers: { "content-type": "application/json" },
			body: JSON.stringify({ username: "admin", password: "anything" }),
		});
		expect(login.status).toBe(403);
	});

	it("rejects bad credentials and unauthenticated bearer tokens with 401", async () => {
		harness = startHarness({ adminUser: "ops", adminPassword: "sesame" });
		const badPass = await fetch(`${h().httpUrl}/api/admin/login`, {
			method: "POST",
			headers: { "content-type": "application/json" },
			body: JSON.stringify({ username: "ops", password: "wrong" }),
		});
		expect(badPass.status).toBe(401);
		const badUser = await fetch(`${h().httpUrl}/api/admin/login`, {
			method: "POST",
			headers: { "content-type": "application/json" },
			body: JSON.stringify({ username: "admin", password: "sesame" }),
		});
		expect(badUser.status).toBe(401);
		const noBody = await fetch(`${h().httpUrl}/api/admin/login`, { method: "POST" });
		expect(noBody.status).toBe(401);

		const noToken = await fetch(`${h().httpUrl}/api/admin/shares`);
		expect(noToken.status).toBe(401);
		const fakeToken = await fetch(`${h().httpUrl}/api/admin/shares`, {
			headers: { authorization: "Bearer forged" },
		});
		expect(fakeToken.status).toBe(401);
	});

	it("logs in with .env credentials, lists and deletes shares, and honors logout", async () => {
		harness = startHarness({ adminUser: "ops", adminPassword: "sesame" });
		const { id } = await upload(new Uint8Array(32));

		const login = await fetch(`${h().httpUrl}/api/admin/login`, {
			method: "POST",
			headers: { "content-type": "application/json" },
			body: JSON.stringify({ username: "ops", password: "sesame" }),
		});
		expect(login.status).toBe(200);
		const session = (await login.json()) as { token: string; expiresAt: string };
		expect(session.token.length).toBeGreaterThan(20);
		expect(Date.parse(session.expiresAt)).toBeGreaterThan(Date.now());
		const auth = { authorization: `Bearer ${session.token}` };

		const listed = await fetch(`${h().httpUrl}/api/admin/shares`, { headers: auth });
		expect(listed.status).toBe(200);
		const listing = (await listed.json()) as { shares: Array<{ id: string; bytes: number }> };
		expect(listing.shares).toHaveLength(1);
		expect(listing.shares[0].id).toBe(id);
		expect(listing.shares[0].bytes).toBe(32);

		const deleted = await fetch(`${h().httpUrl}/api/admin/shares/${id}`, { method: "DELETE", headers: auth });
		expect(deleted.status).toBe(200);
		const gone = await fetch(`${h().httpUrl}/s/${id}/raw`);
		expect(gone.status).toBe(404);

		const logout = await fetch(`${h().httpUrl}/api/admin/logout`, { method: "POST", headers: auth });
		expect(logout.status).toBe(200);
		const revoked = await fetch(`${h().httpUrl}/api/admin/rooms`, { headers: auth });
		expect(revoked.status).toBe(401);
	});
});
