# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What is Fnug?

Fnug is a TUI-based command runner / terminal multiplexer that auto-selects and executes lint/test commands based on git changes or file watching. It is a standalone Rust binary using ratatui for the terminal UI. It also provides a headless `check` mode for CI/pre-commit hooks.

## Development Commands

```bash
cargo fmt                                              # Format
cargo clippy --fix --allow-dirty --allow-staged        # Lint
cargo test                                             # Run tests
cargo build                                            # Debug build
cargo run --bin fnug                                   # Run TUI
cargo run --bin fnug -- check                          # Run headless check
```

## Architecture

Fnug is a standalone Rust binary (Rust 1.93, edition 2024) with a ratatui-based TUI.

### Module overview (`src/`)

- **`bin/fnug.rs`** — Binary entry point: parses CLI args (clap), loads config, launches TUI or subcommands (`check`, `init-hooks`)
- **`lib.rs`** — `load_config()`: finds/parses config, validates tree (duplicate IDs, empty names, dependency cycles), applies inheritance
- **`config_file.rs`** — Finds and parses `.fnug.yaml`/`.fnug.yml`/`.fnug.json` (serde)
- **`check.rs`** — Headless command runner: selects commands, resolves `depends_on` with topological sort, runs sequentially, reports pass/fail/skip
- **`init_hooks.rs`** — Installs a git pre-commit hook that runs `fnug check`
- **`logger.rs`** — Custom `log` implementation: writes to a ring buffer (`LogBuffer`) and optional file, notifies TUI for redraws
- **`theme.rs`** — Color constants for the UI (accent, status colors, toolbar palette)
- **`commands/`** — Data structures:
  - `command.rs` — `Command` struct (id, name, cmd, cwd, auto, env, depends_on, scrollback)
  - `group.rs` — `CommandGroup` (nested tree of groups and commands)
  - `auto.rs` — `Auto` rules (git, watch, always)
  - `inherit.rs` — `Inheritable` trait for cascading settings from parent to children
- **`selectors/`** — Auto-selection: `git.rs` (git2-based diff matching), `watch.rs` (notify file watcher), `always.rs`
- **`pty/`** — `terminal.rs` spawns commands in a PTY via portable-pty with dedicated reader/writer threads feeding a vt100 parser; `messages.rs` formats styled PTY output; `command.rs` builds the shell command
- **`tui/`** — ratatui UI:
  - `app.rs` — Main app state + tokio event loop (handles config hot-reload, watcher events, log updates)
  - `render.rs` — Layout and rendering logic
  - `key_handler.rs` / `mouse_handler.rs` — Input handling
  - `process_manager.rs` — Starting/stopping/restarting command processes
  - `tree_widget.rs` / `tree_state.rs` — Command tree navigation
  - `terminal_widget.rs` — PTY output renderer
  - `toolbar.rs` — Bottom toolbar with keybind hints
  - `log_state.rs` — `LogBuffer` / `LogEntry` ring buffer for in-TUI log panel
  - `event.rs` — Crossterm-to-app event translation

### Key patterns

- **Inheritance**: Settings (cwd, auto rules) cascade from parent `CommandGroup` to children via `Inheritable` trait
- **Dependencies**: Commands can declare `depends_on` other commands; resolved via topological sort (Kahn's algorithm) in check mode
- **PTY management**: Each command runs in its own PTY with dedicated reader/writer threads feeding a vt100 parser
- **Watch channel**: Terminal output changes are broadcast via `tokio::sync::watch` to trigger UI redraws
- **Config hot-reload**: Config file is watched with `notify`; changes trigger re-parse and UI update
- **Logging**: Custom `log` crate logger writes to a shared ring buffer displayed in a TUI log panel, with optional file output
- **Async runtime**: Tokio multi-thread runtime drives the event loop, signal handling, and process management

## Configuration

Fnug searches for `.fnug.yaml`, `.fnug.yml`, or `.fnug.json` from cwd upward. Config defines a tree of `CommandGroup`s containing `Command`s with optional `auto` rules (git, watch, always). Commands support `depends_on` for ordering, `env` for environment variables, and `scrollback` for PTY buffer size.

## Releasing

Releases are automated via GitHub Actions (`.github/workflows/release.yaml`), triggered when the version in `Cargo.toml` changes on the `main` branch. The workflow automatically creates and pushes the `vX.Y.Z` git tag.

1. Update the version in `Cargo.toml` (and `vendor/vt100/Cargo.toml` if the vendored crate changed)
2. Run `cargo generate-lockfile` to update `Cargo.lock`
3. Commit: `git commit -m "chore: bump version to X.Y.Z"`

## Python Package

Fnug is also published to PyPI via maturin (`bindings = "bin"`). The Python wrapper lives in `python/fnug/`.

```bash
uv venv && source .venv/bin/activate.fish
uv pip install maturin pyyaml
maturin develop --release                              # Build & install locally
python -c "import fnug; fnug.run('--help')"            # Verify wrapper
python -c "from fnug.config import Config, Command; print(Config(name='test', commands=[Command(name='t', cmd='echo hi')]).to_yaml())"
```

## Commit Messages

- Use conventional commits (e.g. `feat:`, `fix:`, `chore:`)
- Be as concise as possible
- Max line width of 72 characters
- Avoid writing a body unless necessary
- For breaking changes, include `BREAKING CHANGE:` in the body with a description
- Split up large changes into multiple commits if possible (e.g. separate refactors from feature additions)

## Code Style

- Rust: rustfmt + clippy (pedantic), edition 2024
- Python: ruff (select = ALL, see pyproject.toml for ignores)
- Vendored dependency: `vendor/vt100` (modified vt100 crate, published as `fnug-vt100`)
- Key crates: ratatui, crossterm, tokio, clap, git2, portable-pty, notify, log, parking_lot
