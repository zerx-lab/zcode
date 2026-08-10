import { afterEach, describe, expect, it } from "bun:test";
import { packEnvelope, startHarness, type TestHarness, unpackEnvelope } from "./helpers";

const ROOM = "RelayRoom_12345";

let harness: TestHarness | null = null;

function h(): TestHarness {
	if (!harness) throw new Error("harness not started");
	return harness;
}

afterEach(() => {
	harness?.stop();
	harness = null;
});

describe("collab-server relay contract", () => {
	it("rejects upgrades without a valid role and guests before a host", async () => {
		harness = startHarness();
		const noRole = await fetch(`${h().httpUrl}/r/${ROOM}`);
		expect(noRole.status).toBe(404);
		const badRole = await fetch(`${h().httpUrl}/r/${ROOM}?role=admin`);
		expect(badRole.status).toBe(404);
		const noUpgrade = await fetch(`${h().httpUrl}/r/${ROOM}?role=host`);
		expect(noUpgrade.status).toBe(426);

		const orphan = h().socket(`/r/${ROOM}?role=guest`);
		const closed = await h().waitEvent<CloseEvent>(orphan, "close", "guest without room");
		expect(closed.code).toBe(4004);
	});

	it("rejects a second host with 4009 and keeps the live room intact", async () => {
		harness = startHarness();
		const host = h().socket(`/r/${ROOM}?role=host`);
		await h().waitOpen(host);

		const usurper = h().socket(`/r/${ROOM}?role=host`);
		const closed = await h().waitEvent<CloseEvent>(usurper, "close", "second host rejection");
		expect(closed.code).toBe(4009);

		// 原房间不受影响：guest 仍可加入
		const guest = h().socket(`/r/${ROOM}?role=guest`);
		await h().waitOpen(guest);
		expect(JSON.parse(await h().waitText(host, "peer join"))).toEqual({ t: "peer-joined", peer: 1 });
	});

	it("routes envelopes: guest peerId rewrite, host broadcast and unicast", async () => {
		harness = startHarness();
		const host = h().socket(`/r/${ROOM}?role=host`);
		await h().waitOpen(host);

		const guest1 = h().socket(`/r/${ROOM}?role=guest`);
		await h().waitOpen(guest1);
		expect(JSON.parse(await h().waitText(host, "first peer join"))).toEqual({ t: "peer-joined", peer: 1 });
		const guest2 = h().socket(`/r/${ROOM}?role=guest`);
		await h().waitOpen(guest2);
		expect(JSON.parse(await h().waitText(host, "second peer join"))).toEqual({ t: "peer-joined", peer: 2 });

		// guest → host：信封 peerId 被改写为发送者
		guest1.send(packEnvelope(0, new Uint8Array([1, 2, 3])));
		const fromGuest = unpackEnvelope(await h().waitBinary(host, "guest envelope"));
		expect(fromGuest.peerId).toBe(1);
		expect([...fromGuest.payload]).toEqual([1, 2, 3]);

		// host peerId 0 → 广播给全部 guest
		host.send(packEnvelope(0, new Uint8Array([9])));
		expect(unpackEnvelope(await h().waitBinary(guest1, "broadcast to guest1")).peerId).toBe(0);
		expect(unpackEnvelope(await h().waitBinary(guest2, "broadcast to guest2")).peerId).toBe(0);

		// host peerId 2 → 只给 guest2
		host.send(packEnvelope(2, new Uint8Array([7, 7])));
		const unicast = unpackEnvelope(await h().waitBinary(guest2, "unicast to guest2"));
		expect(unicast.peerId).toBe(2);
		expect([...unicast.payload]).toEqual([7, 7]);
		// guest1 静默：下一帧必须是超时（用短探测帧验证顺序性代价高，改为断言 guest1 无排队消息）
		await Bun.sleep(50);
		await expect(h().waitBinary(guest1, "unexpected unicast leak")).rejects.toThrow(/timed out/);
	}, 5_000);

	it("notifies peer-left on guest close and room-closed + 4001 on host close", async () => {
		harness = startHarness();
		const host = h().socket(`/r/${ROOM}?role=host`);
		await h().waitOpen(host);
		const guest1 = h().socket(`/r/${ROOM}?role=guest`);
		await h().waitOpen(guest1);
		await h().waitText(host, "peer join");
		const guest2 = h().socket(`/r/${ROOM}?role=guest`);
		await h().waitOpen(guest2);
		await h().waitText(host, "second peer join");

		guest1.close(1000);
		expect(JSON.parse(await h().waitText(host, "peer left"))).toEqual({ t: "peer-left", peer: 1 });

		const closure = h().waitText(guest2, "room close control");
		const guestClose = h().waitEvent<CloseEvent>(guest2, "close", "guest room close");
		host.close(1000);
		expect(JSON.parse(await closure)).toEqual({ t: "room-closed" });
		expect((await guestClose).code).toBe(4001);
	});

	it("rejects guests over the room cap with 4029", async () => {
		harness = startHarness({ maxGuests: 1 });
		const host = h().socket(`/r/${ROOM}?role=host`);
		await h().waitOpen(host);
		const guest1 = h().socket(`/r/${ROOM}?role=guest`);
		await h().waitOpen(guest1);
		await h().waitText(host, "peer join");

		const overflow = h().socket(`/r/${ROOM}?role=guest`);
		const closed = await h().waitEvent<CloseEvent>(overflow, "close", "room full rejection");
		expect(closed.code).toBe(4029);
	});
});
