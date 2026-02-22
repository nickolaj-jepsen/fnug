# Fnug

> [!WARNING]
> The main branch is currently undergoing a refactor to Rust. If you're looking for the latest Python version, see the [`python`](https://github.com/nickolaj-jepsen/fnug/tree/python) branch.

[![CI](https://github.com/nickolaj-jepsen/fnug/workflows/CI/badge.svg)](https://github.com/nickolaj-jepsen/fnug/actions)
[![Crates.io](https://img.shields.io/crates/v/fnug)](https://crates.io/crates/fnug)

Fnug is a TUI command runner that automatically selects and executes lint and test commands based on git changes or file watching. Think of it as a terminal multiplexer (like [tmux](https://github.com/tmux/tmux/wiki)), but purpose-built for running your dev commands side by side.

![screenshot](https://github.com/nickolaj-jepsen/fnug/assets/1039554/3fd812fc-e1dc-4dd2-86eb-de91dc8e027f)

## Features

- **TUI with full keyboard and mouse support** — navigate, select, and run commands from a terminal UI
- **Git integration** — automatically select commands based on uncommitted file changes
- **File watching** — monitor the file system and re-select commands when files change
- **Terminal emulation with scrollback** — full PTY support for interactive commands and long output
- **Headless mode** (`fnug check`) — run selected commands without the TUI, useful for CI and pre-commit hooks
- **Git hook integration** (`fnug init-hooks`) — install a pre-commit hook that runs `fnug check`
- **Search/filter** — press `/` to filter the command tree
- **Command dependencies** — define `depends_on` to control execution order
- **Environment variables** — set per-command or per-group env vars
- **Configurable scrollback** — set scrollback buffer size per command
- **Nested command groups** — organize commands into a hierarchical tree with inherited settings

## Installation

### From crates.io

```bash
cargo install fnug
```

### From GitHub Releases

Download a prebuilt binary from [GitHub Releases](https://github.com/nickolaj-jepsen/fnug/releases).

### With Nix

```bash
# Run directly
nix run github:nickolaj-jepsen/fnug

# Or install to profile
nix profile install github:nickolaj-jepsen/fnug
```

### From source

```bash
git clone https://github.com/nickolaj-jepsen/fnug.git
cd fnug
cargo install --path .
```

## Usage

Run `fnug` in a directory with a `.fnug.yaml` configuration file (or pass `-c path/to/config.yaml`).

### Subcommands

| Command | Description |
|---------|-------------|
| `fnug` | Launch the TUI |
| `fnug check` | Run selected commands headlessly (exit code reflects pass/fail) |
| `fnug check --fail-fast` | Stop on first failure |
| `fnug init-hooks` | Install a git pre-commit hook that runs `fnug check` |
| `fnug init-hooks --force` | Overwrite an existing pre-commit hook |

## Configuration

Fnug searches for `.fnug.yaml`, `.fnug.yml`, or `.fnug.json` from the current directory upward.

### Minimal example

```yaml
fnug_version: 0.1.0
name: my-project
commands:
  - name: hello
    cmd: echo world
```

### Git auto-selection

Select commands based on uncommitted changes. Re-trigger with `g` in the TUI.

```yaml
fnug_version: 0.1.0
name: my-project
commands:
  - name: lint
    cmd: cargo clippy
    auto:
      git: true
      path:
        - "./src"
      regex:
        - "\\.rs$"
```

### File watching

Monitor the file system and select commands when matching files change. Can be combined with git auto.

```yaml
fnug_version: 0.1.0
name: my-project
commands:
  - name: test
    cmd: cargo test
    auto:
      watch: true
      path:
        - "./src"
      regex:
        - "\\.rs$"
```

### Nested groups with inheritance

Groups inherit `cwd`, `auto`, and `env` settings from their parent.

```yaml
fnug_version: 0.1.0
name: my-project
children:
  - name: backend
    auto:
      git: true
      watch: true
      path:
        - "./src"
      regex:
        - "\\.rs$"
    commands:
      - name: fmt
        cmd: cargo fmt
      - name: test
        cmd: cargo test
      - name: clippy
        cmd: cargo clippy
```

### Advanced example

See this project's [`.fnug.yaml`](.fnug.yaml) for a full example.

## Keyboard Shortcuts

| Key | Context | Action |
|-----|---------|--------|
| `j` / `↓` | Tree | Move down |
| `k` / `↑` | Tree | Move up |
| `h` / `←` | Tree | Collapse group / Deselect command |
| `l` / `→` | Tree | Expand group / Select command |
| `Space` | Tree | Toggle expand/select |
| `Enter` | Tree | Run all selected commands |
| `r` | Tree | Run current command |
| `s` | Tree | Stop current command |
| `c` | Tree | Clear current command |
| `g` | Tree | Git auto-select |
| `/` | Tree | Search/filter commands |
| `Esc` | Search | Clear search |
| `L` | Tree | Toggle log panel |
| `Tab` | Tree | Focus terminal |
| `Esc` | Terminal | Back to tree |
| `Ctrl+R` | Global | Toggle fullscreen |
| `Ctrl+C` | Global | Quit |
| `q` | Tree | Quit |

### Mouse

- **Click** a tree item to select it
- **Double-click** a command to run it, or a group to expand/collapse
- **Click** the selection orb (●/○) or arrow (▼/▶) to toggle
- **Drag** the separator between tree and terminal to resize
- **Scroll wheel** in the terminal panel to scroll output

## Demo

https://github.com/nickolaj-jepsen/fnug/assets/1039554/8f8a4d34-8beb-4fb4-9bbc-6fd0a4a384be
