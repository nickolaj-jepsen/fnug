# Changelog

All notable changes to fnug are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
Releases up to 0.1.0-alpha.13 are described on
[GitHub Releases](https://github.com/nickolaj-jepsen/fnug/releases).

## [Unreleased]

### Added

- `fnug schema` prints the config's JSON Schema, for editor completion and validation
- `--root <DIR>` resolves the config's paths against another directory
- `depends_on` accepts command names as well as ids
- Python wrapper: `all_` and `no_workspace` options

### Changed

- **Breaking:** Unknown config keys are errors; replace YAML merge keys (`<<:`) with plain aliases
- **Breaking:** Ids default to the name instead of a random UUID; give same-named siblings an explicit `id`
- **Breaking:** `auto.regex: []` and `auto.path: []` clear the inherited value
- **Breaking:** `env` values expand `$VAR` and `${VAR}`; write `$$` for a literal `$`
- **Breaking:** Workspace packages don't inherit the root's `cwd`, `auto` or `env`, and their ids get a package prefix
- **Breaking:** `-c` loads that file as the root; a parent workspace is used only if it includes the nearest config
- `fnug_version` is optional
- **Breaking:** TUI stop, restart and clear send `SIGINT` to the whole process group, then `SIGKILL` after 2 s
- **Breaking:** `Esc` in the TUI tree no longer quits; use `q` or `Ctrl+C`
- **Breaking:** MCP `run_all` skips `auto.check: false` commands, like `fnug check`
- **Breaking:** The Python wrapper runs an in-memory `Config` from the caller's working directory
- **Breaking:** The `fnug` library API changed throughout; it has no stability promise yet

### Removed

- **Breaking:** Windows binaries and wheels; run fnug under WSL

### Fixed

- `-c` and other global options work after the subcommand, with relative and `..` paths
- Git selection works in linked worktrees and submodules
- Each `auto` field inherits from the parent group on its own
- Workspaces outside a git repository find their packages, and each package's `fnug_version` is checked
- `fnug check` keeps its failing exit code after handing off to the TUI
- `fnug setup` writes Cursor's MCP config under `mcpServers`
- A command whose `cwd` is missing fails with a clear error, and so do the commands that depend on it
- The TUI clears stale errors on rerun, and clearing a queued command cancels its dependents
- The TUI stays responsive while a command prints a lot of output

### Security

- **Breaking:** fnug refuses a config it found that another user owns; pass it with `-c` or trust it in `FNUG_SAFE_DIRECTORIES`

[Unreleased]: https://github.com/nickolaj-jepsen/fnug/compare/v0.1.0-alpha.13...HEAD
