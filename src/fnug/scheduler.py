import asyncio
from collections.abc import Callable
from functools import lru_cache
from graphlib import TopologicalSorter
from pathlib import Path
from typing import ClassVar, Optional, TypeAlias

from textual.geometry import Size

from fnug.config import Config, ConfigCommand, ConfigCommandGroup, Dependency
from fnug.terminal_emulator import TerminalEmulator
from fnug.ui.components.lint_tree import StatusType

RunPlan: TypeAlias = "TopologicalSorter[str]"


def _generate_command_fs(group: ConfigCommandGroup, current_path: Path = Path()) -> dict[Path, ConfigCommand]:
    """Generate a "file system" of commands."""
    result: dict[Path, ConfigCommand] = {}
    for command in group.commands:
        result[current_path / command.name] = command
    for child in group.children:
        result.update(_generate_command_fs(child, current_path / child.name))
    return result


class CommandFS:
    """
    A "file system" of commands.

    Useful for getting commands by their (relative) path, paths by command and command by id.
    """

    def __init__(self, config: Config) -> None:
        self.fs: dict[Path, ConfigCommand] = _generate_command_fs(config)
        self._by_id = {command.id: command for command in self.fs.values()}
        self._reverse_fs = {command.id: path for path, command in self.fs.items()}

    def get_command(self, path: Path, relative: Path | None = None) -> ConfigCommand:
        """Get a command by (relative) path."""
        if relative:
            return self.fs[relative.parent / path]
        return self.fs[path]

    def get_by_id(self, command_id: str) -> ConfigCommand | None:
        """Get a command by id."""
        return self._by_id.get(command_id)

    def get_path(self, command_id: str) -> Path:
        """Get the path of a command."""
        return self._reverse_fs[command_id]


class Scheduler:
    """A scheduler for running commands when their dependencies are ready."""

    run_plan: Optional[RunPlan] = None  # noqa: UP007 -- python <3.10 is not happy with string aliases and unions
    terminal_emulators: ClassVar[dict[str, TerminalEmulator]] = {}

    def __init__(
        self, config: Config, update_fn: Callable[[str, StatusType], None], terminal_size: Size, cwd: Path
    ) -> None:
        self.config = config
        self.update_status = update_fn
        self.fs = CommandFS(config)
        self.terminal_size = terminal_size
        self.cwd = cwd

    def resize_terminals(self, size: Size) -> None:
        """Resize all terminal emulators."""
        self.terminal_size = size
        for terminal in self.terminal_emulators.values():
            terminal.dimensions = size

    @lru_cache(maxsize=128)  # noqa: B019
    def get_dependencies(self, command: ConfigCommand) -> list[tuple[ConfigCommand, Dependency]]:
        """Get the dependencies of a command."""
        command_path = self.fs.get_path(command.id)
        result: list[tuple[ConfigCommand, Dependency]] = []
        for dep in command.depends:
            dep_command = self.fs.get_command(dep.path, command_path)
            result.append((dep_command, dep))
            result.extend(self.get_dependencies(dep_command))
        return result

    def generate_run_plan(self, commands_ids: list[str]) -> RunPlan:
        """Generate a run plan for a list of commands."""
        graph: RunPlan = TopologicalSorter()
        for command_id in commands_ids:
            if command := self.fs.get_by_id(command_id):
                graph.add(
                    command.id,
                    *(cmd.id for cmd, dep in self.get_dependencies(command) if cmd.id in commands_ids or dep.always),
                )
        graph.prepare()
        return graph

    def schedule(self, commands_ids: list[str], force: bool = False) -> None:
        """Schedule commands to be run."""
        # TODO: Implement adding commands to an existing/running schedule
        # TODO: Implement force
        self.run_plan = self.generate_run_plan(commands_ids)

    def stop(self, commands_ids: list[str]) -> None:
        """Stop running commands."""
        ...

    def get_terminal_emulator(self, command_id: str) -> TerminalEmulator | None:
        """Get (or create) a terminal emulator for a command."""
        if command_id in self.terminal_emulators:
            return self.terminal_emulators[command_id]

        command = self.fs.get_by_id(command_id)
        if command is None:
            return None
        new_emulator = TerminalEmulator(self.terminal_size, can_focus=command.interactive)
        self.terminal_emulators[command_id] = new_emulator
        return new_emulator

    async def _run_command(self, command_id: str) -> bool:
        command = self.fs.get_by_id(command_id)
        terminal_emulator = self.get_terminal_emulator(command_id)
        if command is None or terminal_emulator is None:
            return False
        terminal_emulator.clear()

        cwd = self.cwd
        if command and command.cwd:
            cwd = cwd / command.cwd

        return await terminal_emulator.run_shell(command.cmd, cwd)

    async def run(self) -> None:
        """Run all scheduled commands."""
        running_commands: set[asyncio.Task[bool]] = set()
        if self.run_plan is None:
            return

        while True:
            # Run all ready commands
            for command_id in self.run_plan.get_ready():
                task = asyncio.create_task(self._run_command(command_id))
                self.update_status(command_id, "running")
                setattr(task, "command_id", command_id)
                running_commands.add(task)

            # Break if no commands are running
            if not running_commands:
                break

            # Wait for any command to finish, and reassign running_commands to the remaining tasks
            done_tasks, running_commands = await asyncio.wait(running_commands, return_when=asyncio.FIRST_COMPLETED)

            # Update the status of the finished commands
            for task in done_tasks:
                command_id = getattr(task, "command_id", None)
                if not isinstance(command_id, str):
                    raise ValueError("Task is missing command_id attribute")  # just to make pyright happy
                if task.result():
                    self.update_status(command_id, "success")
                    self.run_plan.done(command_id)
                else:
                    self.update_status(command_id, "failure")
