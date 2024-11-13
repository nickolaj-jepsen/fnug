import subprocess
from collections.abc import Callable
from dataclasses import dataclass
from functools import partial
from pathlib import Path
from typing import ClassVar

import click
import rich
from textual import events, on
from textual.app import App, ComposeResult
from textual.binding import Binding, BindingType
from textual.command import Hit, Hits, Provider
from textual.containers import Horizontal
from textual.widgets import Footer
from textual.widgets._tree import TreeNode
from textual.worker import Worker

from fnug.core import CommandGroup, FnugCore
from fnug.terminal_emulator import (
    TerminalEmulator,
    any_key_message,
    failure_message,
    start_message,
    stopped_message,
    success_message,
)
from fnug.ui.components.context_menu import ContextMenu
from fnug.ui.components.lint_tree import (
    LintTree,
    LintTreeDataType,
    all_commands,
    sum_selected_commands,
    toggle_select_node,
    update_node,
)
from fnug.ui.components.terminal import Terminal


class _CommandProvider(Provider):
    commands: dict[str, TreeNode[LintTreeDataType]]

    async def startup(self) -> None:
        app = self.app
        if not isinstance(app, FnugApp):
            return

        self.commands = app.lint_tree.command_leafs

    async def search(self, query: str) -> Hits:
        """Search for Python files."""
        app = self.app
        if not isinstance(app, FnugApp):
            return

        matcher = self.matcher(query)

        for node_id, node in self.commands.items():
            if not node.data:
                continue

            score = matcher.match(node_id)
            if score > 0:
                callback: partial[Callable[[], None]] = partial(app.display_terminal, node_id)
                yield Hit(
                    score,
                    match_display=matcher.highlight(node.data.name),
                    command=callback,
                    text=node.data.name,
                    help=node_id,
                )


@dataclass
class TerminalInstance:
    """A collection of tasks and emulator for a terminal."""

    emulator: TerminalEmulator
    run_task: Worker[None]


