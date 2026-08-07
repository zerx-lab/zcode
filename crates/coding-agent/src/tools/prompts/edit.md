Performs an exact string replacement in an existing file.

Usage:
- You must read the file (with `read`) before editing it, and copy `old_string` from the real file content — never reconstruct it from memory.
- `old_string` must carry enough surrounding context (a full statement, a whole block, a few neighboring lines) to identify exactly one location in the file. A short or generic snippet that occurs more than once is rejected with the list of matching lines — widen the context instead of retrying the same `old_string`.
- Matching tolerates minor incidental drift (trailing whitespace, a uniform indentation offset, one drifted line inside an otherwise-matching block) so a slightly stale read still applies, but this is a safety net, not something to rely on — always prefer copying the exact current text.
- The edit fails if `old_string` is not found anywhere in the file; the error includes the closest candidate line as a hint.
- Set `replace_all: true` to replace every occurrence at once (useful for renaming a variable/symbol throughout the file) instead of requiring a single unique match.
- `old_string` and `new_string` must differ. If a call is rejected for being a no-op, that means the payload itself produced no change — re-check which text you actually meant to change instead of resubmitting the same arguments.
- ALWAYS prefer `edit` over `write` for changes to existing files. NEVER rewrite an entire file with `write` just to change a small part of it.
- Only use emojis if the user explicitly requests it.
