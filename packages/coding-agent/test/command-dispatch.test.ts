/**
 * zcode fork: frontmatter dispatch contract for file-based slash commands.
 *
 * Contract under test: which frontmatter shapes produce a dispatch spec, which
 * execution mode a spec selects (opencode-compatible `agent`/`model`/`subtask`
 * semantics), and what a subagent dispatch actually does — it issues the `task`
 * call itself instead of asking the model to issue it.
 */

import { describe, expect, test } from "bun:test";
import * as fs from "node:fs/promises";
import * as os from "node:os";
import * as path from "node:path";
import type { AgentMessage, AgentTool, AgentToolResult } from "@oh-my-pi/pi-agent-core";
import type { AssistantMessage, Model, ToolResultMessage } from "@oh-my-pi/pi-ai";
import { Settings } from "../src/config/settings";
import {
	commandDispatchMode,
	expandAndDispatchSlashCommand,
	parseCommandDispatchSpec,
} from "../src/extensibility/command-dispatch";
import { runSubagentDispatch } from "../src/extensibility/command-subagent-dispatch";
import { type FileSlashCommand, loadSlashCommands } from "../src/extensibility/slash-commands";
import type { AgentSession } from "../src/session/agent-session";

function md(frontmatter: string): string {
	return `---\n${frontmatter}\n---\nBody text $ARGUMENTS\n`;
}

