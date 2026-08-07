import { describe, expect, it } from "bun:test";
import { CONFIG_DIR_NAME, parseArgs } from "@oh-my-pi/pi-coding-agent/extensibility/legacy-pi-coding-agent-shim";
import { CONFIG_DIR_NAME as UTILS_CONFIG_DIR_NAME } from "@oh-my-pi/pi-utils";

describe("legacy shim CLI exports", () => {
	it("re-exports parseArgs and CONFIG_DIR_NAME from the legacy package root", () => {
		expect(CONFIG_DIR_NAME).toBe(UTILS_CONFIG_DIR_NAME);
		expect(parseArgs(["hello"]).messages).toEqual(["hello"]);
	});
});
