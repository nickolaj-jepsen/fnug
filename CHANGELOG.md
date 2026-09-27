# Changelog

All notable changes to fnug are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
Releases up to 0.1.0-alpha.13 are described on
[GitHub Releases](https://github.com/nickolaj-jepsen/fnug/releases).

## [Unreleased]

### Added

- `fnug init` and `fnug setup` write a starter config from the project's tooling, with a workspace root in monorepos
- `fnug schema` prints the config's JSON Schema, for editor completion and validation
- `--root <DIR>` resolves the config's paths against another directory
- `-V`/`--version`
- `depends_on` accepts command names as well as ids
- Per-command `timeout`, inherited by child groups and commands
- `fnug check --jobs N` runs independent commands in parallel; `exclusive: true` keeps a command to itself
- `fnug check <TARGET>...` runs the named commands
- `fnug check --staged` selects by staged changes, and `--stash` checks exactly what is staged
- `fnug check --base <REF>` selects changes since the merge base with `REF`, for CI
- Commands get the changed files that selected them in `{files}` and `FNUG_FILES`
- `auto.run_on_change` reruns a watched command on each change; `w` toggles it in the TUI
- The TUI shows why each command was selected, and reloads the config when it changes or on `F5`
- The TUI shows status messages in the toolbar and renders dim, strikethrough and combining characters
- MCP: `base`, `include_manual`, `timeout_secs` and `jobs` parameters, and `runs_in_check` in `list_lints`
- Python wrapper: `all_`, `no_workspace`, `root`, `log_level` and the new `fnug check` options
- Python `Config`: `timeout`, `exclusive`, `run_on_change` and `$schema`

### Changed

- **Breaking:** Unknown config keys are errors; replace YAML merge keys (`<<:`) with plain aliases
- **Breaking:** Ids default to the name instead of a random UUID; give same-named siblings an explicit `id`
- **Breaking:** `auto.regex: []` and `auto.path: []` clear the inherited value
- **Breaking:** `env` values expand `$VAR` and `${VAR}`; write `$$` for a literal `$`
- **Breaking:** Workspace packages don't inherit the root's `cwd`, `auto` or `env`, and their ids get a package prefix
- **Breaking:** `-c` loads that file as the root; a parent workspace is used only if it includes the nearest config
- `fnug_version` is optional
- **Breaking:** `auto.regex` matches paths relative to the command's `cwd`; rewrite `/tests/` as `(^|/)tests/`
- **Breaking:** `auto.watch` ignores gitignored files and `.git`; list an ignored directory as a watch `path` to watch it
- **Breaking:** On Linux, `auto.watch` no longer follows symlinked directories; watch the link's target instead
- **Breaking:** fnug exits 2 when it can't run (bad config, TUI failure), so 1 always means a check failed
- **Breaking:** `fnug check --all` skips `auto.check: false` commands; add `--include-manual` to run those too
- **Breaking:** `fnug check` fails a command that modifies tracked files; use check-only forms or `--allow-modifications`
- **Breaking:** Without a terminal (hook, CI, `--jobs`), commands can't prompt on `/dev/tty` and their leftover processes are stopped
- **Breaking:** The pre-commit hook runs `fnug check --staged --stash`; upgrade fnug everywhere, then rerun `fnug setup`
- **Breaking:** `fnug setup` installs the hook where git reads it (`core.hooksPath` or the main repo); rerun it
- **Breaking:** fnug's hook lines are fenced by `# >>> fnug >>>` right after the shebang; rerun `fnug setup` to update
- **Breaking:** `fnug setup` offers sub-repo hooks only for workspace packages in another repository
- `fnug setup` edits editor MCP configs in place, keeping comments, and passes on `-c`, `--root` and `--no-workspace`
- **Breaking:** TUI stop, restart and clear send `SIGINT` to the whole process group, then `SIGKILL` after 2 s
- **Breaking:** `Esc` in the TUI tree no longer quits; use `q` or `Ctrl+C`
- **Breaking:** `Tab` focuses any running command, and every key (`Esc` too) goes to it until you press `Ctrl+]`
- A TUI run waits for every dependency in it and starts each command once; rerunning stops the previous run
- **Breaking:** MCP returns a compact summary plus capped output of failed commands; set `verbose` for the rest
- **Breaking:** MCP `run_lint` reports an ambiguous or unknown name as a tool error; tools reject unknown parameters
- **Breaking:** MCP `run_all` skips `auto.check: false` commands; set `include_manual` to run them
- The MCP server reloads the config on every call and starts even when it is invalid
- **Breaking:** The Python wrapper runs an in-memory `Config` from the caller's directory, where its workspace looks for packages
- **Breaking:** The `fnug` library API changed throughout; it has no stability promise yet

### Removed

- **Breaking:** Windows binaries and wheels; run fnug under WSL

### Fixed

- `-c` and other global options work after the subcommand, with relative and `..` paths
- Git selection works in linked worktrees and submodules
- Selection keeps working for paths outside git and file names that aren't UTF-8
- Each `auto` field inherits from the parent group on its own
- Workspaces outside a git repository find their packages, and each package's `fnug_version` is checked
- File watching reacts within 500 ms and keeps watching files that editors replace on save
- `fnug check` keeps its failing exit code after handing off to the TUI
- `fnug check` passes the signal it gets on to its commands, and lists commands that never ran
- `fnug setup` writes Cursor's MCP config under `mcpServers`
- A command whose `cwd` is missing fails with a clear error, and so do the commands that depend on it
- The TUI clears stale errors on rerun, and clearing a queued command cancels its dependents
- The TUI stays responsive while a command prints a lot of output
- The TUI resizes command terminals with the pane, and clicks and scrolling hit the right rows
- The TUI quits cleanly on `SIGTERM` and `SIGHUP`, and restores the terminal when it fails to start
- `c` copies a command's whole output, through OSC 52 when there is no system clipboard
- Cancelling an MCP call stops the running command's whole process group

### Security

- **Breaking:** fnug refuses a config it found that another user owns; pass it with `-c` or trust it in `FNUG_SAFE_DIRECTORIES`

[Unreleased]: https://github.com/nickolaj-jepsen/fnug/compare/v0.1.0-alpha.13...HEAD
