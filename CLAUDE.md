# CLAUDE.md

## What is Fnug?

Fnug is a TUI command runner that auto-selects lint/test commands based on git changes or file watching. Standalone Rust binary (edition 2024, Rust 1.93) with a ratatui TUI. Also provides headless `check` mode for CI/pre-commit and an MCP server for editor integration.

## Development

Nix dev environment via `flake.nix` + `direnv`. All tools (rust toolchain, ruff, alejandra, maturin, etc.) are provided by the flake.

```bash
# Rust
cargo fmt                                              # Format
cargo clippy --fix --allow-dirty --allow-staged        # Lint (auto-fix)
cargo clippy --all-targets -- -D warnings              # Lint (check only)
cargo nextest run                                      # Run tests (cargo test also works)
cargo test --doc                                       # Doctests (nextest skips them)
cargo build                                            # Debug build

# Nix
alejandra --check .                                    # Format check
statix check .                                         # Lint
deadnix .                                              # Dead code check

# Python (python/ directory)
ruff check python/                                     # Lint
ruff format --check python/                            # Format check

# Run
cargo run --bin fnug                                   # TUI mode
cargo run --bin fnug -- check                          # Headless check
cargo run --bin fnug -- setup                          # Interactive setup wizard
cargo run --bin fnug -- mcp                            # MCP server (stdio)
```

The project dogfoods itself — see `.fnug.yaml` for the lint/test config. The fnug MCP server is also available in this workspace for running checks.

## Architecture

### Source layout (`src/`)

