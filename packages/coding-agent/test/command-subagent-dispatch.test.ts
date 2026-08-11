/**
 * zcode fork: end-to-end contract for `subtask:` slash commands.
 *
 * A dispatching command must reach the subagent without a model round trip:
 * the `task` call carries the expanded body verbatim, the provider is called
 * exactly once (the relay), and that single call already sees the finished
 * tool interaction in its context.
 */

import { afterEach, beforeEach, expect, it } from "bun:test";
import * as path from "node:path";
import { type } from "@oh-my-pi/omptype";
import { Agent, type AgentTool } from "@oh-my-pi/pi-agent-core";
import type { AssistantMessage, ToolResultMessage } from "@oh-my-pi/pi-ai";
import { createMockModel, type MockModel } from "@oh-my-pi/pi-ai/providers/mock";
import { getBundledModel } from "@oh-my-pi/pi-catalog/models";
import { ModelRegistry } from "@oh-my-pi/pi-coding-agent/config/model-registry";
import { Settings } from "@oh-my-pi/pi-coding-agent/config/settings";
import type { FileSlashCommand } from "@oh-my-pi/pi-coding-agent/extensibility/slash-commands";
import { AgentSession } from "@oh-my-pi/pi-coding-agent/session/agent-session";
import { AuthStorage } from "@oh-my-pi/pi-coding-agent/session/auth-storage";
import { convertToLlm } from "@oh-my-pi/pi-coding-agent/session/messages";
import { SessionManager } from "@oh-my-pi/pi-coding-agent/session/session-manager";
import { TempDir } from "@oh-my-pi/pi-utils";

let tempDir: TempDir;
let authStorage: AuthStorage | undefined;
let session: AgentSession;
let mock: MockModel;
let taskCalls: Array<Record<string, unknown>>;

const REVIEW_COMMAND: FileSlashCommand = {
	name: "review",
	description: "review a file",
	content: "Review $ARGUMENTS against the checklist.",
	source: "test",
	dispatch: { agent: "reviewer", subtask: true },
};

beforeEach(async () => {
	tempDir = TempDir.createSync("@pi-command-subtask-");
	const model = getBundledModel("anthropic", "claude-sonnet-4-5");
	if (!model) throw new Error("Expected claude-sonnet-4-5 model to exist");

	authStorage = await AuthStorage.create(path.join(tempDir.path(), "testauth.db"));
	authStorage.setRuntimeApiKey("anthropic", "test-key");
	const modelRegistry = new ModelRegistry(authStorage, path.join(tempDir.path(), "models.yml"));
	const settings = Settings.isolated({ "compaction.enabled": false });

	taskCalls = [];
	const taskTool: AgentTool = {
		name: "task",
		label: "Task",
		description: "Mock task tool",
		parameters: type("object"),
		execute: async (_toolCallId, params) => {
			taskCalls.push(params as Record<string, unknown>);
			return { content: [{ type: "text" as const, text: "reviewer verdict: ship it" }] };
		},
	};

	mock = createMockModel({ handler: () => ({ content: ["relayed"] }) });

	const agent = new Agent({
		getToolChoice: () => session.nextToolChoiceDirective(),
		getApiKey: () => "test-key",
		initialState: { model, systemPrompt: ["Test"], tools: [taskTool], messages: [] },
		convertToLlm,
		streamFn: mock.stream,
	});

	session = new AgentSession({
		agent,
		sessionManager: SessionManager.inMemory(tempDir.path()),
		settings,
		modelRegistry,
		toolRegistry: new Map([[taskTool.name, taskTool]]),
		slashCommands: [REVIEW_COMMAND],
	});
});

afterEach(async () => {
	await session.dispose();
	authStorage?.close();
	authStorage = undefined;
	tempDir.removeSync();
});

it("hands the expanded body to the subagent and spends one model call relaying the result", async () => {
	await session.prompt("/review foo.ts");

	expect(taskCalls).toEqual([{ agent: "reviewer", task: "Review foo.ts against the checklist." }]);
	expect(mock.calls).toHaveLength(1);

	const roles = session.messages.map(message => message.role);
	expect(roles).toEqual(["user", "assistant", "toolResult", "assistant"]);

	const call = (session.messages[1] as AssistantMessage).content[0];
	expect(call).toMatchObject({ type: "toolCall", name: "task" });
	const result = session.messages[2] as ToolResultMessage;
	expect(result.isError).toBe(false);
	expect(result.content).toEqual([{ type: "text", text: "reviewer verdict: ship it" }]);

	// The single provider request must already carry the finished interaction —
	// that is what makes it a relay rather than the call-issuing turn.
	const relayed = mock.calls[0].context.messages.map(message => message.role);
	expect(relayed).toEqual(["user", "assistant", "toolResult"]);
});

it("leaves a non-dispatching command on the ordinary prompt path", async () => {
	session.setSlashCommands([{ ...REVIEW_COMMAND, dispatch: undefined }]);
	await session.prompt("/review foo.ts");

	expect(taskCalls).toEqual([]);
	expect(mock.calls).toHaveLength(1);
	expect(mock.calls[0].context.messages.at(-1)).toMatchObject({ role: "user" });
});
