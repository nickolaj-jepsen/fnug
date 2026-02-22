# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What is Fnug?

Fnug is a TUI-based command runner / terminal multiplexer that auto-selects and executes lint/test commands based on git changes or file watching. It is a standalone Rust binary using ratatui for the terminal UI.

## Development Commands

```bash
cargo fmt                                              # Format
cargo clippy --fix --allow-dirty --allow-staged        # Lint
cargo test                                             # Run tests
cargo build                                            # Debug build
cargo run --bin fnug                                   # Run
```

## Architecture

Fnug is a standalone Rust binary (Rust 1.86, edition 2021) with a ratatui-based TUI.

### Module overview (`src/`)

- **`bin/fnug.rs`** — Binary entry point: parses CLI args (clap), loads config, launches TUI
- **`lib.rs`** — `load_config()`: finds and parses `.fnug.yaml`, applies inheritance
- **`config_file.rs`** — Finds and parses `.fnug.yaml`/`.fnug.yml`/`.fnug.json` (serde)
- **`theme.rs`** — Color constants for the UI (accent, status colors, toolbar palette)
- **`commands/`** — Data structures: `Command`, `CommandGroup`, `Auto` rules, inheritance logic (`inherit.rs`)
- **`selectors/`** — Auto-selection: `git.rs` (git2-based diff matching), `watch.rs` (notify file watcher), `always.rs`
- **`pty/`** — `terminal.rs` spawns commands in a PTY via portable-pty with dedicated reader/writer threads feeding a vt100 parser; `messages.rs` formats styled PTY output; `command.rs` builds the shell command
- **`tui/`** — ratatui UI:
  - `app.rs` — Main app state + tokio event loop
  - `render.rs` — Layout and rendering logic
  - `key_handler.rs` / `mouse_handler.rs` — Input handling
  - `process_manager.rs` — Starting/stopping/restarting command processes
  - `tree_widget.rs` / `tree_state.rs` — Command tree navigation
  - `terminal_widget.rs` — PTY output renderer
  - `toolbar.rs` — Bottom toolbar with keybind hints
  - `event.rs` — Crossterm-to-app event translation

### Key patterns

- **Inheritance**: Settings (cwd, auto rules) cascade from parent `CommandGroup` to children via `Inheritable` trait
- **PTY management**: Each command runs in its own PTY with dedicated reader/writer threads feeding a vt100 parser
- **Watch channel**: Terminal output changes are broadcast via `tokio::sync::watch` to trigger UI redraws
- **Async runtime**: Tokio multi-thread runtime drives the event loop, signal handling, and process management

## Configuration

Fnug searches for `.fnug.yaml`, `.fnug.yml`, or `.fnug.json` from cwd upward. Config defines a tree of `CommandGroup`s containing `Command`s with optional `auto` rules (git, watch, always).

## Code Style

- Rust: rustfmt + clippy (pedantic), edition 2021
- Vendored dependency: `vendor/vt100` (modified vt100 crate)
- Key crates: ratatui, crossterm, tokio, clap, git2, portable-pty, notify