class FnugApp(App[None]):
    """A Textual app to manage stopwatches."""

    COMMANDS: ClassVar[set[type[Provider] | Callable[[], type[Provider]]]] = {_CommandProvider}
    CSS_PATH = "app.tcss"

    BINDINGS: ClassVar[list[BindingType]] = [Binding("escape", "quit", "Quit", show=False)]

    terminals: ClassVar[dict[str, TerminalInstance]] = {}
    active_terminal_id: str | None = None
    display_task: Worker[None] | None = None

    def __init__(self, core: FnugCore):
        super().__init__()
        self.core = core

    @classmethod
    def from_group(cls, group: CommandGroup, cwd: Path) -> "FnugApp":
        """
        Create an app from a config object.

        :param group: A command group.
        :param cwd: The current working directory.
        """
        return cls(FnugCore.from_group(group, cwd))  # pyright: ignore [reportUnknownMemberType]

    @classmethod
    def from_config_file(cls, config_file: Path | None = None) -> "FnugApp":
        """
        Create an app from a config file.

        :param config_file: The path to the config file.
        """
        return cls(FnugCore.from_config_file(config_file))

    def compose(self) -> ComposeResult:
        """Create child widgets for the app."""
        with Horizontal(id="main"):
            yield LintTree(self.core, id="lint-tree", classes="custom-scrollbar")
            yield Terminal(id="terminal", classes="custom-scrollbar")
        yield Footer(show_command_palette=False)

    @property
    def lint_tree(self) -> LintTree:
        """The lint tree."""
        return self.query_one("#lint-tree", LintTree)

    @property
    def _terminal(self) -> Terminal:
        return self.query_one("#terminal", Terminal)

    @on(LintTree.NodeHighlighted, "#lint-tree")
    def _switch_terminal(self, event: LintTree.NodeHighlighted[LintTreeDataType]):
        if event.node.data is not None:
            self.display_terminal(event.node.data.id)

    @on(LintTree.RunCommand, "#lint-tree")
    def _action_run_command(self, event: LintTree.RunCommand):
        if event.node.data is not None:
            self._run_command(event.node.data)

    @on(LintTree.RunExclusiveCommand, "#lint-tree")
    def _action_run_exclusive_command(self, event: LintTree.RunExclusiveCommand):
        if event.node.data is None or event.node.data.command is None:
            return

        self._run_command_fullscreen(event.node.data)

    @on(LintTree.StopCommand, "#lint-tree")
    def _action_stop_command(self, event: LintTree.StopCommand):
        if event.node.data:
            self._stop_command(event.node.data.id)

    @on(LintTree.ClearTerminal, "#lint-tree")
    def _action_clear_terminal(self, event: LintTree.ClearTerminal):
        if event.node.data:
            self._clear_terminal(event.node.data.id)

    @on(LintTree.RunAllCommand, "#lint-tree")
    def _run_all(self, event: LintTree.RunAllCommand):
        cursor_id = getattr(self.lint_tree.cursor_node, "id", None)

        for node in event.nodes:
            if node.data is not None:
                self._run_command(node.data, background=cursor_id != node.id)

    async def _handle_context_menu(
        self, node: TreeNode[LintTreeDataType], event: events.Click, active_node: bool = False
    ):
        if node.data is None:
            return

        tree = self.lint_tree

        def handle_selection(selection: str | None):
            if node.data is None or selection is None:
                return
            cursor_id = getattr(tree.cursor_node, "id", None)

            if selection == "run":
                self._run_command(node.data, background=not active_node)
            elif selection == "run-fullscreen":
                self._run_command_fullscreen(node.data)
            elif selection == "restart":
                self._stop_command(node.data.id)
                self._run_command(node.data)
            elif selection == "stop":
                self._stop_command(node.data.id)
            elif selection == "stop-clear":
                self._stop_command(node.data.id)
                self._clear_terminal(node.data.id)
            elif selection == "clear":
                self._clear_terminal(node.data.id)
            elif selection == "run-all":
                for command in all_commands(node):
                    if command.data is not None:
                        self._run_command(command.data, background=cursor_id != command.id)
            elif selection == "stop-all":
                for command in all_commands(node):
                    if command.data is not None:
                        self._stop_command(command.data.id)
            elif selection == "rerun-failures":
                for command in all_commands(node):
                    if command.data is not None and command.data.status == "failure":
                        self._run_command(command.data, background=cursor_id != command.id)
            elif selection == "select-all":
                toggle_select_node(node, True)
            elif selection == "deselect-all":
                toggle_select_node(node, False)

        if node.data.type == "group":
            commands = {
                "run-all": "Run all",
            }

            sums = sum_selected_commands(node)
            if sums.running:
                commands["stop-all"] = "Stop all"
            if sums.total != sums.selected:
                commands["select-all"] = "Select all"
            if sums.selected:
                commands["deselect-all"] = "Deselect all"
            if sums.failure:
                commands["rerun-failures"] = "Re-run failures"

        elif node.data.status == "running":
            commands = {
                "restart": "Restart",
                "stop": "Stop",
                "stop-clear": "Stop and clear",
            }
        elif node.data.status in ("failure", "success"):
            commands = {
                "run": "Re-run",
                "run-fullscreen": "Re-run (fullscreen)",
                "clear": "Clear",
            }
        else:
            commands = {
                "run": "Run",
                "run-fullscreen": "Run (fullscreen)",
            }

        await self.push_screen(
            ContextMenu(
                commands,
                event,
            ),
            handle_selection,
        )

    @on(LintTree.OpenContextMenu, "#lint-tree")
    async def _linttree_open_context_menu(self, event: LintTree.OpenContextMenu):
        await self._handle_context_menu(event.node, event.click_event, active_node=event.is_active_node)

    @on(Terminal.OpenContextMenu, "#terminal")
    async def _terminal_open_context_menu(self, event: Terminal.OpenContextMenu):
        node = self.lint_tree.cursor_node
        if node is None or node.data is None or node.data.type == "group":
            return
        await self._handle_context_menu(node, event.click_event, active_node=True)

    def display_terminal(self, command_id: str):
        """Display the terminal for a command."""
        if self.display_task is not None:
            self.display_task.cancel()

        tree = self.lint_tree

        if tree.cursor_node and tree.cursor_node.data and tree.cursor_node.data.id != command_id:
            new_node = tree.command_leafs.get(command_id)
            if new_node:
                update_node(new_node)
                self.lint_tree.select_node(new_node)

        terminal = self.terminals.get(command_id)
        self.display_task = self.run_worker(
            self._terminal.attach_emulator(terminal.emulator if terminal else None), name="display_task"
        )

    def _run_command(self, command: LintTreeDataType, background: bool = False):
        if command.type != "command":
            return

        tree = self.lint_tree
        tree.update_status(command.id, "running")

        te = TerminalEmulator(
            self._terminal.size,
            can_focus=command.command.interactive if command.command else False,
        )

        async def run_shell():
            cwd = self.core.cwd
            if command.command and command.command.cwd:
                cwd = cwd / command.command.cwd

            if command.command and await te.run_shell(command.command.cmd, cwd):
                tree.update_status(command.id, "success")
            else:
                tree.update_status(command.id, "failure")

        if command.id in self.terminals:
            self.terminals[command.id].run_task.cancel()

        self.terminals[command.id] = TerminalInstance(
            emulator=te,
            run_task=self.run_worker(run_shell()),
        )
        if not background:
            self.display_terminal(command.id)

    def _run_command_fullscreen(self, command: LintTreeDataType):
        # stop existing command, if it's running
        self._stop_command(command.id)

        if not command.command:
            return

        with self.suspend():
            click.clear()
            rich.print(start_message(command.command.cmd), end="")
            process = subprocess.run(command.command.cmd, shell=True)  # noqa: S602
            exit_code = process.returncode
            if exit_code == 0:
                rich.print(success_message())
                status = "success"
            else:
                rich.print(failure_message(exit_code))
                status = "failure"

            rich.print(any_key_message())
            click.pause("")
        self.lint_tree.update_status(command.id, status)

    def _stop_command(self, command_id: str):
        tree = self.lint_tree

        command = tree.get_command(command_id)
        if command is None or command.status != "running":
            return

        if command_id in self.terminals:
            self.terminals[command_id].emulator.echo("")  # makes sure the cursor position is reset
            self.terminals[command_id].emulator.echo(stopped_message())
            self.terminals[command_id].run_task.cancel()
            tree.update_status(command_id, "failure")

    def _clear_terminal(self, command_id: str):
        tree = self.lint_tree

        command = tree.get_command(command_id)
        if command is None or command.status == "running":
            return

        if command_id in self.terminals:
            self.terminals[command_id].emulator.clear()
            tree.update_status(command_id, "pending")
