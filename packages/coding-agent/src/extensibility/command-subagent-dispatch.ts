/**
 * zcode fork: direct execution for `subtask:` slash commands.
 *
 * Every argument of the `task` call — target agent, instructions, per-spawn
 * options — is already known the moment the command expands. The previous
 * design still forced a `task` tool choice and asked the main model to copy the
 * body into the call, which bought a full-context round trip, a second call to
 * relay the result, and a standing chance that the model summarized the body it
 * was told to pass verbatim. This module issues the call itself.
 *
 * The emitted event sequence mirrors `executeToolCalls` in
 * `@oh-my-pi/pi-agent-core` exactly — assistant `message_start`/`message_end`,
 * `tool_execution_start`, one `tool_execution_update` per progress tick,
 * `tool_execution_end`, then the tool-result message — so persistence, the TUI,
 * extension events, and transcript rebuilds cannot tell this call apart from a
 * model-issued one. `emitExternalEvent` appends each `message_end` to the
 * agent's live message list, so the interaction is in context by the time the
 * turn resumes with `agent.continue()`; that continuation is the turn's first
 * and only model call, and it sees the finished work.
 */
import {
	type AgentMessage,
	type AgentTool,
	type AgentToolResult,
	ASIDE_MESSAGE_COMMIT,
	type CommittableAsideMessage,
} from "@oh-my-pi/pi-agent-core";
import type { AssistantMessage, ToolCall, ToolResultMessage, Usage } from "@oh-my-pi/pi-ai";
import { logger } from "@oh-my-pi/pi-utils";
import type { AgentSession } from "../session/agent-session";

/** A fully resolved `task` call for a `subtask:` command, ready to execute. */
export interface PreparedSubagentDispatch {
	/** `/name` of the originating command, for diagnostics. */
	label: string;
	/** Target agent. Also the key of the per-spawn model override. */
	agentName: string;
	/** Flat-form `task` arguments (`{ agent, task, ... }`). */
	args: Record<string, unknown>;
	/** Model selector applied to {@link agentName} for the duration of the call. */
	modelOverride?: string;
}

/** Synthetic assistant turns cost nothing: no request was made. */
const ZERO_USAGE: Usage = {
	input: 0,
	output: 0,
	cacheRead: 0,
	cacheWrite: 0,
	totalTokens: 0,
	cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
};

/**
 * Commit `turnMessages` to the transcript, run the prepared `task` call, then
 * resume the turn so the model relays the result.
 *
 * Replaces `agent.prompt(turnMessages)` for a dispatching turn: the caller has
 * already finished the turn preamble (model/key validation, system prompt,
 * pre-prompt compaction), so the messages it assembled are committed here
 * instead of being handed to the loop, and the loop is entered through
 * `continue()` with the completed tool interaction already in context.
 */
export async function runSubagentDispatch(
	session: AgentSession,
	prepared: PreparedSubagentDispatch,
	turnMessages: readonly AgentMessage[],
): Promise<void> {
	const model = session.model;
	if (!model) throw new Error(`${prepared.label}: no model is selected.`);
	const tool = session.getToolByName("task");
	if (!tool) throw new Error(`${prepared.label}: the task tool is not registered in this session.`);

	for (const message of turnMessages) commitMessage(session, message);

	const call: ToolCall = {
		type: "toolCall",
		id: `cmd_${crypto.randomUUID().slice(0, 8)}`,
		name: "task",
		arguments: prepared.args,
	};
	const assistant: AssistantMessage = {
		role: "assistant",
		content: [call],
		api: model.api,
		provider: model.provider,
		model: model.id,
		usage: { ...ZERO_USAGE, cost: { ...ZERO_USAGE.cost } },
		stopReason: "toolUse",
		timestamp: Date.now(),
	};
	commitMessage(session, assistant);

	// The agent loop does not own this call yet, so its abort controller cannot
	// reach the subagent. Register ours: the session is already in-flight
	// (`isStreaming`), so the UI offers an interrupt the user expects to work.
	const controller = new AbortController();
	const unregister = session.registerPreModelAbort(controller);
	let result: AgentToolResult<unknown>;
	try {
		result = await executeTask(session, prepared, tool, call, controller.signal);
	} finally {
		unregister();
	}

	const isError = result.isError === true;
	const toolResult: ToolResultMessage = {
		role: "toolResult",
		toolCallId: call.id,
		toolName: call.name,
		content: result.content,
		details: result.details,
		providerMetadata: result.providerMetadata,
		isError,
		...(result.useless && !isError ? { useless: true } : {}),
		timestamp: Date.now(),
	};
	commitMessage(session, toolResult);

	// An interrupted dispatch leaves a paired call/result in the transcript and
	// stops there — spending a model call to narrate an aborted run is exactly
	// what the user cancelled.
	if (controller.signal.aborted) return;
	await session.agent.continue();
}

/**
 * Run the `task` tool with the loop's execution events around it. The per-spawn
 * model override rides `task.agentModelOverrides` (the same runtime settings
 * channel `/agents` and the security coordinator use) because it cannot travel
 * on the tool's wire schema; unlike the forced-tool-call design, the override is
 * held only across this `execute` call rather than a whole model turn.
 */
async function executeTask(
	session: AgentSession,
	prepared: PreparedSubagentDispatch,
	tool: AgentTool,
	call: ToolCall,
	signal: AbortSignal,
): Promise<AgentToolResult<unknown>> {
	const previousOverrides = { ...session.settings.get("task.agentModelOverrides") };
	if (prepared.modelOverride) {
		session.settings.override("task.agentModelOverrides", {
			...previousOverrides,
			[prepared.agentName]: prepared.modelOverride,
		});
	}
	session.agent.emitExternalEvent({
		type: "tool_execution_start",
		toolCallId: call.id,
		toolName: call.name,
		args: call.arguments,
	});
	let result: AgentToolResult<unknown>;
	try {
		result = await tool.execute(call.id, call.arguments, signal, partialResult => {
			session.agent.emitExternalEvent({
				type: "tool_execution_update",
				toolCallId: call.id,
				toolName: call.name,
				args: call.arguments,
				partialResult,
			});
		});
	} catch (error) {
		// The loop turns a throwing tool into an error result rather than killing
		// the turn; a dispatched command gets the same treatment so the failure
		// reaches the model as context instead of a stack trace.
		const detail = error instanceof Error ? error.message : String(error);
		logger.warn("Subtask command dispatch failed", {
			command: prepared.label,
			agent: prepared.agentName,
			error: detail,
		});
		result = {
			content: [{ type: "text", text: `The ${prepared.agentName} subagent failed: ${detail}` }],
			isError: true,
		};
	} finally {
		if (prepared.modelOverride) {
			session.settings.override("task.agentModelOverrides", previousOverrides);
		}
	}
	session.agent.emitExternalEvent({
		type: "tool_execution_end",
		toolCallId: call.id,
		toolName: call.name,
		result,
		isError: result.isError === true,
	});
	return result;
}

/**
 * Append a message to the live context exactly the way the loop's own input
 * emission does: `message_end` is what appends it to agent state, persists it,
 * and renders it, and an aside (e.g. a queued advisor card) must be told it
 * landed so it is not re-delivered on the next turn.
 */
function commitMessage(session: AgentSession, message: AgentMessage): void {
	session.agent.emitExternalEvent({ type: "message_start", message });
	session.agent.emitExternalEvent({ type: "message_end", message });
	(message as CommittableAsideMessage)[ASIDE_MESSAGE_COMMIT]?.();
}
