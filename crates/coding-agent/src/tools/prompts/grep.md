Searches file contents with an in-process regex engine.

<instruction>
- `pattern` is a regex. If it fails to compile, the search degrades to a literal substring
  match — the response always says so explicitly, it is never silent about it.
- `path` scopes the search to one or more files or directories; separate multiple targets with
  `;` (`src; tests`). Omitted -> searches the workspace root (`.`).
- A single-file entry may carry a line-range selector, same grammar as `read`
  (`src/foo.rs:50-100`, `src/foo.rs:50+10`) to search only within those lines.
- `case` forces case-sensitive (`true`) or case-insensitive (`false`) matching; omitted uses
  smart-case (case-sensitive only when the pattern itself contains an uppercase letter).
- `skip` pages past files already seen — pass the `skip=<N>` value from the previous
  response's pagination footer.
</instruction>

<output>
Results are grouped by file, showing line number and text for each match, followed by a
pagination footer with the exact `skip=<N>` for the next page when more files remain.
</output>

<critical>
MUST use this instead of shelling out to `grep`/`rg`.
</critical>
