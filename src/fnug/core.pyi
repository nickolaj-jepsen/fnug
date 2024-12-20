# This file is automatically generated by pyo3_stub_gen
# ruff: noqa: E501, F401

import os
import pathlib
import typing

class Auto:
    r"""
    Automation rules that determine when commands should execute
    
    # Examples
    
    ```python
    # Watch for git changes in specific paths matching regex patterns
    auto = Auto(
        watch=True,
        git=True,
        path=["src/", "tests/"],
        regex=[".*\\.rs$", ".*\\.toml$"]
    )
    ```
    """
    watch: bool
    git: bool
    path: list[str]
    regex: list[str]
    always: bool
    def __new__(cls,watch = ...,git = ...,path = ...,regex = ...,always = ...): ...

class Command:
    r"""
    A single executable task with its configuration and automation rules
    
    Commands are the leaf nodes in the command tree. Each command has:
    - A unique identifier
    - A working directory (inherited from parent group if not specified)
    - Automation rules (merged with parent group rules)
    - An executable shell command
    
    # Examples
    
    ```python
    cmd = Command(
        name="build",
        cmd="cargo build",
    )
    ```
    """
    id: str
    name: str
    cmd: str
    cwd: str
    interactive: bool
    auto: Auto
    def __new__(cls,name,cmd,id = ...,cwd = ...,interactive = ...,auto = ...): ...
    def __eq__(self, other:Command) -> bool:
        ...


class CommandGroup:
    r"""
    Hierarchical grouping of related commands
    
    CommandGroups form the nodes of a command tree, allowing logical organization
    of related commands. Groups can define common settings that are inherited by
    their children:
    
    - Working directory - Children execute relative to parent's directory
    - Automation rules - Children inherit and can extend parent rules
    - File patterns - Children can add to parent's watch patterns
    
    # Examples
    
    ```python
    group = CommandGroup(
        name="backend",
        auto=Auto(git=True, path=["backend/"]),
        commands=[Command(name="test", cmd="cargo test")],
        children=[CommandGroup(name="api", ...)]
    )
    ```
    """
    id: str
    name: str
    auto: Auto
    cwd: str
    commands: list[Command]
    children: list[CommandGroup]
    def __new__(cls,name,id = ...,auto = ...,cwd = ...,commands = ...,children = ...): ...
    def as_yaml(self) -> str:
        ...


class FnugCore:
    config: CommandGroup
    cwd: typing.Any
    @staticmethod
    def from_group(command_group:CommandGroup, cwd:str | os.PathLike | pathlib.Path) -> FnugCore:
        r"""
        Creates a new FnugCore instance from an existing CommandGroup
        
        This method is useful when you want to programmatically create a command structure
        rather than loading it from a configuration file.
        """
        ...

    @staticmethod
    def from_config_file(config_file = ...) -> FnugCore:
        r"""
        Creates a new FnugCore instance by loading a configuration file
        
        If no configuration file is specified, Fnug will search for a .fnug.yaml,
        .fnug.yml, or .fnug.json file in the current directory and its parents.
        
        # Errors
        
        - Raises `PyFileNotFoundError` if the config file doesn't exist or can't be read
        - Raises `PyValueError` if the config file contains invalid YAML/JSON
        
        # Examples
        
        ```python
        # Load from specific file
        core = FnugCore.from_config_file(".fnug.yaml")
        
        # Auto-detect config file
        core = FnugCore.from_config_file()
        ```
        """
        ...

    def all_commands(self) -> list[Command]:
        r"""
        Returns a list of all commands in the configuration
        
        This includes commands from all nested command groups.
        """
        ...

    def watch(self) -> WatcherIterator:
        r"""
        Returns a async iterator that watches for file system changes, yielding commands to run
        """
        ...

    def selected_commands(self) -> list[Command]:
        r"""
        Returns commands that have detected git changes in their watched paths, or have `always=True`
        """
        ...


class OutputIterator:
    def __aiter__(self) -> OutputIterator:
        ...

    def __anext__(self) -> typing.Any:
        ...


class Process:
    output: OutputIterator
    can_focus: bool
    def __new__(cls,command:Command, width:int, height:int): ...
    def status(self) -> typing.Any:
        ...

    def kill(self) -> None:
        ...

    def scroll(self, lines:int) -> None:
        ...

    def set_scroll(self, rows:int) -> None:
        ...

    def resize(self, width:int, height:int) -> None:
        ...

    def click(self, x:int, y:int) -> None:
        ...

    def clear(self) -> None:
        ...

    def write(self, data:typing.Sequence[int]) -> None:
        ...


class WatcherIterator:
    def __aiter__(self) -> WatcherIterator:
        ...

    def __anext__(self) -> typing.Any:
        ...


