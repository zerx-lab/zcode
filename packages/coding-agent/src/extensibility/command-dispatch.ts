/**
 * zcode fork: agent/model dispatch for file-based slash commands.
 *
 * Mirrors opencode's command `agent` / `model` / `subtask` frontmatter fields
 * (https://opencode.ai/docs/commands/):
 *
 * - Subagent mode (`agent:` present, or `subtask: true`): the expanded command
 *   body is delegated to the named agent through a forced `task` tool call, so
 *   the run gets the full task stack (progress UI, isolation, persistence).
 * - Current-session mode (`model:` without `agent`, or `subtask: false`): the
 *   command runs in the current conversation under the resolved model for one
 *   turn; the previous model is restored when the turn ends.
 *
 * Model selectors accept anything `resolveModelOverride` understands. Role
 * aliases (`@advisor`, `@smol`, ...) are recommended over provider/id pins so
 * rebinding a role in settings retargets every command that references it.
 */
import { parseFrontmatter, prompt } from "@oh-my-pi/pi-utils";
import { resolveModelOverride } from "../config/model-resolver";
import type { AgentSession } from "../session/agent-session";
import delegateTemplate from "./command-dispatch-delegate.md" with { type: "text" };
import { expandSlashCommand, type FileSlashCommand } from "./slash-commands";

/** Dispatch directives parsed from a command file's frontmatter. */
export interface CommandDispatchSpec {
	/** Agent definition to delegate to (implies subagent mode unless `subtask: false`). */
	agent?: string;
	/** Model selector; prefer a role alias such as `@advisor`. */
	model?: string;
	/** Force subagent (`true`) or current-session (`false`) execution. */
	subtask?: boolean;
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
	return { agent, model, subtask };
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
 * dispatch directives. Returns the text to submit through the normal prompt
 * flow — this function never consumes the prompt itself.
 */
export async function expandAndDispatchSlashCommand(
	session: AgentSession,
	text: string,
	fileCommands: FileSlashCommand[],
): Promise<string> {
	const spaceIndex = text.indexOf(" ");
	const commandName = spaceIndex === -1 ? text.slice(1) : text.slice(1, spaceIndex);
	const command = fileCommands.find(cmd => cmd.name === commandName);
	const expanded = expandSlashCommand(text, fileCommands);
	const spec = command?.dispatch;
	if (!spec) return expanded;

	const label = `/${commandName}`;
	if (session.isStreaming) {
		// Forced tool choices and model switches applied mid-turn would hijack
		// the in-flight loop; queue the plain expansion instead.
		session.emitNotice(
			"warning",
			`${label}: agent/model dispatch is skipped while the agent is streaming; queued as a plain prompt.`,
			"command-dispatch",
		);
		return expanded;
	}
	if (commandDispatchMode(spec) === "subagent") {
		return dispatchToSubagent(session, label, spec, expanded);
	}
	return dispatchInSession(session, label, spec, expanded);
}

/**
 * Subagent mode: force the next model call to invoke the `task` tool once
 * (`setForcedToolChoice` pushes `[forced, "none"]`, so the model spawns the
 * subagent and then summarizes its result) and wrap the expanded body in a
 * delegation instruction naming the target agent.
 */
function dispatchToSubagent(session: AgentSession, label: string, spec: CommandDispatchSpec, expanded: string): string {
	const agentName = spec.agent ?? "task";
	if (spec.model) {
		// Per-spawn model overrides cannot ride the task wire schema, so route
		// them through the `task.agentModelOverrides` runtime settings layer —
		// the same channel /agents and the security coordinator use — and
		// restore the prior record at turn end. Cost: the runtime override
		// layer owns this key for the rest of the process, shadowing later
		// project/global edits to it; acceptable for user-driven serial
		// commands.
		const before = { ...session.settings.get("task.agentModelOverrides") };
		session.settings.override("task.agentModelOverrides", { ...before, [agentName]: spec.model });
		onceAgentEnd(session, () => session.settings.override("task.agentModelOverrides", before));
	}
	try {
		session.setForcedToolChoice("task");
	} catch (error) {
		// Task tool inactive (spawn policy, subagent depth) or the provider
		// cannot force a named tool: the delegation prompt alone still steers.
		const detail = error instanceof Error ? error.message : String(error);
		session.emitNotice(
			"warning",
			`${label}: cannot force the task tool (${detail}); delegation relies on the prompt alone.`,
			"command-dispatch",
		);
	}
	return prompt.render(delegateTemplate, { agentName, body: expanded });
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
): Promise<string> {
	if (!spec.model) {
		if (spec.agent) {
			session.emitNotice(
				"warning",
				`${label}: 'agent' requires subagent execution; with 'subtask: false' it is ignored.`,
				"command-dispatch",
			);
		}
		return expanded;
	}
	const resolved = resolveModelOverride([spec.model], session.modelRegistry, session.settings);
	if (!resolved.model) {
		const detail = resolved.warning ? ` (${resolved.warning})` : "";
		session.emitNotice(
			"warning",
			`${label}: could not resolve model "${spec.model}"${detail}; using the current model.`,
			"command-dispatch",
		);
		return expanded;
	}
	const previous = session.model;
	if (previous && previous.provider === resolved.model.provider && previous.id === resolved.model.id) {
		return expanded;
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
	return expanded;
}

/** Run `restore` exactly once, on the next `agent_end` (fires on abort/error too). */
function onceAgentEnd(session: AgentSession, restore: () => void): void {
	const unsubscribe = session.subscribe(event => {
		if (event.type !== "agent_end") return;
		unsubscribe();
		restore();
	});
}
