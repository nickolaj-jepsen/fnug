# Fnug

[![image](https://img.shields.io/pypi/v/fnug.svg)](https://pypi.python.org/pypi/fnug)
[![image](https://img.shields.io/pypi/l/fnug.svg)](https://pypi.python.org/pypi/fnug)
[![image](https://img.shields.io/pypi/pyversions/fnug.svg)](https://pypi.python.org/pypi/fnug)
[![Actions status](https://github.com/nickolaj-jepsen/fnug/workflows/CI/badge.svg)](https://github.com/nickolaj-jepsen/fnug/actions)

Fnug /_fnuk_/ is a lint runner, well actually it's a terminal multiplexer (like e.g. [tmux](https://github.com/tmux/tmux/wiki)), but with a focus on running lint commands defined in a [`.fnug.yaml`](#config) file, and displaying the result of those command.

> [!IMPORTANT]
> This is prerelease software, breaking changes might be introduced in minor versions!

## Installation

Currently, only Python 3.11 and 3.12 is supported. [pipx](https://github.com/pypa/pipx) is highly recommended:

```bash
pipx install fnug
```

## Demo

https://github.com/nickolaj-jepsen/fnug/assets/1039554/a0c47289-9670-4d81-aa24-39204caf8c89

## Config

Fnug is controlled by a `.fnug.yaml`, a minimal example would be:

### Minimal example:

Runs a single commands (without autorun)

```yaml
fnug_version: 0.1.0
name: fnug
commands:
  - name: hello
    cmd: echo world
```

### Git autorun example:

Uses git autorun to select commands (reselect by pressing "g")

```yaml
fnug_version: 0.1.0
name: fnug
commands:
  - name: hello
    cmd: echo world
    autorun:
      git: true
      path:
        - "./"
      regex:
        - "\\.fnug\\.yaml$"
```

### Watch autorun example:

Uses file autorun to monitor the file system for changes, and select commands accordingly, can be combined with git autorun

```yaml
fnug_version: 0.1.0
name: fnug
commands:
  - name: hello
    cmd: echo world
    autorun:
      watch: true
      path:
        - "./"
      regex:
        - "\\.fnug\\.yaml$"
```

### Advanced example:

View this projects [`.fnug.yaml`](.fnug.yaml) file for an advanced example

## Development

Build with [Rye](https://rye-up.com/)

### Install dependencies

`rye sync`

### Run in textual devmode

log terminal: `rye run console`
fnug (debug) terminal: `rye run debug`

### Run lint/tests

Use `fnug` of course 😄 (or `rye run fnug`)

## Thanks

Made possible by:
 - [textual](https://github.com/Textualize/textual)
 - [textual-terminal](https://github.com/mitosch/textual-terminal/)
 - [pyte](https://github.com/selectel/pyte)
 - [watchfiles](https://github.com/samuelcolvin/watchfiles)
