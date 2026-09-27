# Changelog

All notable changes to fnug are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
Releases up to 0.1.0-alpha.13 are described on
[GitHub Releases](https://github.com/nickolaj-jepsen/fnug/releases).

## [Unreleased]

### Changed

- **Breaking:** `Esc` in the TUI tree no longer quits; use `q` or `Ctrl+C`
- **Breaking:** MCP `run_all` skips `auto.check: false` commands, like `fnug check`

### Fixed

- `-c` and other global options work after the subcommand, with relative and `..` paths
- Git selection works in linked worktrees and submodules
- `fnug check` keeps its failing exit code after handing off to the TUI
- `fnug setup` writes Cursor's MCP config under `mcpServers`
- The TUI clears stale errors on rerun, and clearing a queued command cancels its dependents
- The TUI stays responsive while a command prints a lot of output

[Unreleased]: https://github.com/nickolaj-jepsen/fnug/compare/v0.1.0-alpha.13...HEAD
