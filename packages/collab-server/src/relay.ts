import { ENVELOPE_HEADER_LENGTH } from "@oh-my-pi/pi-wire";

/**
 * collab WebSocket relay：内容盲转发，严格实现客户端契约
 * （参照 packages/collab-web/scripts/local-relay.ts 与 docs/collab.md）：
 *
 * - `GET /r/<roomId>?role=host|guest` 升级为 WebSocket。
 * - host 先到创建房间；第二个 host 以 4009 拒绝；无房间的 guest 以 4004 拒绝；
 *   房间满员的 guest 以 4029 拒绝（客户端视为致命，不重连）。
 * - host 二进制帧：信封 peerId 0 广播给全部 guest，peerId N 单播——原样转发。
 * - guest 二进制帧：信封前 4 字节改写为发送者 peerId 后转给 host。
 * - TEXT 控制帧只由 relay 生成：`peer-joined`/`peer-left` → host，
 *   `room-closed` → guests（随后 close 4001，房间回收）。
 *
 * relay 永远接触不到明文：负载端到端 AES-256-GCM 密封。
 */
export const ROOM_PATH_RE = /^\/r\/([A-Za-z0-9_-]{10,64})$/;

export interface RelaySocketData {
	roomId: string;
	role: "host" | "guest";
	/** guest 在 open 时分配；host 恒为 0。 */
	peerId: number;
}

type RelaySocket = Bun.ServerWebSocket<RelaySocketData>;

interface Room {
	host: RelaySocket;
	guests: Map<number, RelaySocket>;
	nextPeerId: number;
	createdAt: number;
}

export interface RoomSnapshot {
	id: string;
	guests: number;
	createdAt: string;
}

export interface RelayMetrics {
	rooms: number;
	guests: number;
	roomsOpened: number;
	framesForwarded: number;
	bytesForwarded: number;
}

/** 信封头 peerId（uint32 BE）；不足 4 字节返回 null。 */
function readPeerId(data: Uint8Array): number | null {
	if (data.byteLength < ENVELOPE_HEADER_LENGTH) return null;
	return new DataView(data.buffer, data.byteOffset, ENVELOPE_HEADER_LENGTH).getUint32(0, false);
}

/** 原地改写信封 peerId，不拷贝负载。 */
function rewritePeerId(data: Uint8Array, peerId: number): void {
	new DataView(data.buffer, data.byteOffset, ENVELOPE_HEADER_LENGTH).setUint32(0, peerId, false);
}

export class RelayHub {
	#rooms = new Map<string, Room>();
	#maxGuests: number;
	#roomsOpened = 0;
	#framesForwarded = 0;
	#bytesForwarded = 0;

	constructor(maxGuests: number) {
		this.#maxGuests = maxGuests;
	}

	/** 传给 Bun.serve 的 websocket 处理器。 */
	readonly handlers: Bun.WebSocketHandler<RelaySocketData> = {
		open: (ws: RelaySocket): void => this.#open(ws),
		message: (ws: RelaySocket, message: string | Buffer): void => this.#message(ws, message),
		close: (ws: RelaySocket): void => this.#close(ws),
	};

	#open(ws: RelaySocket): void {
		const { roomId, role } = ws.data;
		if (role === "host") {
			if (this.#rooms.has(roomId)) {
				ws.close(4009, "a host is already connected for this room");
				return;
			}
			this.#rooms.set(roomId, { host: ws, guests: new Map(), nextPeerId: 1, createdAt: Date.now() });
			this.#roomsOpened++;
			return;
		}
		const room = this.#rooms.get(roomId);
		if (!room) {
			ws.close(4004, "no such room");
			return;
		}
		if (room.guests.size >= this.#maxGuests) {
			ws.close(4029, "room is full");
			return;
		}
		const peerId = room.nextPeerId++;
		ws.data.peerId = peerId;
		room.guests.set(peerId, ws);
		room.host.send(JSON.stringify({ t: "peer-joined", peer: peerId }));
	}

	#message(ws: RelaySocket, message: string | Buffer): void {
		if (typeof message === "string") return; // 客户端从不发 TEXT
		const room = this.#rooms.get(ws.data.roomId);
		if (!room) return;
		if (ws.data.role === "host") {
			const peerId = readPeerId(message);
			if (peerId === null) return;
			this.#framesForwarded++;
			this.#bytesForwarded += message.byteLength;
			if (peerId === 0) {
				for (const guest of room.guests.values()) guest.send(message);
			} else {
				room.guests.get(peerId)?.send(message);
			}
			return;
		}
		if (message.byteLength < ENVELOPE_HEADER_LENGTH) return;
		rewritePeerId(message, ws.data.peerId);
		this.#framesForwarded++;
		this.#bytesForwarded += message.byteLength;
		room.host.send(message);
	}

	#close(ws: RelaySocket): void {
		const { roomId, role, peerId } = ws.data;
		const room = this.#rooms.get(roomId);
		if (!room) return;
		if (role === "host") {
			// 被 4009 拒绝的第二个 host：活房间不归它拆。
			if (room.host !== ws) return;
			this.#rooms.delete(roomId);
			this.#closeGuests(room);
			return;
		}
		if (room.guests.delete(peerId)) {
			room.host.send(JSON.stringify({ t: "peer-left", peer: peerId }));
		}
	}

	#closeGuests(room: Room): void {
		const closure = JSON.stringify({ t: "room-closed" });
		for (const guest of room.guests.values()) {
			guest.send(closure);
			guest.close(4001, "room closed");
		}
		room.guests.clear();
	}

	metrics(): RelayMetrics {
		let guests = 0;
		for (const room of this.#rooms.values()) guests += room.guests.size;
		return {
			rooms: this.#rooms.size,
			guests,
			roomsOpened: this.#roomsOpened,
			framesForwarded: this.#framesForwarded,
			bytesForwarded: this.#bytesForwarded,
		};
	}

	listRooms(): RoomSnapshot[] {
		const rooms: RoomSnapshot[] = [];
		for (const [id, room] of this.#rooms) {
			rooms.push({ id, guests: room.guests.size, createdAt: new Date(room.createdAt).toISOString() });
		}
		return rooms;
	}

	/** 优雅停机：通知全部 guest、关闭 host 连接、清空房间表。 */
	closeAll(): void {
		for (const room of this.#rooms.values()) {
			this.#closeGuests(room);
			room.host.close(1001, "relay shutting down");
		}
		this.#rooms.clear();
	}
}
