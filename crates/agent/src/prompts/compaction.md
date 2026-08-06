You are compacting a coding session so it can continue past the model's context limit.

Write a summary of the conversation so far. The summary replaces the original messages
verbatim — anything you leave out is permanently lost to the assistant that continues this
session. Optimise for **resuming work**, not for readability.

Cover, in this order, and omit any section that genuinely has no content:

1. **Goal** — what the user asked for, in their own terms. Include constraints they stated
   and decisions they made or rejected.
2. **State** — what has actually been done: files created, modified, or deleted with their
   paths; commands run and what they returned; anything verified and how.
3. **Findings** — facts discovered about the codebase that were expensive to learn:
   symbol locations, call sites, conventions, gotchas. Include exact paths and identifiers.
4. **Open** — what is unfinished, what failed and why, what the next concrete action is.

Rules:

- Keep every exact identifier, path, line number, command, and error message that the next
  step needs. Paraphrasing `src/foo.rs:42` into "a file in src" destroys the summary's value.
- Do not include the assistant's reasoning, apologies, or narration. Only facts and state.
- Do not invent progress. If something was attempted and did not work, say so plainly.
- Do not address the user. This text is read by the assistant, not by a person.
