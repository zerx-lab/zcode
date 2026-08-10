/**
 * zcode fork: frontmatter dispatch contract for file-based slash commands.
 *
 * Contract under test: which frontmatter shapes produce a dispatch spec, and
 * which execution mode a spec selects (opencode-compatible `agent`/`model`/
 * `subtask` semantics).
 */

import { describe, expect, test } from "bun:test";
import * as fs from "node:fs/promises";
import * as os from "node:os";
import * as path from "node:path";
import type { Model } from "@oh-my-pi/pi-ai";
import { Settings } from "../src/config/settings";
import {
	commandDispatchMode,
	expandAndDispatchSlashCommand,
	parseCommandDispatchSpec,
} from "../src/extensibility/command-dispatch";
import { type FileSlashCommand, loadSlashCommands } from "../src/extensibility/slash-commands";
import type { AgentSession } from "../src/session/agent-session";

function md(frontmatter: string): string {
	return `---\n${frontmatter}\n---\nBody text $ARGUMENTS\n`;
}

describe("parseCommandDispatchSpec", () => {
	test("parses agent, model, and subtask fields", () => {
		const spec = parseCommandDispatchSpec(md('agent: reviewer\nmodel: "@advisor"\nsubtask: true'));
		expect(spec).toEqual({ agent: "reviewer", model: "@advisor", subtask: true });
	});

	test("returns undefined without frontmatter or without dispatch fields", () => {
		expect(parseCommandDispatchSpec("Just a body\n")).toBeUndefined();
		expect(parseCommandDispatchSpec(md("description: greet the user"))).toBeUndefined();
	});

	test("subtask: false alone is a no-op", () => {
		expect(parseCommandDispatchSpec(md("subtask: false"))).toBeUndefined();
	});

	test("subtask: true alone forces a spec", () => {
		expect(parseCommandDispatchSpec(md("subtask: true"))).toEqual({
			agent: undefined,
			model: undefined,
			subtask: true,
		});
	});

	test("trims values and drops empty or non-string fields", () => {
		const spec = parseCommandDispatchSpec(md('agent: " scout "\nmodel: ""'));
		expect(spec).toEqual({ agent: "scout", model: undefined, subtask: undefined });
		expect(parseCommandDispatchSpec(md("agent: 42"))).toBeUndefined();
	});
});

describe("commandDispatchMode", () => {
	test("agent implies subagent mode", () => {
		expect(commandDispatchMode({ agent: "reviewer" })).toBe("subagent");
	});

	test("subtask: false overrides agent back to session mode", () => {
		expect(commandDispatchMode({ agent: "reviewer", subtask: false })).toBe("session");
	});

	test("model alone runs in the current session", () => {
		expect(commandDispatchMode({ model: "@advisor" })).toBe("session");
	});

	test("subtask: true forces subagent mode even without agent", () => {
		expect(commandDispatchMode({ model: "@advisor", subtask: true })).toBe("subagent");
	});
});

interface StubSessionHarness {
	session: AgentSession;
	settings: Settings;
	notices: string[];
	forcedTools: string[];
	modelSwitches: Array<{ model: Model; thinkingLevel: unknown; ephemeral: boolean | undefined }>;
	fireAgentEnd(): void;
}

const FAKE_MODEL = { provider: "fake", id: "fake-model", name: "Fake Model" } as unknown as Model;
const CURRENT_MODEL = { provider: "fake", id: "current-model", name: "Current Model" } as unknown as Model;

function makeStubSession(options?: { streaming?: boolean; model?: Model | undefined }): StubSessionHarness {
	const settings = Settings.isolated();
	const notices: string[] = [];
	const forcedTools: string[] = [];
	const modelSwitches: StubSessionHarness["modelSwitches"] = [];
	const listeners: Array<(event: { type: string }) => void> = [];
	const session = {
		isStreaming: options?.streaming ?? false,
		settings,
		modelRegistry: { getAvailable: () => [FAKE_MODEL, CURRENT_MODEL] },
		model: options && "model" in options ? options.model : CURRENT_MODEL,
		configuredThinkingLevel: () => undefined,
		emitNotice: (_level: string, message: string) => {
			notices.push(message);
		},
		setForcedToolChoice: (name: string) => {
			forcedTools.push(name);
		},
		setModelTemporary: async (model: Model, thinkingLevel: unknown, opts?: { ephemeral?: boolean }) => {
			modelSwitches.push({ model, thinkingLevel, ephemeral: opts?.ephemeral });
		},
		subscribe: (listener: (event: { type: string }) => void) => {
			listeners.push(listener);
			return () => {
				const index = listeners.indexOf(listener);
				if (index !== -1) listeners.splice(index, 1);
			};
		},
	} as unknown as AgentSession;
	return {
		session,
		settings,
		notices,
		forcedTools,
		modelSwitches,
		fireAgentEnd: () => {
			for (const listener of [...listeners]) listener({ type: "agent_end" });
		},
	};
}

function makeCommand(overrides: Partial<FileSlashCommand>): FileSlashCommand {
	return {
		name: "probe",
		description: "probe",
		content: "Review $ARGUMENTS carefully.",
		source: "bundled",
		...overrides,
	};
}