describe("parseCommandDispatchSpec", () => {
	test("parses agent, model, and subtask fields", () => {
		const spec = parseCommandDispatchSpec(md('agent: reviewer\nmodel: "@advisor"\nsubtask: true'));
		expect(spec).toMatchObject({ agent: "reviewer", model: "@advisor", subtask: true });
	});

	test("returns undefined without frontmatter or without dispatch fields", () => {
		expect(parseCommandDispatchSpec("Just a body\n")).toBeUndefined();
		expect(parseCommandDispatchSpec(md("description: greet the user"))).toBeUndefined();
	});

	test("subtask: false alone is a no-op", () => {
		expect(parseCommandDispatchSpec(md("subtask: false"))).toBeUndefined();
	});

	test("subtask: true alone forces a spec", () => {
		expect(parseCommandDispatchSpec(md("subtask: true"))).toMatchObject({ subtask: true });
	});

	test("trims values and drops empty or non-string fields", () => {
		const spec = parseCommandDispatchSpec(md('agent: " scout "\nmodel: ""'));
		expect(spec).toMatchObject({ agent: "scout", model: undefined, subtask: undefined });
		expect(parseCommandDispatchSpec(md("agent: 42"))).toBeUndefined();
	});

	test("parses the per-spawn task fields alongside a dispatch directive", () => {
		const spec = parseCommandDispatchSpec(
			md("agent: reviewer\nname: Audit\nschemaMode: strict\nisolated: true\neffort: hi"),
		);
		expect(spec).toMatchObject({ name: "Audit", schemaMode: "strict", isolated: true, effort: "hi" });
	});

	test("rejects out-of-range effort and schemaMode values", () => {
		const spec = parseCommandDispatchSpec(md("agent: reviewer\neffort: extreme\nschemaMode: loose"));
		expect(spec).toMatchObject({ agent: "reviewer", effort: undefined, schemaMode: undefined });
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

interface RecordedEvent {
	type: string;
	role?: string;
	toolCallId?: string;
	args?: unknown;
}

interface StubSessionHarness {
	session: AgentSession;
	settings: Settings;
	notices: string[];
	modelSwitches: Array<{ model: Model; thinkingLevel: unknown; ephemeral: boolean | undefined }>;
	events: RecordedEvent[];
	messages: AgentMessage[];
	/** `task.agentModelOverrides` snapshot taken inside `execute`. */
	overridesDuringExecute: Record<string, string> | undefined;
	executeCalls: Array<{ toolCallId: string; params: unknown; signal: AbortSignal | undefined }>;
	continueCalls: number;
	abortRegistrations: AbortController[];
	fireAgentEnd(): void;
}

const FAKE_MODEL = { provider: "fake", id: "fake-model", name: "Fake Model" } as unknown as Model;
const CURRENT_MODEL = {
	provider: "fake",
	id: "current-model",
	name: "Current Model",
	api: "openai-completions",
} as unknown as Model;

function makeStubSession(options?: {
	streaming?: boolean;
	model?: Model | undefined;
	taskTool?: "present" | "absent";
	taskResult?: AgentToolResult<unknown> | (() => Promise<AgentToolResult<unknown>>);
}): StubSessionHarness {
	const settings = Settings.isolated();
	const notices: string[] = [];
	const modelSwitches: StubSessionHarness["modelSwitches"] = [];
	const listeners: Array<(event: { type: string }) => void> = [];
	const events: RecordedEvent[] = [];
	const messages: AgentMessage[] = [];
	const executeCalls: StubSessionHarness["executeCalls"] = [];
	const abortRegistrations: AbortController[] = [];
	const harness = {
		settings,
		notices,
		modelSwitches,
		events,
		messages,
		overridesDuringExecute: undefined as Record<string, string> | undefined,
		executeCalls,
		continueCalls: 0,
		abortRegistrations,
	};

	const taskTool = {
		name: "task",
		execute: async (toolCallId: string, params: unknown, signal?: AbortSignal) => {
			executeCalls.push({ toolCallId, params, signal });
			harness.overridesDuringExecute = { ...settings.get("task.agentModelOverrides") };
			const configured = options?.taskResult;
			if (typeof configured === "function") return await configured();
			return configured ?? { content: [{ type: "text", text: "subagent output" }] };
		},
	} as unknown as AgentTool;

	const session = {
		isStreaming: options?.streaming ?? false,
		settings,
		modelRegistry: { getAvailable: () => [FAKE_MODEL, CURRENT_MODEL] },
		model: options && "model" in options ? options.model : CURRENT_MODEL,
		configuredThinkingLevel: () => undefined,
		emitNotice: (_level: string, message: string) => {
			notices.push(message);
		},
		getToolByName: (name: string) => (options?.taskTool === "absent" || name !== "task" ? undefined : taskTool),
		registerPreModelAbort: (controller: AbortController) => {
			abortRegistrations.push(controller);
			return () => {};
		},
		agent: {
			emitExternalEvent: (event: { type: string; message?: AgentMessage; toolCallId?: string; args?: unknown }) => {
				events.push({
					type: event.type,
					role: event.message?.role,
					toolCallId: event.toolCallId,
					args: event.args,
				});
				if (event.type === "message_end" && event.message) messages.push(event.message);
			},
			continue: async () => {
				harness.continueCalls++;
			},
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

	return Object.assign(harness, {
		session,
		fireAgentEnd: () => {
			for (const listener of [...listeners]) listener({ type: "agent_end" });
		},
	});
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

function userMessage(text: string): AgentMessage {
	return { role: "user", content: [{ type: "text", text }], timestamp: Date.now() };
}

describe("expandAndDispatchSlashCommand", () => {
	test("command without dispatch spec expands as plain text with no side effects", async () => {
		const harness = makeStubSession();
		const outcome = await expandAndDispatchSlashCommand(harness.session, "/probe foo.ts", [makeCommand({})]);
		expect(outcome.text).toContain("Review foo.ts carefully.");
		expect(outcome.dispatch).toBeUndefined();
		expect(harness.modelSwitches).toEqual([]);
	});

	test("subagent mode prepares a flat task call carrying the body verbatim", async () => {
		const harness = makeStubSession();
		const command = makeCommand({ dispatch: { agent: "reviewer" } });
		const outcome = await expandAndDispatchSlashCommand(harness.session, "/probe foo.ts", [command]);
		expect(outcome.dispatch).toEqual({
			label: "/probe",
			agentName: "reviewer",
			args: { agent: "reviewer", task: "Review foo.ts carefully." },
			modelOverride: undefined,
		});
	});

	test("subtask without agent targets the general-purpose task agent", async () => {
		const harness = makeStubSession();
		const command = makeCommand({ dispatch: { subtask: true } });
		const outcome = await expandAndDispatchSlashCommand(harness.session, "/probe x", [command]);
		expect(outcome.dispatch?.agentName).toBe("task");
		expect(outcome.dispatch?.args).toEqual({ agent: "task", task: "Review x carefully." });
	});

	test("per-spawn fields ride the call; unset ones stay absent", async () => {
		const harness = makeStubSession();
		const command = makeCommand({
			dispatch: { agent: "reviewer", name: "Audit", schemaMode: "strict", isolated: true, effort: "hi" },
		});
		const outcome = await expandAndDispatchSlashCommand(harness.session, "/probe x", [command]);
		expect(outcome.dispatch?.args).toEqual({
			agent: "reviewer",
			task: "Review x carefully.",
			name: "Audit",
			schemaMode: "strict",
			isolated: true,
			effort: "hi",
		});
	});

	test("model rides the prepared call instead of a settings override at prepare time", async () => {
		const harness = makeStubSession();
		const command = makeCommand({ dispatch: { agent: "reviewer", model: "@advisor" } });
		const outcome = await expandAndDispatchSlashCommand(harness.session, "/probe x", [command]);
		expect(outcome.dispatch?.modelOverride).toBe("@advisor");
		expect(harness.settings.get("task.agentModelOverrides")).toEqual({});
	});

	test("an unavailable task tool rejects the command instead of running it in the main agent", async () => {
		const harness = makeStubSession({ taskTool: "absent" });
		const command = makeCommand({ dispatch: { agent: "reviewer" } });
		const outcome = await expandAndDispatchSlashCommand(harness.session, "/probe x", [command]);
		expect(outcome.text).toBeUndefined();
		expect(outcome.dispatch).toBeUndefined();
		expect(harness.notices.some(notice => notice.includes("task tool is not available"))).toBe(true);
	});

	test("session mode switches to the role-aliased model ephemerally and restores on agent_end", async () => {
		const harness = makeStubSession();
		harness.settings.setModelRole("advisor", "fake/fake-model");
		const command = makeCommand({ dispatch: { model: "@advisor" } });
		const outcome = await expandAndDispatchSlashCommand(harness.session, "/probe y", [command]);
		expect(outcome.text).toContain("Review y carefully.");
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
		const outcome = await expandAndDispatchSlashCommand(harness.session, "/probe z", [command]);
		expect(outcome.text).toContain("Review z carefully.");
		expect(harness.modelSwitches).toEqual([]);
		expect(harness.notices.some(notice => notice.includes("no-such/model"))).toBe(true);
	});

	test("streaming session skips dispatch and queues the plain expansion", async () => {
		const harness = makeStubSession({ streaming: true });
		const command = makeCommand({ dispatch: { agent: "reviewer" } });
		const outcome = await expandAndDispatchSlashCommand(harness.session, "/probe s", [command]);
		expect(outcome.text).toContain("Review s carefully.");
		expect(outcome.dispatch).toBeUndefined();
		expect(harness.notices.some(notice => notice.includes("streaming"))).toBe(true);
	});
});

describe("runSubagentDispatch", () => {
	const prepared = {
		label: "/probe",
		agentName: "reviewer",
		args: { agent: "reviewer", task: "Review foo.ts carefully." },
	};

	test("commits the turn, issues the call itself, and resumes the loop for the relay", async () => {
		const harness = makeStubSession();
		await runSubagentDispatch(harness.session, prepared, [userMessage("/probe foo.ts")]);

		expect(harness.events.map(event => event.type)).toEqual([
			"message_start",
			"message_end",
			"message_start",
			"message_end",
			"tool_execution_start",
			"tool_execution_end",
			"message_start",
			"message_end",
		]);
		expect(harness.messages.map(message => message.role)).toEqual(["user", "assistant", "toolResult"]);

		const assistant = harness.messages[1] as AssistantMessage;
		const call = assistant.content[0];
		expect(call).toMatchObject({ type: "toolCall", name: "task", arguments: prepared.args });
		expect(assistant.stopReason).toBe("toolUse");
		expect(assistant.usage.cost.total).toBe(0);

		expect(harness.executeCalls).toHaveLength(1);
		expect(harness.executeCalls[0].params).toEqual(prepared.args);

		const result = harness.messages[2] as ToolResultMessage;
		expect(result.toolCallId).toBe(harness.executeCalls[0].toolCallId);
		expect(result.isError).toBe(false);
		expect(result.content).toEqual([{ type: "text", text: "subagent output" }]);

		expect(harness.continueCalls).toBe(1);
	});

	test("holds the per-spawn model override only across the call", async () => {
		const harness = makeStubSession();
		await runSubagentDispatch(harness.session, { ...prepared, modelOverride: "@advisor" }, []);
		expect(harness.overridesDuringExecute).toEqual({ reviewer: "@advisor" });
		expect(harness.settings.get("task.agentModelOverrides")).toEqual({});
	});

	test("a throwing tool becomes an error result rather than killing the turn", async () => {
		const harness = makeStubSession({
			taskResult: async () => {
				throw new Error("spawn policy refused");
			},
		});
		await runSubagentDispatch(harness.session, prepared, []);
		const result = harness.messages.at(-1) as ToolResultMessage;
		expect(result.isError).toBe(true);
		expect(result.content).toEqual([{ type: "text", text: "The reviewer subagent failed: spawn policy refused" }]);
		expect(harness.continueCalls).toBe(1);
	});

	test("an interrupt still records the result but spends no model call narrating it", async () => {
		const harness = makeStubSession({
			taskResult: async () => {
				harness.abortRegistrations[0].abort();
				return { content: [{ type: "text", text: "partial" }] };
			},
		});
		await runSubagentDispatch(harness.session, prepared, []);
		expect(harness.messages.map(message => message.role)).toEqual(["assistant", "toolResult"]);
		expect(harness.continueCalls).toBe(0);
	});

	test("the executing tool receives the registered interrupt signal", async () => {
		const harness = makeStubSession();
		await runSubagentDispatch(harness.session, prepared, []);
		expect(harness.abortRegistrations).toHaveLength(1);
		expect(harness.executeCalls[0].signal).toBe(harness.abortRegistrations[0].signal);
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
			expect(command?.dispatch).toMatchObject({ agent: "reviewer", model: "@advisor", subtask: undefined });
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
