import { describe, expect, it } from "bun:test";
import * as fs from "node:fs";
import * as path from "node:path";
import { type EmbeddedManifest, materializeEmbeddedPublic, setEmbeddedPublicArchive } from "../src/embedded";

function packArchive(manifest: EmbeddedManifest): string {
	return Buffer.from(Bun.gzipSync(new TextEncoder().encode(JSON.stringify(manifest)))).toString("base64");
}

describe("embedded public archive", () => {
	it("returns null when no archive is registered", () => {
		// 模块级状态默认未注册（本文件在独立进程/文件作用域内先于注册断言）
		expect(materializeEmbeddedPublic()).toBeNull();
	});

	it("materializes the manifest tree to tmp and reuses the completed extraction", () => {
		const marker = `probe-${Date.now()}`;
		setEmbeddedPublicArchive(
			packArchive({
				"web/index.html": Buffer.from(`<html>${marker}</html>`).toString("base64"),
				"dash/assets/main.js": Buffer.from("console.log(1)").toString("base64"),
				"share-viewer.html": Buffer.from("viewer").toString("base64"),
			}),
		);
		const dir = materializeEmbeddedPublic();
		expect(dir).not.toBeNull();
		if (!dir) throw new Error("unreachable");
		expect(fs.readFileSync(path.join(dir, "web/index.html"), "utf8")).toBe(`<html>${marker}</html>`);
		expect(fs.readFileSync(path.join(dir, "dash/assets/main.js"), "utf8")).toBe("console.log(1)");
		expect(fs.readFileSync(path.join(dir, "share-viewer.html"), "utf8")).toBe("viewer");

		// 复用：改写一个文件后再次 materialize 不重释放（.complete 命中）
		fs.writeFileSync(path.join(dir, "share-viewer.html"), "mutated");
		expect(materializeEmbeddedPublic()).toBe(dir);
		expect(fs.readFileSync(path.join(dir, "share-viewer.html"), "utf8")).toBe("mutated");

		fs.rmSync(dir, { recursive: true, force: true });
	});
});
