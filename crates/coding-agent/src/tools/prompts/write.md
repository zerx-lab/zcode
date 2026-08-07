Writes a file to the local filesystem, creating it if it does not exist.

Usage:
- This tool overwrites the entire existing file if there is one at the given path. It is not for partial edits — use `edit` to change part of an existing file, and prefer `edit` over `write` whenever the file already exists and you only need to change a portion of it.
- Missing parent directories are created automatically.
- The write is atomic: content is staged in a temporary file next to the target and then renamed into place, so a crash or a full disk can never leave the original file truncated or half-written.
- If the target path is a symbolic link, the write follows it through to the real file instead of replacing the link itself.
- `path` may be relative to the workspace root or absolute.
- Do not pass a `read`-style range selector (e.g. `foo.py:50-100`) as `path` with an empty `content` — if no literal file by that exact name exists, the call is rejected so you don't silently create a stray file named after a selector. Use `read` to read a range; only write to a selector-shaped filename on purpose by supplying non-empty `content`.
- ALWAYS prefer editing existing files in the codebase with `edit`. NEVER write new files unless explicitly required by the task.
- NEVER proactively create documentation files (`*.md`) or README files. Only create them if explicitly requested.
