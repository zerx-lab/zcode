Finds files and directories matching a glob pattern, ranked by recency.

<instruction>
- `pattern` uses gitignore-style glob syntax (`**` for recursive descent, `*`/`?`/`[...]` per
  path segment); a pattern with no `/` matches at any depth under `path`.
- `path` scopes the search to one or more directories; separate multiple targets with `;`
  (`src; tests`). Omitted -> searches the workspace root (`.`). A bare `/` or `//` means
  "search from here", not the filesystem root.
- `gitignore` defaults `true`. Set `false` to include ignored files such as `.env*`, logs, or
  build output.
- `hidden` defaults `true`; pair it with `gitignore: false` to see ignored dotfiles too.
- `limit` caps the result count; default and hard ceiling are both 200 — it can only be
  lowered, never raised.
</instruction>

<output>
Matches are sorted newest-first by modification time (recently touched files are what most
agent tasks care about, not alphabetical order); directories end in `/`.
</output>

<critical>
A scan that times out returns an incomplete result, not proof that nothing matches — narrow
`path` instead of retrying blindly.
</critical>
