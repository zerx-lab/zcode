Executes a command in a shell.

<instruction>
- `command` is the only required parameter: the full command line to run, including pipes,
  redirects, globs, and `&&`/`;` chains — it is handed to a real shell, not split into argv
  yourself.
- `timeout` (seconds) bounds how long the command may run. Omit it to use the configured
  default. Whatever you pass is clamped into a fixed range — a value outside that range is
  silently rounded to the nearest bound, not rejected.
- `cwd` overrides the working directory for this one call; relative paths resolve against the
  workspace root. Omit it to run in the current working directory.
- This tool is for commands that finish on their own. It is not a job queue: there is no way to
  attach to a command after this call returns, and no way to send it further input. Do not use
  it for servers, watchers, or anything meant to keep running past the call — start those with a
  tool built for background processes, or run them in a way that detaches and exits immediately.
</instruction>

<timeout-semantics>
Hitting the timeout is not silent data loss: the command's own process tree is terminated (not
just the shell wrapper — anything it spawned is killed too), and whatever it had already printed
is not thrown away. If a command is close to done, prefer rerunning with a larger `timeout` over
polling; if it genuinely never finishes on its own, it does not belong behind this tool.
</timeout-semantics>

<critical>
A small, fixed set of command shapes (recursive deletion of `/`, piping a remote download
straight into a shell, disk-device writes, and similar) requires explicit confirmation before
running, regardless of the current approval mode. This is not configurable per call.
</critical>
