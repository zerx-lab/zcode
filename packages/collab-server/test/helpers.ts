import type { ServerConfig } from "../src/config";
import { type CollabServer, startServer } from "../src/index";

export const REQUEST_TIMEOUT_MS = 1_000;

/** 测试基线配置：内存库、无静态资源、随机端口。 */
export function testConfig(overrides: Partial<ServerConfig> = {}): ServerConfig {
	return {
		port: 0,
		hostname: "127.0.0.1",
		dataDir: ":memory:",
		adminUser: "admin",
		adminPassword: null,
		shareTtlDays: 0,
		maxGuests: 32,
		shareMaxBytes: 1_000_000,
		webDir: null,
		dashDir: null,
		shareViewerPath: null,
		...overrides,
	};
}

export interface TestHarness {
	server: CollabServer;
	httpUrl: string;
	wsUrl: string;
	socket(path: string): WebSocket;
	nextMessage(ws: WebSocket, label: string): Promise<MessageEvent>;
	waitText(ws: WebSocket, label: string): Promise<string>;
	waitBinary(ws: WebSocket, label: string): Promise<Uint8Array>;
	waitOpen(ws: WebSocket): Promise<Event>;
	waitEvent<T extends Event>(ws: WebSocket, type: string, label: string): Promise<T>;
	stop(): void;
}

interface Inbox {
	queue: MessageEvent[];
	waiters: Array<(event: MessageEvent) => void>;
}

export function startHarness(overrides: Partial<ServerConfig> = {}): TestHarness {
	const server = startServer(testConfig(overrides));
	const httpUrl = server.url;
	const wsUrl = httpUrl.replace(/^http:/, "ws:");
	const sockets: WebSocket[] = [];
	const inboxes = new Map<WebSocket, Inbox>();

	function socket(path: string): WebSocket {
		const ws = new WebSocket(`${wsUrl}${path}`);
		ws.binaryType = "arraybuffer";
		const inbox: Inbox = { queue: [], waiters: [] };
		inboxes.set(ws, inbox);
		ws.addEventListener("message", event => {
			const waiter = inbox.waiters.shift();
			if (waiter) waiter(event as MessageEvent);
			else inbox.queue.push(event as MessageEvent);
		});
		sockets.push(ws);
		return ws;
	}

	function nextMessage(ws: WebSocket, label: string): Promise<MessageEvent> {
		const inbox = inboxes.get(ws);
		if (!inbox) throw new Error("socket not created via harness.socket()");
		const queued = inbox.queue.shift();
		if (queued) return Promise.resolve(queued);
		const { promise, resolve, reject } = Promise.withResolvers<MessageEvent>();
		const onEvent = (event: MessageEvent): void => {
			clearTimeout(timer);
			resolve(event);
		};
		const timer = setTimeout(() => {
			const idx = inbox.waiters.indexOf(onEvent);
			if (idx !== -1) inbox.waiters.splice(idx, 1);
			reject(new Error(`timed out waiting for ${label}`));
		}, REQUEST_TIMEOUT_MS);
		inbox.waiters.push(onEvent);
		return promise;
	}

	function waitEvent<T extends Event>(ws: WebSocket, type: string, label: string): Promise<T> {
		const { promise, resolve, reject } = Promise.withResolvers<T>();
		let timer: Timer | undefined;
		const cleanup = (): void => {
			ws.removeEventListener(type, onEvent);
			if (timer !== undefined) clearTimeout(timer);
		};
		const onEvent = (event: Event): void => {
			cleanup();
			resolve(event as T);
		};
		timer = setTimeout(() => {
			cleanup();
			reject(new Error(`timed out waiting for ${label}`));
		}, REQUEST_TIMEOUT_MS);
		ws.addEventListener(type, onEvent);
		return promise;
	}

	return {
		server,
		httpUrl,
		wsUrl,
		socket,
		nextMessage,
		waitEvent,
		waitOpen(ws: WebSocket): Promise<Event> {
			if (ws.readyState === WebSocket.OPEN) return Promise.resolve(new Event("open"));
			return waitEvent(ws, "open", "socket open");
		},
		async waitText(ws: WebSocket, label: string): Promise<string> {
			const event = await nextMessage(ws, label);
			if (typeof event.data !== "string") throw new Error(`${label} was not TEXT`);
			return event.data;
		},
		async waitBinary(ws: WebSocket, label: string): Promise<Uint8Array> {
			const event = await nextMessage(ws, label);
			const data: unknown = event.data;
			if (data instanceof ArrayBuffer) return new Uint8Array(data);
			if (ArrayBuffer.isView(data)) return new Uint8Array(data.buffer, data.byteOffset, data.byteLength);
			throw new Error(`${label} was not binary`);
		},
		stop(): void {
			for (const ws of sockets.splice(0)) {
				if (ws.readyState === WebSocket.CONNECTING || ws.readyState === WebSocket.OPEN) ws.close(1000);
			}
			inboxes.clear();
			server.stop();
		},
	};
}

export function packEnvelope(peerId: number, payload: Uint8Array): Uint8Array {
	const out = new Uint8Array(4 + payload.byteLength);
	new DataView(out.buffer).setUint32(0, peerId, false);
	out.set(payload, 4);
	return out;
}

export function unpackEnvelope(data: Uint8Array): { peerId: number; payload: Uint8Array } {
	return {
		peerId: new DataView(data.buffer, data.byteOffset, 4).getUint32(0, false),
		payload: data.subarray(4),
	};
}
