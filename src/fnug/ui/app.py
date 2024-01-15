import asyncio
from dataclasses import dataclass
from pathlib import Path
from typing import ClassVar

from textual import on
from textual.app import App, ComposeResult
from textual.containers import Horizontal
from textual.geometry import Size
from textual.widgets import Footer

from fnug.config import ConfigRoot
from fnug.terminal_emulator import TerminalEmulator
from fnug.ui.components.lint_tree import LintTree, LintTreeDataType
from fnug.ui.components.terminal import Terminal


@dataclass
class TerminalInstance:
    emulator: TerminalEmulator
    reader_task: asyncio.Task[None]
    run_task: asyncio.Task[None]


class FnugApp(App[None]):
    """A Textual app to manage stopwatches."""

    CSS_PATH = "app.tcss"

    terminals: ClassVar[dict[str, TerminalInstance]] = {}
    display_task: asyncio.Task[None] | None = None
    update_ready = asyncio.Event()

    def __init__(self, config: ConfigRoot, cwd: Path | None = None):
        super().__init__()
        self.cwd = cwd or Path.cwd()
        self.config = config

    def compose(self) -> ComposeResult:
        """Create child widgets for the app."""
        with Horizontal():
            yield LintTree(self.config, id="lint-tree")
            yield Terminal(id="terminal")
        yield Footer()

    @on(LintTree.NodeHighlighted, "#lint-tree")
    def _switch_terminal(self, event: LintTree.NodeHighlighted[LintTreeDataType]):
        if self.display_task is not None:
            self.display_task.cancel()
            self.query_one("#terminal", Terminal).clear()
        if event.node.data:
            self.display_task = asyncio.create_task(self.display_terminal(event.node.data.id))

    @on(LintTree.RunCommand, "#lint-tree")
    def _run_command(self, event: LintTree.RunCommand):
        if event.node.data is not None:
            self.run_command(event.node.data)

    @on(LintTree.StopCommand, "#lint-tree")
    def _stop_command(self, event: LintTree.RunCommand):
        if event.node.data:
            self.stop_command(event.node.data.id)

    @on(LintTree.RunAllCommand, "#lint-tree")
    def _run_all(self, event: LintTree.RunAllCommand):
        for node in event.nodes:
            if node.data is not None:
                self.run_command(node.data)

    async def display_terminal(self, command_id: str):
        terminal = self.terminals.get(command_id)
        if terminal is None:
            return

        ui = self.query_one("#terminal", Terminal)
        self.update_ready.set()
        await ui.attach_emulator(terminal.emulator, self.update_ready)

    def run_command(self, command: LintTreeDataType):
        if command.type != "command":
            return

        tree = self.query_one("#lint-tree", LintTree)
        tree.update_status(command.id, "running")

        te = TerminalEmulator(self.terminal_size(), self.update_ready)

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
        if self.display_task is not None:
            self.display_task.cancel()

        self.terminals[command.id] = TerminalInstance(
            emulator=te,
            reader_task=asyncio.create_task(te.reader()),
            run_task=asyncio.create_task(run_shell()),
        )
        self.display_task = asyncio.create_task(self.display_terminal(command.id))
        self.update_ready.set()

    def stop_command(self, command_id: str):
        tree = self.query_one("#lint-tree", LintTree)

        if command_id in self.terminals:
            self.terminals[command_id].emulator.clear()
            self.terminals[command_id].run_task.cancel()
            self.terminals[command_id].reader_task.cancel()
            self.update_ready.set()
            tree.update_status(command_id, "pending")

    def terminal_size(self) -> Size:
        return Size(width=self.size.width - 30, height=self.size.height - 1)

    async def on_resize(self, event: None) -> None:
        for terminal in self.terminals.values():
            terminal.emulator.dimensions = self.terminal_size()
