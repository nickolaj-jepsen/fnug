# Fnug

[![image](https://img.shields.io/pypi/v/fnug.svg)](https://pypi.python.org/pypi/fnug)
[![image](https://img.shields.io/pypi/l/fnug.svg)](https://pypi.python.org/pypi/fnug)
[![image](https://img.shields.io/pypi/pyversions/fnug.svg)](https://pypi.python.org/pypi/fnug)
[![Actions status](https://github.com/nickolaj-jepsen/fnug/workflows/CI/badge.svg)](https://github.com/nickolaj-jepsen/fnug/actions)

Fnug is a command runner, well actually it's a terminal multiplexer (like [tmux](https://github.com/tmux/tmux/wiki)),
but with a focus on running all your lint and test commands, at once, and displaying the result of those command.
Confused? Watch the [demo](#demo)

![screenshot](https://github.com/nickolaj-jepsen/fnug/assets/1039554/3fd812fc-e1dc-4dd2-86eb-de91dc8e027f)

## Features

- User-friendly terminal interface, with 100% support for both keyboard and mouse navigation
- Git integration, automatically select lints and tests that's should be run, based on what files have uncommitted
  changes
- Track file changes, and selects commands based on the changed files
- Terminal emulation with scroll back, for those really long error messages

## Installation

Python 3.10 or later is required.

[pipx](https://github.com/pypa/pipx) or [rye tool](https://rye-up.com/guide/tools/) are highly recommended:

```bash
# Recommended
pipx install fnug
# (or with rye tool)
rye install fnug
# Via pip (NOT RECOMMENDED)
pip install fnug
```

## Usage

To start `fnug` you only need to run it in a directory with a `.fnug.yaml` configuration file (or with the argument
`-c path/to/config.yaml`)

### Config

Fnug is controlled by a `.fnug.yaml` configuration file (or `.fnug.json` if thats more your speed).

#### Minimal example:

Runs a single commands

```yaml
fnug_version: 0.1.0
name: fnug
commands:
  - name: hello
    cmd: echo world
```

#### Git selection example:

Uses git auto to select commands based on what files have uncommitted changes (reselect by pressing "g")

```yaml
fnug_version: 0.1.0
name: fnug
commands:
  - name: hello
    cmd: echo world
    auto:
      git: true
      path:
        - "./"
      regex:
        - "\\.fnug\\.yaml$"
```

#### File watching example:

Uses file watching to monitor the file system for changes, and select commands accordingly, can be combined with git
auto

```yaml
fnug_version: 0.1.0
name: fnug
commands:
  - name: hello
    cmd: echo world
    auto:
      watch: true
      path:
        - "./"
      regex:
        - "\\.fnug\\.yaml$"
```

#### Advanced example:

View this projects [`.fnug.yaml`](.fnug.yaml) file for an advanced example

## Demo

https://github.com/nickolaj-jepsen/fnug/assets/1039554/8f8a4d34-8beb-4fb4-9bbc-6fd0a4a384be
