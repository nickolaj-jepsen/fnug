# CLAUDE.md

## What is Fnug?

Fnug is a TUI command runner that auto-selects lint/test commands based on git changes or file watching. Standalone Rust binary (edition 2024; toolchain 1.93 pinned in `rust-toolchain.toml`, MSRV 1.88 in `Cargo.toml`) with a ratatui TUI. Also provides headless `check` mode for CI/pre-commit, `init` and `setup` for getting started, an MCP server for editor integration, and a thin Python wrapper on PyPI.

## Development

Nix dev environment via `flake.nix` + `direnv`. All tools (rust toolchain, ruff, alejandra, maturin, etc.) are provided by the flake.

```bash
# Rust
cargo fmt                                              # Format
cargo clippy --fix --allow-dirty --allow-staged        # Lint (auto-fix)
cargo clippy --all-targets -- -D warnings              # Lint (check only, tests included)
cargo nextest run                                      # Run tests (cargo test also works)
cargo test --doc                                       # Doctests (nextest skips them)
cargo test --manifest-path vendor/vt100/Cargo.toml     # Vendored vt100 fork's tests
cargo build                                            # Debug build

# Nix
alejandra --check .                                    # Format check
statix check .                                         # Lint
deadnix --fail .                                       # Dead code check
nix build -L .#default                                 # Package build

# Python (python/ directory)
ruff check python/                                     # Lint
ruff format --check python/                            # Format check
uv venv && uv pip install maturin pytest pyyaml        # Test venv (once)
uv run --no-project maturin develop --uv               # Build fnug into the venv
uv run --no-project pytest python/tests                # Wrapper tests

# Run
cargo run --bin fnug                                   # TUI mode
cargo run --bin fnug -- check                          # Headless check
cargo run --bin fnug -- init                           # Create a config
cargo run --bin fnug -- setup                          # Interactive setup wizard
cargo run --bin fnug -- mcp                            # MCP server (stdio)
cargo run --bin fnug -- schema                         # Config JSON Schema
```

CI (`.github/workflows/ci.yaml`) runs these checks (formatting with `cargo fmt --check`), the Rust tests on macOS too, `cargo +1.88 check --locked --all-targets` for the MSRV (keep it in sync with `rust-version` by hand), and `cargo deny check advisories`.

The project dogfoods itself — see `.fnug.yaml` for the lint/test config. It sets `workspace: true`, so `docs/.fnug.yaml`, the TUI demo that `docs/demo.tape` records with vhs, is merged in as a package; its `auto.check: false` keeps it out of `fnug check` and MCP runs. The fnug MCP server is also available in this workspace for running checks.

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

`cargo test` runs the unit tests (`#[cfg(test)]` modules next to the code, with `insta` snapshots in `src/tui/snapshots/`) and the integration tests in `tests/`, one file per area:

| File | Covers |
|---|---|
| `config.rs` | Loading, validation, ids, inheritance, the schema file, and the README and dogfood configs |
| `selectors.rs` | Git scopes, `auto.path`/`auto.regex` matching, selection issues, file watching |
| `runner.rs` | Planning with git selection, executing plans with real processes |
| `check_cli.rs` | `fnug check` as a process: output, exit codes, signals |
| `stash.rs` | `fnug check --staged --stash` |
| `cli.rs` | Argument parsing, config loading, setup output |
| `setup.rs` | Hooks (run with `sh` and a shim `fnug` on `PATH`), editor MCP configs |
| `init.rs` | `fnug init` detection and the config it writes |
| `mcp_cli.rs` | `fnug mcp` as a process: cancellation and shutdown |
| `tui.rs` | The binary's TUI in a pseudo-terminal |
| `integration.rs` | Older end-to-end tests of loading and check |

Shared helpers live in `tests/common/` (`mod common;`): writing and loading a config in a `tempfile::tempdir()`, `wait_until`, process helpers, and `common::git`, which runs the git CLI isolated from the user's git config and from the `GIT_DIR` a hook exports. Pattern:

```rust
let dir = tempfile::tempdir().unwrap();
let (config, cwd) = common::load(dir.path(), "fnug_version: 0.1.0\nname: t\ncommands: [...]");
```

Tests that start the binary use `env!("CARGO_BIN_EXE_fnug")`. Process and PTY tests synchronise through files the commands write and poll with a timeout instead of sleeping, and PTY tests skip themselves when no PTY can be opened. Review changed snapshots with `cargo insta review`, or accept them with `INSTA_UPDATE=always cargo test`.

The Python tests (`python/tests`) run the wrapper against a fake binary that records its arguments; `test_integration.py` needs the real one (`maturin develop`, or `FNUG_TEST_BINARY`) and skips without it.

## Configuration

Fnug searches for `.fnug.yaml`/`.yml`/`.json` from cwd upward (`-c` names one instead). Config is a tree of `CommandGroup`s containing `Command`s with optional `auto` rules (`git`, `watch`, `always`, `path`, `regex`, `check`, `run_on_change`). Commands support `id`, `depends_on`, `env`, `timeout`, `exclusive` and `scrollback`. Ids default to the name (the group path when names clash), so `depends_on` can use names. Unknown keys are errors. README.md's "Configuration reference" lists every key.

Workspace mode (`workspace: true` or `workspace: { paths: [...] }`) discovers package configs in subdirectories. When run from a package directory, fnug loads the nearest parent workspace root whose discovery includes that config. Use `--no-workspace` to disable, or `--root` to resolve paths against another directory.

## Releasing

Automated via GitHub Actions (`release.yaml`). A push to `main` that changes `Cargo.toml` releases its version unless the tag `v$VERSION` already exists. The tag is created last, so a release that failed partway can be retried from the Actions tab (`workflow_dispatch`); registries that already have the version are skipped. After CI passes, it builds binaries and wheels (Linux and macOS, x86_64 and aarch64), publishes fnug-vt100 and then fnug to crates.io, uploads the wheels and sdist to PyPI, and finally tags the commit and creates the GitHub release with the binaries and `SHA256SUMS`.

1. Update version in `Cargo.toml` (and `vendor/vt100/Cargo.toml` if the vendored crate changed since the last release; CI's `vt100-version` job fails otherwise)
2. Update the version in the README install commands (`cargo install --locked fnug@X.Y.Z`)
3. `cargo generate-lockfile`
4. `git commit -m "chore: bump version to X.Y.Z"`

## Python Package

Published to PyPI via maturin (`bindings = "bin"`): the wheel ships the binary. The wrapper in `python/fnug/` provides `run()`, `start()` (TUI), `check()` and programmatic config generation (`config.py`, which has to track the Rust config types). An in-memory `Config` is written to a temporary file and run with `--root` set to the caller's working directory.

```bash
uv venv && source .venv/bin/activate.fish
uv pip install maturin pytest pyyaml
maturin develop --release
pytest python/tests
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
