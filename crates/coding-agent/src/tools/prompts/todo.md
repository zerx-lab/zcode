Structured, phased todo list scoped to this session. Tasks are referenced by their
**verbatim content string**, never an auto-generated id — there is no "task-1"/"task-N".
Pass the task's full text (from a previous `view`/result) in the `task` field.

Single-op tool: every call carries exactly one `op`. To apply several changes, call the
tool several times.

## Operations

| `op` | Relevant fields | Effect |
| --- | --- | --- |
| `init` | `list: [{phase, items: string[]}]` | Replace the entire list with a phased plan |
| `init` | `items: string[]` (optional `phase`) | Flattened single-phase init |
| `start` | `task` | Mark one task `in_progress` |
| `done` | `task` or `phase` (omit both for all) | Mark completed |
| `drop` | `task` or `phase` (omit both for all) | Mark abandoned |
| `block` | `task` or `phase`, optional `reason` | Mark blocked — open but waiting on something outside your control |
| `unblock` | `task` or `phase` | Return a blocked task to `pending` |
| `rm` | `task` or `phase` (omit both to clear everything) | Remove matching tasks/phase |
| `append` | `phase`, `items: string[]` | Append tasks to `phase`; creates the phase if it doesn't exist |
| `view` | — | Read-only: echo the current list, no state change |

`in_progress` is a singleton: after any write, the earliest still-open task (in phase
order) is auto-promoted to `in_progress` if nothing else is. Completing tasks out of
phase order can move this pointer **back** to an earlier phase — that's expected;
completed tasks are never reverted.

`block` only reaches open work: a `phase` target skips tasks that are already
`completed`/`abandoned` rather than reopening them. An already-`blocked` task can be
`block`ed again to replace its `reason`.

A call whose target can't be resolved (unknown task/phase, missing required field,
duplicate task in `init`/`append`) fails as a whole — nothing from that call is
persisted, so retrying after fixing the argument never collides with a half-applied
write.

## Anatomy

- **Task content**: 5–10 words, what not how. Must be unique across the list.
- **Phase name**: short noun phrase (`Foundation`, `Auth`, `Verification`). Must be
  unique. Never prefix it with `1.`/`A)`/`Phase 1:` — numbering is display-only.

## Rules

- Mark a task `done` immediately after finishing it; don't batch completions.
- Never make a todo call your turn's only tool call — pair `init` with the first
  real reads/edits, and each `done`/`start` with the next action.
- Waiting on something you can't act on yourself (a user decision, another agent, an
  external service)? `block` the task instead of leaving it `pending` forever, and
  `unblock` once it's actionable again. If the blocker is itself something you can do,
  `append` a task for it instead of blocking.
- Keep `task`/`phase` strings stable once introduced — renaming breaks every later
  reference to them.
- Lost the exact task text? `view` echoes the list; never guess from memory.

## When to create a list

Create one when the work has 3+ distinct steps, the user explicitly asks for one, the
user hands you a set of tasks, or new instructions arrive mid-task that need to be
captured before you keep going. A multi-step plan from the user (phased todo, numbered
checklist, "N bugs/items") means every item becomes its own task — enumerate all of
them, never summarize into fewer tasks or drop items.

Skip it for single-step asks (one file read, one small edit, one question) — the
tracking overhead outweighs the benefit, and an unnecessary list is one more thing that
can drift from what you're actually doing.
