Delegate this command to a subagent instead of doing the work yourself.

Call the `task` tool exactly once:

- Set `agent` to "{{agentName}}".
- Pass the task body below VERBATIM as the task instructions — do not summarize, rewrite, or extend it.
- If the tool requires the batch form, use `context` plus a single-item `tasks` array carrying the same body.

When the subagent returns, relay its result to the user, adding only essential framing.

<task-body>
{{{body}}}
</task-body>
