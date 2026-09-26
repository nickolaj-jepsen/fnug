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
cargo test                                             # Run tests
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
| `bin/fnug/` | Binary entry point: CLI (clap), dispatches to TUI, check, setup, or MCP |
| `lib.rs` | `load_config()` — find/parse config, validate (duplicate IDs, cycles, empty names), apply inheritance |
| `config_file.rs` | Config file discovery and serde parsing (`.fnug.yaml`/`.yml`/`.json`) |
| `workspace.rs` | Workspace discovery: filesystem walk or glob expansion, merges sub-configs |
| `check.rs` | Headless runner: dependency resolution (topological sort), sequential execution, exit codes |
| `mcp.rs` | MCP server (rmcp): exposes `list_lints`, `run_all`, `run_lint` tools over stdio |
| `setup/` | Interactive wizard: git hook install/remove, MCP editor config (Claude Code, VS Code, Cursor) |
| `commands/` | Data model: `Command`, `CommandGroup`, `Auto` rules, `Inheritable` trait |
| `selectors/` | Auto-selection logic: `git.rs` (git2 diff matching), `watch.rs` (notify), `always.rs` |
| `pty/` | PTY management: spawns commands via portable-pty, reader/writer threads, vt100 parser |
| `tui/` | ratatui UI: app state/event loop, rendering, input handling, tree widget, process manager |
| `logger.rs` | Custom `log` impl: ring buffer for TUI log panel + optional file output |
| `theme.rs` | Color constants |

### Key patterns

- **Inheritance** — Settings (cwd, auto rules) cascade parent→child via `Inheritable` trait
- **Dependencies** — `depends_on` resolved via topological sort (Kahn's algorithm) in check mode
- **PTY** — Each command gets its own PTY with dedicated reader/writer threads feeding a vt100 parser
- **Async** — Tokio multi-thread runtime for event loop, signals, and process management
- **Redraws** — PTY output marks its terminal dirty and wakes the event loop through a shared `Arc<Notify>`; the loop redraws when the visible terminal is dirty or an event changed something, at most one frame per 16 ms. Only a mouse button press or release draws right away, and only when something besides the visible terminal's output changed since the last frame

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
