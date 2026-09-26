# Fnug

[![CI](https://github.com/nickolaj-jepsen/fnug/workflows/CI/badge.svg)](https://github.com/nickolaj-jepsen/fnug/actions)
[![Crates.io](https://img.shields.io/crates/v/fnug)](https://crates.io/crates/fnug)
[![image](https://img.shields.io/pypi/v/fnug.svg)](https://pypi.python.org/pypi/fnug)

Fnug is a TUI command runner that automatically selects and executes lint and test commands based on git changes or file watching. Think of it as a terminal multiplexer (like [tmux](https://github.com/tmux/tmux/wiki)), but purpose-built for running your dev commands side by side.

![Fnug demo](docs/demo.gif)

## Features

- **Git integration** — automatically select commands based on uncommitted file changes
- **File watching** — monitor the file system and re-select commands when files change
- **Terminal emulation with scrollback** — full PTY support for interactive commands and long output
- **Headless mode** (`fnug check`) — run selected commands without the TUI, useful for CI
- **Setup wizard** (`fnug setup`) — install a pre-commit hook that runs `fnug check` and add the MCP server to your editor
- **Command dependencies** — define `depends_on` to control execution order
- **Environment variables** — set per-command or per-group env vars
- **Nested command groups** — organize commands into a hierarchical tree with inherited settings
- **Workspace support** — discover and merge `.fnug.yaml` files from subdirectories in mono-repos

## Installation

Linux and macOS only; commands run via `sh -c`.

### From crates.io

Only prereleases are published so far. `cargo install fnug` skips prereleases, so name the version explicitly:

```bash
cargo install --locked fnug@0.1.0-alpha.13
```

### From PyPI

The latest stable release on PyPI (0.0.x) is the old Python implementation, so allow prereleases:

```bash
# With uv
uv tool install --prerelease=allow fnug

# With pipx
pipx install --pip-args=--pre fnug
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

| Command       | Description                                                     |
| ------------- | --------------------------------------------------------------- |
| `fnug`        | Launch the TUI                                                  |
| `fnug check`  | Run selected commands headlessly (exit code reflects pass/fail) |
| `fnug setup`  | Interactive wizard: git pre-commit hook and editor MCP config   |
| `fnug mcp`    | Run an MCP server over stdio                                    |
| `fnug schema` | Print the config file's JSON Schema                             |

### Flags

| Flag              | Description                                                     |
| ----------------- | --------------------------------------------------------------- |
| `-c <path>`       | Path to config file, always loaded as the root                  |
| `--no-workspace`  | Disable workspace resolution (don't search for a parent root)   |
| `--root <dir>`    | Resolve the config's paths and workspace against `<dir>` instead of the config's directory, and don't look for a parent workspace root; without `-c`, search for the config from `<dir>` |
| `--log-file`      | Also write logs to a file                                       |
| `--log-level`     | Log level: off, error, warn, info, debug, trace (default: info, and warn on stderr) |
| `--fail-fast`     | Stop on first failure (`check` only)                            |
| `--no-tui`        | Never prompt to open TUI on failure (`check` only)              |
| `--mute-success`  | Capture each command's output and print it only if it fails (`check` only) |
| `--all`           | Include commands with `auto.check: false` (`check` only)        |
| `--timeout <dur>` | Kill commands that run longer than `<dur>` (seconds, or e.g. `90s`, `5m`) unless their config sets `timeout` (`check` only) |
| `-j`, `--jobs <n>` | Run up to `<n>` commands at once, each after its dependencies; `0` means one per CPU (default `1`, `check` only) |
| `-V`, `--version` | Print fnug's version                                            |

`-c`, `--no-workspace`, `--root`, `--log-file` and `--log-level` work with every subcommand. By default, warnings and errors, such as a config that needs a newer fnug, go to stderr in every mode, except while the TUI is open; then they show in its log panel (`L`). `--log-level` or the `FNUG_LOG` environment variable sets the stderr level too: `info` or `debug` shows more, and `error` or `off` hides warnings. fnug never logs to stdout, which `fnug mcp` uses for the protocol.

`fnug check` prints `PASS`, `FAIL (exit 3)`, `SKIP (build failed)` and so on for each command, then a summary that counts every selected command once: passed, failed, skipped because a dependency failed, and not run after `--fail-fast` stopped the run. Without `--mute-success`, commands share fnug's terminal and their output streams through. With it, each command's stdout and stderr are captured, merged in order. `--jobs` above 1 captures output the same way and prints each command's output, unless it passed and `--mute-success` is set, as soon as the command finishes. Commands still start in config order once their dependencies pass, and an `exclusive` one waits until nothing else runs and holds back the rest until it ends.

Captured commands, from `--mute-success`, `--jobs` above 1, the pre-commit hook or an MCP run, have no terminal. Their stdin is `/dev/null`, and each runs in a session of its own, so opening `/dev/tty`, as password and host-key prompts do, fails at once instead of waiting for input that never comes.

When none of fnug's stdin, stdout and stderr is a terminal, as in CI or behind a pipe, streamed commands run in a session of their own too. From a terminal, a streamed command stays in fnug's process group so it can use the terminal, and fnug signals only the command's own process: a process it started, such as a test binary under `cargo test`, can outlive a timeout or a signal and keep writing to the terminal. Use `--mute-success` or `--jobs` for timeouts that stop the whole process tree.

On SIGINT, SIGTERM or SIGHUP, `fnug check` stops the running commands, prints the summary so far, and exits with 128 plus the signal number. On Ctrl+C (SIGINT), a command that shares fnug's terminal has already got the terminal's SIGINT, so fnug gives it 3 s to finish before sending SIGTERM, while a command in a session of its own gets SIGINT from fnug. On SIGTERM or SIGHUP, commands get the same signal. A timeout, `--fail-fast` and an MCP client's cancellation send SIGTERM. Whichever signal a command gets, it gets SIGKILL if it is still running 3 s later. Signals reach the whole process group of a command in a session of its own, but only the process of a command that shares fnug's terminal.

### Setup

`fnug setup` installs a git pre-commit hook that runs `fnug check`, and adds the MCP server to your editors' project config: `.mcp.json` for Claude Code, `.vscode/mcp.json` and `.cursor/mcp.json`. It lists every change before making any, and makes them only once you confirm. Deselect something to remove it.

The hook goes where git reads hooks: in `core.hooksPath` if that is set, otherwise in the main repository's `.git/hooks`, which linked worktrees share. If husky manages the hooks, or `core.hooksPath` points outside the repository, setup prints the lines to add yourself instead. A `pre-commit` that is a symlink counts as the file it links to: setup edits that file, shows it in the list of changes, and prints the lines instead if it is outside the repository.

fnug's lines sit between `# >>> fnug >>>` and `# <<< fnug <<<`, right after the shebang of any existing hook, and the rest of that hook runs after fnug passes. If fnug needs something your hook sets up first, such as `PATH`, move the block below it; updates leave it where it is. A hook that isn't a shell script, such as a Python one, can be chained instead: it moves to `pre-commit.local` and runs after fnug, and removing fnug's hook puts it back.

The hook runs `fnug` from `PATH`, or else the binary that ran `fnug setup`, with the `-c`, `--root` and `--no-workspace` that `fnug setup` was given. A different or older fnug on `PATH` still comes first, so setup tells you when the one on its `PATH` isn't the binary running setup. It runs from the config's directory. With `--root`, it runs from that directory instead, and goes in that directory's repository even when the config is in another one. Run setup again after moving the config, and it offers to update the hook. If the hook runs a different config that still exists, as when you run setup from a nested config or with other flags, setup leaves it alone, even when you deselect the hook, and says which config it runs; it repoints the hook only if you say yes when asked. If fnug isn't installed, the commit fails with a hint; a hook in the work tree, such as one in a committed `core.hooksPath` or a committed script that `.git/hooks/pre-commit` links to, only warns, so teammates without fnug can still commit.

In a workspace, setup also offers a hook for each package whose config is in another repository, such as a git submodule. That hook runs the package's checks with `--no-workspace`.

Setup edits the editor configs in place, keeping comments and formatting, and adds or removes only the `fnug` entry. The entry runs `fnug mcp` with the `-c`, `--root` and `--no-workspace` that `fnug setup` was given, with paths relative to the project directory the editor starts it in, since the editor config is usually committed. fnug has to be on the editor's `PATH`. Run setup again with other flags, and it offers to update the entry.

## Configuration

Fnug searches for `.fnug.yaml`, `.fnug.yml`, or `.fnug.json` from the current directory upward.

Unknown keys are errors, so a typo like `depends-on` or `gti` fails loudly with its line number and a suggestion instead of being ignored. YAML anchors and aliases work (`auto: *defaults`), but merge keys (`<<: *defaults`) are not supported.

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

### Always auto-selection

Mark commands that should always be selected, regardless of git changes or file watching.

```yaml
fnug_version: 0.1.0
name: my-project
commands:
  - name: typecheck
    cmd: cargo check
    auto:
      always: true
```

### Excluding commands from check mode

Commands with `auto.check: false` are skipped during `fnug check`, git hooks and the MCP `run_lints`/`run_all` tools, but remain auto-selected in the TUI. Use `fnug check --all` to include them, or MCP `run_lint` to run one by name.

Useful for commands that are too slow or noisy for pre-commit checks but you still want to run them automatically in the TUI.

```yaml
fnug_version: 0.1.0
name: my-project
commands:
  - name: unit tests
    cmd: cargo test
    auto:
      git: true
  - name: integration tests
    cmd: cargo test --release
    auto:
      git: true
      check: false   # skip in `fnug check`, still auto-selected in TUI
```

### Nested groups with inheritance

Groups inherit `cwd`, `auto`, and `env` settings from their parent. Each `auto` field is inherited on its own, so a group's `check: false` applies to every command below it unless a command sets `check` itself.

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

`auto.path` and `auto.regex` are inherited separately and both must match, so a command that narrows `path` keeps the group's `regex`. Set `regex: []` to drop an inherited regex (any file under `path` matches), or `path: []` to reset `path` to the command's own directory:

```yaml
fnug_version: 0.1.0
name: my-project
children:
  - name: rust
    auto:
      git: true
      path: ["./src"]
      regex: ["\\.rs$"]
    commands:
      - name: test
        cmd: cargo test
      - name: lockfile
        cmd: cargo check --locked
        auto:
          path: ["./Cargo.toml"]
          regex: []
```

### Environment variables

`env` adds environment variables to every command in a group, or to one command, on top of the ones it inherits. In a value, `$VAR` and `${VAR}` expand to the inherited value of `VAR`, or else to fnug's own environment; an unset variable expands to nothing, with a warning. Write `$$` for a literal `$`. Variables in the same `env` map don't see each other.

```yaml
fnug_version: 0.1.0
name: my-project
env:
  PATH: "./node_modules/.bin:$PATH"
commands:
  - name: lint
    cmd: eslint .
    env:
      ESLINT_CACHE: "${HOME}/.cache/eslint"
```

### Ids and dependencies

Every command and group has an id, which `depends_on` and the MCP tools use. It defaults to the name, with `/` replaced by `-`. When several commands or groups would get the same default id, each of them gets its group path instead, such as `backend/test` and `frontend/test`. Set `id` to choose one yourself; explicit ids must be unique and can't contain `/`. If two still end up with the same id (siblings with the same name, or a command and a group with the same name in one group), fnug reports an error naming both; set `id` on one of them.

fnug resolves a `depends_on` entry within the command's config file, using the first of these that matches:

1. an id, or the name of one of the command's siblings;
2. a group path like `backend/test`;
3. a name that is unique in the file;
4. the full id of a command in another workspace package or the root, like `api/build`.

If an entry is the id of one command and also the name of a sibling, fnug reports an error rather than guessing. Use the sibling's group path, or give the other command a different `id`.

```yaml
fnug_version: 0.1.0
name: my-project
children:
  - name: backend
    commands:
      - name: build
        cmd: cargo build
      - name: test
        cmd: cargo test
        depends_on: [build]   # the sibling: backend/build
  - name: frontend
    commands:
      - name: build
        cmd: pnpm build
      - name: test
        cmd: pnpm test
        depends_on: [build]   # frontend/build
```

### Workspace

Workspace mode discovers `.fnug.yaml` files in subdirectories and merges them as child groups. This is useful for mono-repos where each package has its own config.

When `workspace: true`, fnug walks the filesystem (skipping `.gitignore`'d and hidden directories) to find sub-configs. Files do not need to be git-tracked to be discovered. Outside a git repository nothing counts as ignored, so only hidden directories are skipped.

```yaml
# Auto-discover sub-configs (walks up to 5 levels deep)
fnug_version: 0.1.0
name: my-monorepo
workspace: true
commands:
  - name: root-lint
    cmd: echo "root"
```

```yaml
# Custom max scan depth
workspace:
  max_depth: 2
```

```yaml
# Explicit glob patterns
workspace:
  paths:
    - "./packages/*/"
    - "./apps/*/"
```

Each package behaves the same as when fnug runs inside it on its own: its `cwd` and `auto.path` are relative to the package directory, and it inherits no `cwd`, `auto` or `env` from the root config. Package ids are prefixed with the package's id, which defaults to its `name`, so a `build` command in a package named `api` has the id `api/build`. Package ids must be unique across the workspace and must not match a root group's id. Inside a package, `depends_on: [build]` means the package's own `build`; reference another package's command by its full id (`api/build`) and a root command by its id.

When fnug finds a config by searching upward (no `-c`), it loads a parent workspace root instead if that root's own discovery includes the config. A config the root doesn't discover, for example in a gitignored or hidden directory, below `max_depth`, or not matched by `paths`, is loaded on its own. Parent configs that fail to parse, or whose discovery fails, are skipped with a warning. `-c` always loads the given file as the root. Use `--no-workspace` to never look for a parent workspace root.

### Trusted configs

A config runs commands as you, so fnug only loads a config it found by itself (in the current directory or a parent, as a parent workspace root, or as a workspace package) if the file is owned by you or by root, much like git's `safe.directory`. A config passed with `-c` is always loaded. If the nearest config belongs to another user, fnug stops with an error; an untrusted parent workspace root or package is skipped with a warning.

To trust configs owned by someone else, for example in a CI container where the checkout belongs to a different user, list their directories in `FNUG_SAFE_DIRECTORIES`, separated by `:`. Set it to `*` to trust every config.

Running fnug as root, for example with `sudo`, trusts only configs owned by root, so your own repository's config is refused. Pass it with `-c` instead.

### Advanced example

See this project's [`.fnug.yaml`](.fnug.yaml) for a full example.

### Editor support

fnug publishes a JSON Schema for config files, so editors can complete keys and flag mistakes. With the YAML language server (VS Code's YAML extension, Neovim's `yamlls`), add this as the first line of `.fnug.yaml`:

```yaml
# yaml-language-server: $schema=https://raw.githubusercontent.com/nickolaj-jepsen/fnug/main/schema/fnug.schema.json
```

In `.fnug.json`, use a `"$schema"` key with the same URL. `fnug schema` prints the schema for the installed version.

### Configuration reference

#### Root fields

| Field          | Type              | Description                                                       |
| -------------- | ----------------- | ----------------------------------------------------------------- |
| `fnug_version` | string            | Optional. fnug version the config targets (see below)             |
| `name`         | string            | Display name for the root group                                   |
| `workspace`    | bool / object     | Enable workspace mode (see [Workspace](#workspace))               |
| `commands`     | list              | Top-level commands                                                |
| `children`     | list              | Nested command groups                                             |
| `cwd`          | string            | Working directory (inherited by children)                         |
| `env`          | map               | Environment variables (inherited by children, `$VAR` expanded)    |
| `auto`         | object            | Default auto rules (inherited by children)                        |
| `timeout`      | integer / string  | Default command `timeout` (inherited by children)                 |
| `exclusive`    | bool              | Default command `exclusive` (inherited by children)               |
| `$schema`      | string            | JSON Schema URL for editors (mainly for `.fnug.json`); ignored    |

`fnug_version` compares only the `major.minor.patch` numbers, so `0.1.0` matches `0.1.0-alpha.13`. fnug warns when the config needs a newer fnug, when it was written for an older release series (a different minor version before 1.0, a different major version after), or when the version can't be parsed.

#### Command fields

| Field        | Type              | Description                                                         |
| ------------ | ----------------- | ------------------------------------------------------------------- |
| `name`       | string            | Display name (required)                                             |
| `cmd`        | string            | Shell command to run (required)                                     |
| `id`         | string            | Identifier — defaults to the name ([Ids](#ids-and-dependencies))    |
| `cwd`        | string            | Working directory override                                          |
| `env`        | map               | Extra environment variables (`$VAR` expanded)                       |
| `auto`       | object            | Auto-selection rules (see below)                                    |
| `depends_on` | list of strings   | Commands that must finish first, by id or unique name               |
| `scrollback` | integer           | PTY scrollback buffer size (number of lines)                        |
| `timeout`    | integer / string  | Time limit in `fnug check` and MCP runs (see below)                 |
| `exclusive`  | bool              | Never run alongside another command in `fnug check --jobs` runs     |

`timeout` is whole seconds or a duration with units, such as `90s`, `5m` or `1h 30m`. A command that runs longer in `fnug check` or an MCP run gets `SIGTERM`, then `SIGKILL` 3 s later, and is reported as `TIMEOUT`. The signals reach every process in the command's process group, unless the command streams to fnug's terminal: then only its own process gets them (see `fnug check` above). `0` means no limit, overriding an inherited value and `fnug check --timeout`. There is no limit by default, and the TUI ignores `timeout`.

`exclusive: true` suits commands that rewrite files, such as formatters, so that nothing reads the files while they change. It matters only when `fnug check --jobs` runs several commands at once; the TUI ignores it.

#### Group fields

| Field      | Type              | Description                                                           |
| ---------- | ----------------- | --------------------------------------------------------------------- |
| `name`     | string            | Display name (required)                                               |
| `id`       | string            | Identifier — defaults to the name ([Ids](#ids-and-dependencies))      |
| `cwd`      | string            | Working directory (inherited by children)                             |
| `env`      | map               | Environment variables (inherited by children, `$VAR` expanded)        |
| `auto`     | object            | Default auto rules (inherited by children)                            |
| `timeout`  | integer / string  | Default command `timeout` (inherited by children)                     |
| `exclusive` | bool             | Default command `exclusive` (inherited by children)                   |
| `commands` | list              | Commands in this group                                                |
| `children` | list              | Nested child groups                                                   |

#### Auto fields

| Field    | Type              | Description                                                             |
| -------- | ----------------- | ----------------------------------------------------------------------- |
| `git`    | bool              | Select when git-changed files match `path`/`regex`                      |
| `watch`  | bool              | Select when watched files match `path`/`regex`                          |
| `always` | bool              | Always selected regardless of changes                                   |
| `path`   | list of strings   | Path prefixes to match against (e.g. `"./src"`); they may not exist yet |
| `regex`  | list of strings   | Patterns for file paths relative to `cwd` (e.g. `"^src/.*\\.rs$"`)      |
| `check`  | bool              | Include in `fnug check` — set `false` to skip (default `true`)         |

A changed file selects a command when it is under one of its `path` entries and matches one of its `regex` patterns (any file, if there are none). The patterns see the file's path relative to the command's `cwd`, such as `src/main.rs`, or `../shared/lib.rs` for a file outside it. Anchor with `^` to match from the `cwd` (`^tests/`), or write `(^|/)tests/` to match a directory at any depth.

Neither `git` nor `watch` counts files that git ignores (through `.gitignore`, `.git/info/exclude` or `core.excludesFile`) or anything inside a `.git` directory. The watcher makes one exception: below a `path` that git ignores itself, such as `./target/doc`, every change counts. It goes by the ignore rules alone, so it also skips a file that git tracks although a rule matches it, which `git` still counts. A watch `path` that doesn't exist when fnug starts is not watched. Like `git`, the watcher doesn't follow symlinked directories. On Linux, a directory that a `.gitignore` edit stops ignoring is only watched once fnug restarts.

## Keyboard Shortcuts

| Key       | Context  | Action                            |
| --------- | -------- | --------------------------------- |
| `j` / `↓` | Tree     | Move down                         |
| `k` / `↑` | Tree     | Move up                           |
| `h` / `←` | Tree     | Collapse group / Deselect command |
| `l` / `→` | Tree     | Expand group / Select command     |
| `Space`   | Tree     | Toggle expand/select              |
| `Enter`   | Tree     | Run all selected commands         |
| `r`       | Tree     | Run current command               |
| `s`       | Tree     | Stop current command              |
| `c`       | Tree     | Clear current command             |
| `g`       | Tree     | Git auto-select                   |
| `/`       | Tree     | Search/filter commands            |
| `Esc`     | Search   | Clear search                      |
| `L`       | Tree     | Toggle log panel                  |
| `Tab`     | Tree     | Focus terminal                    |
| `Esc`     | Terminal | Back to tree                      |
| `Ctrl+R`  | Global   | Toggle fullscreen                 |
| `Ctrl+C`  | Global   | Quit                              |
| `q`       | Tree     | Quit                              |

### Mouse

- **Click** a tree item to select it
- **Double-click** a command to run it, or a group to expand/collapse
- **Click** the selection orb (●/○) or arrow (▼/▶) to toggle
- **Drag** the separator between tree and terminal to resize
- **Scroll wheel** in the terminal panel to scroll output
- **Right-click** a command for a context menu with run/stop/clear options

## Migrating from 0.1.0-alpha.13

- Unknown config keys are errors. Fix any key the error names (it suggests the closest valid key), and replace YAML merge keys (`<<: *anchor`) with a plain alias (`auto: *anchor`).
- `auto.regex: []` and `auto.path: []` now clear the value inherited from the parent group instead of being ignored. If you wrote `regex: []` expecting the group's regex to apply, remove the line.
- Ids default to the command or group name instead of a random UUID, and names that repeat get group-path ids such as `backend/test`. `depends_on` entries can now be names. Explicit ids can't contain `/`. Same-named siblings and same-named workspace packages, which used to load with random ids, are now an error; give one of them an `id`.
- Library API: `config_file::Config` has the root group's fields at the top level (use `Config::into_root()`), `fnug_version` is `Option<String>`, and `Config::find_config` is replaced by `fnug::load(&LoadOptions)`. `ConfigError` is `#[non_exhaustive]`; `DuplicateId` and `DirectoryNotFound` are struct variants and `Yaml`/`Json` carry a `hint`. `workspace::discover_and_merge` is split into `workspace::discover` and `workspace::merge`.
- Workspace packages no longer inherit `cwd`, `auto` or `env` from the root config, and a package's `cwd` is relative to its own directory, so it behaves the same merged or standalone. Package ids are prefixed with the package id (`api/build`); update `depends_on` entries that point into another package.
- `-c`/`--config` loads the given file as the root and no longer switches to a parent workspace root. Without `-c`, a parent workspace root is used only when its discovery includes the nearest config, and parent configs that fail to parse are skipped instead of failing the run.
- fnug refuses a config it finds by itself that is owned by another user (other than root). Pass the file with `-c`, or set `FNUG_SAFE_DIRECTORIES` to the directories to trust (`*` trusts all, e.g. in CI containers where the checkout has a different owner).
- `$` in `env` values now expands variables (`$VAR`, `${VAR}`). Write `$$` for a literal `$`.
- Library API: `pty::terminal::Terminal::new` takes `TerminalOptions` instead of a scrollback size, and `Terminal::wait` returns an `ExitInfo` instead of an exit code. `pty::format_success_message` and `pty::format_failure_message` are replaced by `pty::format_exit_message`.
- Stop, restart and clear send `SIGINT` to the command's whole process group, then `SIGKILL` 2 s later if it is still running; previously they sent one `SIGHUP` to the shell. Quitting sends `SIGHUP` to the whole process group too. Library API: `Terminal::kill` is replaced by `Terminal::stop` and `Terminal::force_kill`, and `tui::app::ProcessInstance::kill_and_abort` by `stop_and_abort`.
- Library API: `tui::app::AppEvent::ProcessExited` and `ProcessError` are struct variants that carry the run's generation, `ProcessExited` carries an `ExitInfo` instead of an exit code, and `CommandStatus` has a new `Stopped` variant.
- Quitting waits up to 1 s for commands to exit after `SIGHUP`, then sends `SIGKILL` to whatever is left, including background processes still holding a command's terminal. Library API: `tui::app::App::shutdown` is async and waits for the commands to exit.
- Windows binaries and wheels are no longer published. fnug supports Linux and macOS; on Windows, run it under WSL.
- `fnug setup` installs the pre-commit hook where git reads it: in `core.hooksPath` if that is set, otherwise in the main repository's hooks directory, which linked worktrees share. If husky manages the hooks, or `core.hooksPath` points outside the repository, it installs nothing and prints the lines to add yourself. Hooks installed from a linked worktree or with `core.hooksPath` set never ran; run `fnug setup` again. Library API: `setup::hooks::HookError` has new variants.
- `fnug setup` no longer edits a `pre-commit` that is a symlink to a file outside the repository; it prints the lines to add yourself instead. One that links into the work tree is treated as shared, so the lines setup adds hold no machine-specific path.
- fnug's lines in the pre-commit hook are fenced by `# >>> fnug >>>` and `# <<< fnug <<<` and sit right after the shebang, so a failing command later in your hook still blocks the commit and an `exec` can't skip fnug. `fnug setup` moves the old `# fnug` lines there. Hooks that aren't shell scripts, such as Python or fish ones, are refused unless you let setup chain them: yours moves to `pre-commit.local`, without the old `# fnug` lines, and runs after fnug. Library API: `setup::hooks::is_installed` also counts an outdated block.
- `fnug setup` offers sub-repo hooks only for workspace packages whose config is in another repository, such as a git submodule, and installs one hook per repository. A plain child group whose `cwd` points into another repository is no longer offered; update or remove a hook installed there by hand. Library API: `setup::workspace::find_sub_repos` returns only workspace packages, with `path` set to the package's config directory, and skips packages that share a hook.
- Library API: `setup::run` takes the `LoadedConfig` instead of its root group, and the `LoadOptions` it was loaded with.
- Library API: `setup::mcp::McpError` has a `Parse` variant instead of `Json`, and `NotAnObject` names the file.
- Library API: `logger::init` takes a `LoggerConfig` and returns a `LoggerHandle`, or an error instead of panicking when a logger is already installed. `logger::connect_event_sender` is replaced by `LoggerHandle::set_notifier`, and `logger::level_color` moved to `tui::log_state`. `LogBuffer` and `LogEntry` are defined in `logger` and still re-exported from `tui::log_state`.
- `auto.regex` is matched against the changed file's path relative to the command's `cwd` (`src/main.rs`) instead of its absolute path, in both git and watch selection. Suffix patterns such as `\.rs$` work as before. Rewrite patterns that relied on the absolute path, such as `/tests/`, as `^tests/` or `(^|/)tests/`.
- `auto.watch` ignores changes that git ignores (through `.gitignore`, `.git/info/exclude` or `core.excludesFile`), even to a tracked file that an ignore rule matches, and anything inside a `.git` directory, so build output and git's own writes no longer select commands. To watch generated or ignored files, add their ignored directory as a watch `path` (e.g. `./target/doc`): every change below such a path counts.
- On Linux, `auto.watch` no longer follows symlinked directories below a watch `path` (macOS never did), just as `auto.git` doesn't. To watch a linked directory, add the link's target as its own `path`.
- Library API: `selectors::watch::watch_commands` returns a `WatchHandle` (`events`, `report`) instead of a tuple, `WatchError` is `#[non_exhaustive]` and has `NothingWatched` instead of `Io`, and `WatchHandle::events` and `tui::app::AppEvent::WatcherTriggered` carry `Vec<WatchMatch>` instead of `Vec<Command>`.
- MCP `run_lint` reports an error listing the matching ids when a name matches several commands, instead of running the first one. Library API: `check::CheckError` has a `Plan` variant (a `runner::PlanError`) instead of `Selector`.
- Library API: `check::run` is async and takes a `CheckOptions` and a `CancellationToken`, and `CheckResult` carries a `runner::RunReport` instead of `selected_ids` and `failed_ids` (use `report.rerun_ids()`).
- MCP `run_lints`, `run_lint` and `run_all` results have one `output` field, with stdout and stderr merged in order, instead of `stdout` and `stderr`. They list every planned command, including ones `fail_fast` kept from running, with the statuses `passed`, `failed`, `timeout`, `skipped`, `cancelled` and `not_run`, and count `timed_out`, `cancelled` and `not_run` too. Cancelling a tool call, closing the server's stdin or sending it SIGTERM stops the running command's whole process group. Library API: `mcp::run` takes a `CancellationToken` that shuts the server down.
- Library API: `selectors::get_selected_commands` and `selectors::SelectorError` are removed; use `selectors::select`, or `runner::plan` for a run's commands in order. `RunnableSelector::split_active_commands` returns the split instead of a `Result`.
- In the TUI, a run waits for every dependency that runs with it, even one that passed before, and starts each command once. Rerunning a command stops its previous run right away, also when the rerun has to wait for a dependency first. Library API: `tui::app::App::start_command` is replaced by `App::run_commands` and `App::run_command`.
- Commands run by `fnug check --mute-success`, `fnug check --jobs` above 1 and the pre-commit hook have no terminal, so a command that prompts on `/dev/tty`, such as an ssh passphrase or host-key prompt or `sudo`, fails at once instead of prompting. Run such a command without `--mute-success`, or set it up so it doesn't need to prompt, for example with an ssh agent.
- Without a terminal, as in CI or behind a pipe, `fnug check` runs each streamed command in a session of its own, as it does captured ones. A timeout or signal then stops every process the command started, the command can't open `/dev/tty`, and processes it leaves running are stopped when it exits.