| Directory/File | Purpose |
|---|---|
| `bin/fnug/` | Binary: clap CLI and global options (`main.rs`), one module per subcommand (`tui.rs`, `check.rs`, `setup.rs`, `init.rs`, `mcp.rs`), `signals.rs` (SIGINT/SIGTERM/SIGHUP as cancellation) |
| `lib.rs` | `load(&LoadOptions)` → `LoadedConfig`: find the config (or a parent workspace root), check `fnug_version`, merge workspace packages, assign ids, validate, apply inheritance. `load_config()` wraps it |
| `config_file.rs` | Config discovery (`.fnug.yaml`/`.yml`/`.json`), serde types with `deny_unknown_fields` and typo hints, `ConfigError` |
| `schema.rs` | JSON Schema generated from the config types (schemars), printed by `fnug schema` |
| `trust.rs` | `TrustPolicy`: refuse configs fnug found itself that another user owns, unless `FNUG_SAFE_DIRECTORIES` lists them |
| `workspace.rs` | Workspace discovery (walk skipping hidden and gitignored dirs, or globs) and merging packages as child groups |
| `commands/` | Data model: `Command`, `CommandGroup`, `Auto`. `inherit.rs` (`Inheritable`), `ids.rs` (default ids, uniqueness, `depends_on` resolution, `resolve_command` by id or unique name), `env.rs` (`$VAR` expansion in `env`) |
| `selectors/` | `select()` → `SelectorOutput`, which never fails: `git.rs` (git2 changes per repo for a `GitScope`), `always.rs`, `watch.rs` (notify watcher → `WatchMatch`), `matching.rs` (`auto.path`/`auto.regex` matching for git and watch), `ignore.rs` (git's ignore rules for the watcher) |
| `runner/` | Engine behind check, MCP and the TUI: `plan.rs` (what runs, in dependency order), `dag.rs` (`DagState`), `exec.rs` (headless executor: jobs, timeouts, cancellation → `RunReport`), `process.rs` (`sh -c` invocation, `{files}`/`FNUG_FILES`, sessions), `output.rs` (bounded head/tail capture), `report.rs` (outcomes and counts) |
| `process.rs` | `ProcessHandle`: sees a child's exit without reaping it, signals its process group (or only its pid), escalates to SIGKILL |
| `check/` | `fnug check`: `mod.rs` (`run` → `CheckResult`), `printer.rs` (stderr lines and summary), `stash.rs` (`--stash`: set unstaged changes aside and put them back), `modified.rs` (fail commands that change tracked files) |
| `mcp/` | MCP server (rmcp, stdio) with `list_lints`, `run_lints`, `run_lint`, `run_all`: `mod.rs` (tools, config reload per call), `params.rs`, `response.rs` (JSON summary and capped output blocks), `text.rs` (escape stripping) |
| `init/` | `fnug init`: `detect.rs` (tooling from marker files, monorepo packages), `render.rs` (write the config) |
| `setup/` | `fnug setup` wizard: `hooks.rs` (pre-commit hook block), `mcp.rs` (editor MCP configs for Claude Code, VS Code and Cursor, edited in place as JSONC), `workspace.rs` (packages in other repos), `fsutil.rs` (atomic writes, `PATH` lookup) |
| `pty/` | One PTY per TUI command (portable-pty): `terminal.rs` (reader/writer threads feeding a vt100 parser), `command.rs` (`CommandBuilder` from a shell invocation), `messages.rs` (fnug's start and exit banners) |
| `tui/` | ratatui UI: `app.rs` (state, events, `run_commands` over `DagState`), `process_manager.rs`, `key_handler.rs`, `mouse_handler.rs`, `keymap.rs` (every keybinding), `render.rs`, `help.rs`, `toolbar.rs`, `tree_*.rs`, `watcher.rs` (`auto.watch` and config-file watchers), `reload.rs` (hot reload keyed by id), `clipboard.rs` (system clipboard or OSC 52), `auto_run.rs`, `stash_wait.rs`, `run_summary.rs`, `status.rs` |
| `logger.rs` | Global `log` impl: ring buffer for the TUI log panel, optional log file, and a stderr sink that is on until the TUI takes the terminal (`LoggerHandle::set_stderr`). Never stdout, which `fnug mcp` uses |
| `theme.rs` | Color constants |

### Key patterns

- **Loading** — Everything that needs a config goes through `fnug::load(&LoadOptions)`; `-c`, `--root` and `--no-workspace` map onto `LoadOptions`
- **Inheritance** — `cwd`, `auto` (each field on its own), `env`, `timeout` and `exclusive` cascade parent→child via the `Inheritable` trait. Workspace packages start fresh
- **Ids** — Default ids come from names, qualified with the group path when they clash and prefixed with the package id in a workspace. `commands::ids::resolve_command` (exact id, then a unique case-insensitive name) is the one lookup for CLI targets, the runner and MCP
- **Selection** — `selectors::select` reports problems as `SelectionIssue`s instead of failing; only fatal ones (a `--base` that doesn't resolve or has no commits to compare, `--staged` outside a repo) become a `PlanError`
- **Dependencies** — `runner::plan` picks the commands and adds their `depends_on` in order; `DagState` starts each command once its dependencies pass. The headless executor (`runner::execute`, used by check and MCP) and the TUI's `App::run_commands` both drive a `DagState`
- **Processes** — Headless commands run `sh -c` through `std::process::Command`, captured ones in a session of their own; `ProcessHandle` sees their exit without reaping them, so a signal never hits a reused pid. Each TUI command gets its own PTY with dedicated reader/writer threads feeding a vt100 parser
- **Async** — Tokio multi-thread runtime, built in `main.rs` with a short `shutdown_timeout`; signals become a `CancellationToken`
- **Redraws** — PTY output marks its terminal dirty and wakes the event loop through a shared `Arc<Notify>`; the loop redraws when the visible terminal is dirty or an event changed something, at most one frame per 16 ms. Only a mouse button press or release draws right away, and only when something besides the visible terminal's output changed since the last frame
- **Keymap** — `tui/keymap.rs` is the only list of keybindings: the help overlay and toolbar render from it, `key_handler` tests check each entry is handled, and `keymap_matches_readme` fails with the README table to paste when they differ
- **Schema** — Config structs are `deny_unknown_fields`; a new key means regenerating `schema/fnug.schema.json` (`cargo run --bin fnug -- schema > schema/fnug.schema.json`, enforced by a test) and adding it to `python/fnug/config.py`

## Testing

Integration tests live in `tests/integration.rs`. Pattern: write config to a `tempfile::tempdir()`, call `load_config()` or `check::run()`, assert results.

```rust
let dir = tempfile::tempdir().unwrap();
write_config(dir.path(), r#"..."#);
let (config, cwd) = load_config(Some(&path), false).unwrap();
```

Unit tests for validation logic are in `lib.rs` (`#[cfg(test)]` module).

## Configuration

Fnug searches for `.fnug.yaml`/`.yml`/`.json` from cwd upward. Config is a tree of `CommandGroup`s containing `Command`s with optional `auto` rules (git, watch, always). Commands support `depends_on`, `env`, and `scrollback`.

Workspace mode (`workspace: true` or `workspace: { paths: [...] }`) discovers sub-configs in subdirectories. When run from a subdirectory, fnug resolves upward to the nearest workspace root. Use `--no-workspace` to disable.

## Releasing

Automated via GitHub Actions (`release.yaml`), triggered when the version in `Cargo.toml` changes on `main`. Publishes to crates.io (fnug-vt100 first, then fnug) and PyPI.

1. Update version in `Cargo.toml` (and `vendor/vt100/Cargo.toml` if vendored crate changed)
2. Update the version in the README install commands (`cargo install --locked fnug@X.Y.Z`)
3. `cargo generate-lockfile`
4. `git commit -m "chore: bump version to X.Y.Z"`

## Python Package

Published to PyPI via maturin (`bindings = "bin"`). Wrapper in `python/fnug/` provides `run()`, `check()`, and programmatic config generation (`config.py`).

```bash
uv venv && source .venv/bin/activate.fish
uv pip install maturin pyyaml
maturin develop --release
```

## Commit Messages

- Conventional commits (`feat:`, `fix:`, `chore:`, etc.)
- Concise, max 72 chars wide, no body unless necessary
- `BREAKING CHANGE:` in body for breaking changes, plus a `**Breaking:**` entry in `CHANGELOG.md`
- Split large changes into multiple commits

## Code Style

- **Rust**: rustfmt + clippy pedantic (module_name_repetitions allowed), edition 2024
- **Python**: ruff with `select = ALL` (see pyproject.toml for ignores)
- **Nix**: alejandra + statix + deadnix
- **Vendored dep**: `vendor/vt100` is a modified vt100 fork published as `fnug-vt100` — version must be bumped separately when changed

## Changelog

`CHANGELOG.md` follows [Keep a Changelog 1.1](https://keepachangelog.com/en/1.1.0/). Every user-visible change (CLI, config, TUI, MCP, hooks, Python wrapper, packaging) adds an entry under `## [Unreleased]` in the same PR. Refactors, tests, CI and internal docs get none.

Readers skim it, so be as brief as possible:

- One line per entry, at most 160 characters (`tests/changelog.rs` enforces this). Say what changed for the user, not how it was built
- Sections in this order, only when non-empty: `Added`, `Changed`, `Deprecated`, `Removed`, `Fixed`, `Security`
- Prefix breaking changes with `**Breaking:**` and name what to do instead, e.g. ``**Breaking:** `--all` skips `auto.check: false` commands; add `--include-manual` ``
- The `fnug` library API has no stability promise yet: cover its changes with at most one `**Breaking:**` line per release
- Edit an existing Unreleased entry rather than adding a second one about the same feature; a fix to something added in the same release needs no entry
- Leave detail to the README; link a section (`[details](README.md#...)`) only when an entry can't stand alone

To release, rename `## [Unreleased]` to `## [X.Y.Z] - YYYY-MM-DD`, add an empty `## [Unreleased]` above it, and update the link references at the bottom. `release.yaml` publishes that section as the GitHub release notes (`scripts/release-notes.sh`) and fails before publishing anything without it.