describe("expandAndDispatchSlashCommand", () => {
	test("command without dispatch spec expands as plain text with no side effects", async () => {
		const harness = makeStubSession();
		const result = await expandAndDispatchSlashCommand(harness.session, "/probe foo.ts", [makeCommand({})]);
		expect(result).toContain("Review foo.ts carefully.");
		expect(harness.forcedTools).toEqual([]);
		expect(harness.modelSwitches).toEqual([]);
	});

	test("subagent mode forces one task call and wraps the body verbatim", async () => {
		const harness = makeStubSession();
		const command = makeCommand({ dispatch: { agent: "reviewer" } });
		const result = await expandAndDispatchSlashCommand(harness.session, "/probe foo.ts", [command]);
		expect(harness.forcedTools).toEqual(["task"]);
		expect(result).toContain('"reviewer"');
		expect(result).toContain("Review foo.ts carefully.");
	});

	test("subagent mode with model routes through task.agentModelOverrides and restores on agent_end", async () => {
		const harness = makeStubSession();
		const command = makeCommand({ dispatch: { agent: "reviewer", model: "@advisor" } });
		await expandAndDispatchSlashCommand(harness.session, "/probe x", [command]);
		expect(harness.settings.get("task.agentModelOverrides")).toEqual({ reviewer: "@advisor" });
		harness.fireAgentEnd();
		expect(harness.settings.get("task.agentModelOverrides")).toEqual({});
	});

	test("session mode switches to the role-aliased model ephemerally and restores on agent_end", async () => {
		const harness = makeStubSession();
		harness.settings.setModelRole("advisor", "fake/fake-model");
		const command = makeCommand({ dispatch: { model: "@advisor" } });
		const result = await expandAndDispatchSlashCommand(harness.session, "/probe y", [command]);
		expect(result).toContain("Review y carefully.");
		expect(harness.modelSwitches).toHaveLength(1);
		expect(harness.modelSwitches[0].model).toBe(FAKE_MODEL);
		expect(harness.modelSwitches[0].ephemeral).toBe(true);
		harness.fireAgentEnd();
		expect(harness.modelSwitches).toHaveLength(2);
		expect(harness.modelSwitches[1].model).toBe(CURRENT_MODEL);
		harness.fireAgentEnd();
		expect(harness.modelSwitches).toHaveLength(2);
	});

	test("session mode with unresolvable model warns and keeps the current model", async () => {
		const harness = makeStubSession();
		const command = makeCommand({ dispatch: { model: "no-such/model" } });
		const result = await expandAndDispatchSlashCommand(harness.session, "/probe z", [command]);
		expect(result).toContain("Review z carefully.");
		expect(harness.modelSwitches).toEqual([]);
		expect(harness.notices.some(notice => notice.includes("no-such/model"))).toBe(true);
	});

	test("streaming session skips dispatch and queues the plain expansion", async () => {
		const harness = makeStubSession({ streaming: true });
		const command = makeCommand({ dispatch: { agent: "reviewer" } });
		const result = await expandAndDispatchSlashCommand(harness.session, "/probe s", [command]);
		expect(result).toContain("Review s carefully.");
		expect(result).not.toContain('"reviewer"');
		expect(harness.forcedTools).toEqual([]);
		expect(harness.notices.some(notice => notice.includes("streaming"))).toBe(true);
	});
});

describe("loadSlashCommands dispatch wiring", () => {
	test("attaches the dispatch spec parsed from a project command file", async () => {
		const dir = await fs.mkdtemp(path.join(os.tmpdir(), "cmd-dispatch-"));
		try {
			await Bun.write(
				path.join(dir, ".omp", "commands", "dispatchy.md"),
				'---\ndescription: probe command\nagent: reviewer\nmodel: "@advisor"\n---\nDo the thing $ARGUMENTS\n',
			);
			const commands = await loadSlashCommands({ cwd: dir });
			const command = commands.find(cmd => cmd.name === "dispatchy");
			expect(command?.dispatch).toEqual({ agent: "reviewer", model: "@advisor", subtask: undefined });
			expect(command?.content).toContain("Do the thing");
		} finally {
			await fs.rm(dir, { recursive: true, force: true });
		}
	});
});

describe("project command discovery from a subdirectory", () => {
	test("walks up to the repo root but not past it", async () => {
		const base = await fs.mkdtemp(path.join(os.tmpdir(), "cmd-walk-"));
		try {
			await Bun.write(path.join(base, "above", ".omp", "commands", "outside.md"), "Outside command\n");
			await fs.mkdir(path.join(base, "above", "repo", ".git"), { recursive: true });
			await Bun.write(path.join(base, "above", "repo", ".omp", "commands", "rooted.md"), "Rooted command\n");
			const subdir = path.join(base, "above", "repo", "packages", "sub");
			await fs.mkdir(subdir, { recursive: true });

			const commands = await loadSlashCommands({ cwd: subdir });
			const names = new Set(commands.map(cmd => cmd.name));
			expect(names.has("rooted")).toBe(true);
			expect(names.has("outside")).toBe(false);
		} finally {
			await fs.rm(base, { recursive: true, force: true });
		}
	});
});
