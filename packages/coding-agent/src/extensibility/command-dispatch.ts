/**
 * zcode fork: agent/model dispatch for file-based slash commands.
 *
 * Mirrors opencode's command `agent` / `model` / `subtask` frontmatter fields
 * (https://opencode.ai/docs/commands/):
 *
 * - Subagent mode (`agent:` present, or `subtask: true`): the expanded command
 *   body is handed straight to a `task` call this module prepares and
 *   {@link runSubagentDispatch} executes, so the body reaches the subagent
 *   verbatim and the turn's only model call is the one that relays the result.
 * - Current-session mode (`model:` without `agent`, or `subtask: false`): the
 *   command runs in the current conversation under the resolved model for one
 *   turn; the previous model is restored when the turn ends.
 *
 * Model selectors accept anything `resolveModelOverride` understands. Role
 * aliases (`@advisor`, `@smol`, ...) are recommended over provider/id pins so
 * rebinding a role in settings retargets every command that references it.
 *
 * Subagent mode also forwards the per-spawn `task` fields opencode has no
 * equivalent for (`name`, `outputSchema`, `schemaMode`, `isolated`, `effort`);
 * they used to be unreachable because the model, not the command, wrote the
 * call.
 */
import { parseFrontmatter } from "@oh-my-pi/pi-utils";
import { resolveModelOverride } from "../config/model-resolver";
import type { AgentSession } from "../session/agent-session";
import type { PreparedSubagentDispatch } from "./command-subagent-dispatch";
import { expandSlashCommand, type FileSlashCommand } from "./slash-commands";

/** Per-spawn effort selector accepted by the `task` tool. */
const TASK_EFFORTS = ["lo", "med", "hi"] as const;
type TaskEffortSelector = (typeof TASK_EFFORTS)[number];

/** Dispatch directives parsed from a command file's frontmatter. */
export interface CommandDispatchSpec {
	/** Agent definition to delegate to (implies subagent mode unless `subtask: false`). */
	agent?: string;
	/** Model selector; prefer a role alias such as `@advisor`. */
	model?: string;
	/** Force subagent (`true`) or current-session (`false`) execution. */
	subtask?: boolean;
	/** Subagent mode: display name for the spawn. */
	name?: string;
	/** Subagent mode: JSON-Schema the spawn's output must satisfy. */
	outputSchema?: unknown;
	/** Subagent mode: how a retry-exhausted invalid structured result is treated. */
	schemaMode?: "permissive" | "strict";
	/** Subagent mode: run the spawn in an isolated worktree. */
	isolated?: boolean;
	/** Subagent mode: coarse per-spawn thinking effort. */
	effort?: TaskEffortSelector;
}

/** What {@link expandAndDispatchSlashCommand} resolved the command to. */
export interface CommandDispatchOutcome {
	/**
	 * Text to submit through the normal prompt flow, or `undefined` when the
	 * command was rejected outright and nothing should run.
	 */
	text?: string;
	/**
	 * Subagent mode only. The caller runs this once the turn preamble has
	 * completed, instead of handing the assembled messages to the model.
	 */
	dispatch?: PreparedSubagentDispatch;
}

/**
 * Parse dispatch directives from a command file's raw content.
 * Returns `undefined` when the frontmatter carries no effective directive
 * (`subtask: false` alone is a no-op). Diagnostics are suppressed here —
 * `parseCommandTemplate` already warns on malformed frontmatter.
 */
export function parseCommandDispatchSpec(content: string, source?: string): CommandDispatchSpec | undefined {
	const { frontmatter } = parseFrontmatter(content, { source, level: "off" });
	const agent =
		typeof frontmatter.agent === "string" && frontmatter.agent.trim() ? frontmatter.agent.trim() : undefined;
	const model =
		typeof frontmatter.model === "string" && frontmatter.model.trim() ? frontmatter.model.trim() : undefined;
	const subtask = typeof frontmatter.subtask === "boolean" ? frontmatter.subtask : undefined;
	if (agent === undefined && model === undefined && subtask !== true) return undefined;
	const name = typeof frontmatter.name === "string" && frontmatter.name.trim() ? frontmatter.name.trim() : undefined;
	const outputSchema = frontmatter.outputSchema;
	const schemaMode =
		frontmatter.schemaMode === "permissive" || frontmatter.schemaMode === "strict"
			? frontmatter.schemaMode
			: undefined;
	const isolated = typeof frontmatter.isolated === "boolean" ? frontmatter.isolated : undefined;
	const effort = TASK_EFFORTS.find(candidate => candidate === frontmatter.effort);
	return {
		agent,
		model,
		subtask,
		name,
		...(outputSchema === undefined ? {} : { outputSchema }),
		schemaMode,
		isolated,
		effort,
	};
}

/**
 * Execution mode for a dispatch spec. opencode-compatible rule: subagent when
 * `subtask: true`, or when an `agent` is named and `subtask` is not `false`.
 */
