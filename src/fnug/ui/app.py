import asyncio
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
from textual.geometry import Size
from textual.scrollbar import ScrollBar, ScrollDown, ScrollTo, ScrollUp
from textual.widgets import Footer
from textual.widgets._tree import TreeNode
from textual.worker import Worker

from fnug.config import Config
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
                yield Hit(
                    score,
                    matcher.highlight(node.data.name),
                    partial(app.display_terminal, node_id),
                    text=node.data.name,
                    help=node_id,
                )


@dataclass
class TerminalInstance:
    """A collection of tasks and emulator for a terminal."""

    emulator: TerminalEmulator
    reader_task: Worker[None]
    run_task: Worker[None]


class FnugApp(App[None]):
    """A Textual app to manage stopwatches."""

    COMMANDS: ClassVar[set[type[Provider] | Callable[[], type[Provider]]]] = {_CommandProvider}
    CSS_PATH = "app.tcss"

    BINDINGS: ClassVar[list[BindingType]] = [Binding("escape", "quit", "Quit", show=False)]

    terminals: ClassVar[dict[str, TerminalInstance]] = {}
    active_terminal_id: str | None = None
    display_task: Worker[None] | None = None
    update_ready = asyncio.Event()

    def __init__(self, config: Config, cwd: Path | None = None):
        super().__init__()
        self.cwd = (cwd or Path.cwd()).resolve()
        self.config = config

    def compose(self) -> ComposeResult:
        """Create child widgets for the app."""
        with Horizontal(id="main"):
            yield LintTree(self.config, cwd=self.cwd, id="lint-tree")
            yield Terminal(id="terminal")
            yield ScrollBar()
        yield Footer()

    @property
    def _active_terminal_emulator(self) -> TerminalInstance | None:
        if self.active_terminal_id is None:
            return None

        return self.terminals[self.active_terminal_id]

    @property
    def lint_tree(self) -> LintTree:
        """The lint tree."""
        return self.query_one("#lint-tree", LintTree)

    @property
    def _terminal(self) -> Terminal:
        return self.query_one("#terminal", Terminal)

    @property
    def _scrollbar(self) -> ScrollBar:
        return self.query_one("ScrollBar", ScrollBar)

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
    def _action_stop_command(self, event: LintTree.RunCommand):
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

    @on(LintTree.Resize, "#lint-tree")
    async def _tree_resize(self, event: LintTree.Resize):
        await self._on_resize()

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

    @on(ScrollDown)
    def _scroll_down(self, event: ScrollTo) -> None:
        if self._active_terminal_emulator is not None:
            self._active_terminal_emulator.emulator.scroll("down")
            self.update_ready.set()

    @on(ScrollUp)
    def _scroll_up(self, event: ScrollTo) -> None:
        if self._active_terminal_emulator is not None:
            self._active_terminal_emulator.emulator.scroll("up")
            self.update_ready.set()

    async def _display_terminal(self, command_id: str):
        scrollbar = self._scrollbar
        terminal = self.terminals.get(command_id)
        if terminal is None:
            scrollbar.window_virtual_size = 0
            return

        ui = self._terminal
        self.active_terminal_id = command_id
        task = ui.attach_emulator(terminal.emulator, self.update_ready, scrollbar)
        ui.update_scrollbar(scrollbar)
        await task

    def display_terminal(self, command_id: str):
        """Display the terminal for a command."""
        if self.display_task is not None:
            self.display_task.cancel()
            self._terminal.clear()

        tree = self.lint_tree

        if tree.cursor_node and tree.cursor_node.data and tree.cursor_node.data.id != command_id:
            new_node = tree.command_leafs.get(command_id)
            if new_node:
                update_node(new_node)
                self.lint_tree.select_node(new_node)

        self.display_task = self.run_worker(self._display_terminal(command_id), name="display_task")

    def _run_command(self, command: LintTreeDataType, background: bool = False):
        if command.type != "command":
            return

        tree = self.lint_tree
        tree.update_status(command.id, "running")

        te = TerminalEmulator(
            self._terminal_size(),
            self.update_ready,
            can_focus=command.command.interactive if command.command else False,
        )

        async def run_shell():
            cwd = self.cwd
            if command.command and command.command.cwd:
                cwd = cwd / command.command.cwd

            if command.command and await te.run_shell(command.command.cmd, cwd):
                tree.update_status(command.id, "success")
            else:
                tree.update_status(command.id, "failure")

        if command.id in self.terminals:
            self.terminals[command.id].run_task.cancel()
            self.terminals[command.id].reader_task.cancel()

        self.terminals[command.id] = TerminalInstance(
            emulator=te,
            reader_task=self.run_worker(te.reader()),
            run_task=self.run_worker(run_shell()),
        )
        if not background:
            self.display_terminal(command.id)
        self.update_ready.set()

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
            self.terminals[command_id].reader_task.cancel()
            self.update_ready.set()
            tree.update_status(command_id, "failure")

    def _clear_terminal(self, command_id: str):
        tree = self.lint_tree

        command = tree.get_command(command_id)
        if command is None or command.status == "running":
            return

        if command_id in self.terminals:
            self.terminals[command_id].emulator.clear()
            self.update_ready.set()
            tree.update_status(command_id, "pending")

    def _terminal_size(self) -> Size:
        scrollbar_width = 1
        return Size(
            width=self.size.width - self.lint_tree.outer_size.width - scrollbar_width, height=self.size.height - 1
        )

    def _on_mount(self):
        self.call_after_refresh(self._on_resize)

    async def _on_resize(self, event: events.Resize | None = None) -> None:
        size = self._terminal_size()
        scrollbar = self._scrollbar
        scrollbar.window_size = size.height
        scrollbar.window_virtual_size = 0

        for terminal in self.terminals.values():
            self._scrollbar.window_size = size.height
            terminal.emulator.dimensions = size