export function commandDispatchMode(spec: CommandDispatchSpec): "subagent" | "session" {
	if (spec.subtask === true) return "subagent";
	if (spec.agent !== undefined && spec.subtask !== false) return "subagent";
	return "session";
}

/**
 * Drop-in replacement for the `expandSlashCommand` call in
 * `AgentSession.prompt()`: expands the matched file command, then applies its
 * dispatch directives. Never consumes the prompt itself and never runs the
 * subagent — subagent mode returns a prepared call for the caller to execute
 * once the turn preamble is done.
 */
export async function expandAndDispatchSlashCommand(
	session: AgentSession,
	text: string,
	fileCommands: FileSlashCommand[],
): Promise<CommandDispatchOutcome> {
	const spaceIndex = text.indexOf(" ");
	const commandName = spaceIndex === -1 ? text.slice(1) : text.slice(1, spaceIndex);
	const command = fileCommands.find(cmd => cmd.name === commandName);
	const expanded = expandSlashCommand(text, fileCommands);
	const spec = command?.dispatch;
	if (!spec) return { text: expanded };

	const label = `/${commandName}`;
	if (session.isStreaming) {
		// A dispatch owns its whole turn (it commits messages and drives the
		// loop); a model switch applied mid-turn would hijack the in-flight one.
		// Queue the plain expansion instead.
		session.emitNotice(
			"warning",
			`${label}: agent/model dispatch is skipped while the agent is streaming; queued as a plain prompt.`,
			"command-dispatch",
		);
		return { text: expanded };
	}
	if (commandDispatchMode(spec) === "subagent") {
		return prepareSubagentDispatch(session, label, spec, expanded);
	}
	return dispatchInSession(session, label, spec, expanded);
}

/**
 * Subagent mode: build the `task` call this command means. The flat
 * (`{ agent, task, ... }`) shape is accepted by the tool under either
 * `task.batch` setting, so there is nothing to choose at runtime.
 *
 * A missing `task` tool is fatal for the command rather than a downgrade:
 * `subtask` asked for delegation, and running the body in the main agent is the
 * opposite of that.
 */
function prepareSubagentDispatch(
	session: AgentSession,
	label: string,
	spec: CommandDispatchSpec,
	expanded: string,
): CommandDispatchOutcome {
	if (!session.getToolByName("task")) {
		session.emitNotice(
			"error",
			`${label}: this command delegates to a subagent, but the task tool is not available in this session.`,
			"command-dispatch",
		);
		return {};
	}
	const agentName = spec.agent ?? "task";
	const args: Record<string, unknown> = { agent: agentName, task: expanded };
	if (spec.name !== undefined) args.name = spec.name;
	if (spec.outputSchema !== undefined) args.outputSchema = spec.outputSchema;
	if (spec.schemaMode !== undefined) args.schemaMode = spec.schemaMode;
	if (spec.isolated !== undefined) args.isolated = spec.isolated;
	if (spec.effort !== undefined) args.effort = spec.effort;
	return { text: expanded, dispatch: { label, agentName, args, modelOverride: spec.model } };
}

/**
 * Current-session mode: switch to the resolved model for this turn only. The
 * switch is ephemeral (never persisted) and the previous model + configured
 * thinking level are restored on `agent_end`.
 */
async function dispatchInSession(
	session: AgentSession,
	label: string,
	spec: CommandDispatchSpec,
	expanded: string,
): Promise<CommandDispatchOutcome> {
	if (!spec.model) {
		if (spec.agent) {
			session.emitNotice(
				"warning",
				`${label}: 'agent' requires subagent execution; with 'subtask: false' it is ignored.`,
				"command-dispatch",
			);
		}
		return { text: expanded };
	}
	const resolved = resolveModelOverride([spec.model], session.modelRegistry, session.settings);
	if (!resolved.model) {
		const detail = resolved.warning ? ` (${resolved.warning})` : "";
		session.emitNotice(
			"warning",
			`${label}: could not resolve model "${spec.model}"${detail}; using the current model.`,
			"command-dispatch",
		);
		return { text: expanded };
	}
	const previous = session.model;
	if (previous && previous.provider === resolved.model.provider && previous.id === resolved.model.id) {
		return { text: expanded };
	}
	const previousLevel = session.configuredThinkingLevel();
	await session.setModelTemporary(
		resolved.model,
		resolved.explicitThinkingLevel ? resolved.thinkingLevel : undefined,
		{ ephemeral: true },
	);
	if (previous) {
		onceAgentEnd(session, () => {
			void session.setModelTemporary(previous, previousLevel, { ephemeral: true });
		});
	}
	return { text: expanded };
}

/** Run `restore` exactly once, on the next `agent_end` (fires on abort/error too). */
function onceAgentEnd(session: AgentSession, restore: () => void): void {
	const unsubscribe = session.subscribe(event => {
		if (event.type !== "agent_end") return;
		unsubscribe();
		restore();
	});
}
