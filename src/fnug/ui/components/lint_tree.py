import time
from collections.abc import AsyncIterable, Callable, Iterator
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING, ClassVar, Literal, Optional

from rich.style import Style
from rich.text import Text
from textual import events
from textual.binding import Binding, BindingType
from textual.geometry import Offset, Region
from textual.message import Message
from textual.reactive import Reactive
from textual.widgets import Tree
from textual.widgets._tree import TOGGLE_STYLE, TreeNode
from textual.worker import Worker

if TYPE_CHECKING:
    from fnug.core import Command, CommandGroup, FnugCore

StatusType = Literal["success", "failure", "running", "pending"]


@dataclass
class LintTreeDataType:
    """Data type used by the lint tree."""

    id: str
    name: str
    type: Literal["group", "command"]
    command: Optional["Command"] = None
    group: Optional["CommandGroup"] = None
    status: StatusType | None = None
    selected: bool = False


def update_node(node: TreeNode[LintTreeDataType]):
    """Update/refresh a node (recursively)."""
    node.expand()
    node.refresh()
    if node.parent:
        update_node(node.parent)


def select_node(node: TreeNode[LintTreeDataType]):
    """Select a node, also expand all parents."""
    if node.data is None:
        return
    node.data.selected = True
    update_node(node)


def toggle_select_node(node: TreeNode[LintTreeDataType], override_value: bool | None = None):
    """Toggle a node (recursively if with children)."""
    if not node.data:
        return

    if node.data.type == "group" and override_value is None:
        # Instead of basing the override value on the previous state, we base it on the children states.
        result = sum_selected_commands(node)
        if result.selected == 0:
            override_value = True
        elif result.selected == result.total:
            override_value = False
        else:
            override_value = False
    elif override_value is None:
        override_value = not node.data.selected

    node.data.selected = override_value
    update_node(node)

    for child in node.children:
        if not child.data:
            continue

        toggle_select_node(child, override_value=override_value)


def all_commands(source_node: TreeNode[LintTreeDataType]) -> Iterator[TreeNode[LintTreeDataType]]:
    """Get all command children of a node (recursively)."""
    for child in source_node.children:
        if child.data and child.data.type == "command":
            yield child
        yield from all_commands(child)


def toggle_all_commands(source_node: TreeNode[LintTreeDataType], commands: list["Command"]):
    """Toggle all commands in a list."""
    for node in all_commands(source_node):
        if node.data and node.data.command in commands:
            toggle_select_node(node)


def select_all_commands(source_node: TreeNode[LintTreeDataType], commands: list["Command"]):
    """Select all commands (recursively)."""
    for node in all_commands(source_node):
        if node.data and node.data.command in commands:
            select_node(node)


@dataclass
class CommandSum:
    """A summary of the status of all selected commands."""

    selected: int = 0
    running: int = 0
    success: int = 0
    failure: int = 0
    total: int = 0


def sum_selected_commands(source_node: TreeNode[LintTreeDataType]) -> CommandSum:
    """Summarize the status of all selected commands (recursively)."""
    command_sum = CommandSum()
    for child in source_node.children:
        if child.data and child.data.type == "command":
            command_sum.total += 1
            if child.data.selected:
                command_sum.selected += 1
            if child.data.status == "running":
                command_sum.running += 1
            elif child.data.status == "success":
                command_sum.success += 1
            elif child.data.status == "failure":
                command_sum.failure += 1
        else:
            child_sum = sum_selected_commands(child)
            command_sum.total += child_sum.total
            command_sum.selected += child_sum.selected
            command_sum.running += child_sum.running
            command_sum.success += child_sum.success
            command_sum.failure += child_sum.failure
    return command_sum


def attach_command(
    tree: TreeNode[LintTreeDataType],
    command_group: "CommandGroup",
    cwd: Path,
    root: bool = False,
) -> dict[str, TreeNode[LintTreeDataType]]:
    """Attach a command group to a tree."""
    command_leafs: dict[str, TreeNode[LintTreeDataType]] = {}

    if not root:
        new_root = tree.add(
            command_group.name,
            data=LintTreeDataType(name=command_group.name, group=command_group, type="group", id=command_group.id),
        )
    else:
        new_root = tree

    for command in command_group.commands:
        command_leafs[command.id] = new_root.add_leaf(
            command.name,
            data=LintTreeDataType(name=command.name, type="command", command=command, id=command.id),
        )
    for child in command_group.children:
        child_commands = attach_command(new_root, child, cwd)
        command_leafs.update(child_commands)
    return command_leafs


async def watch_auto_task(
    source_node: TreeNode[LintTreeDataType], watcher_task: Callable[[], AsyncIterable[list["Command"]]]
):
    """Create a task that watches for changes in the filesystem and selects auto commands."""
    async for changed_commands in watcher_task():
        select_all_commands(source_node, changed_commands)


class LintTree(Tree[LintTreeDataType]):
    """A tree widget for displaying lint commands."""

    guide_depth = 3
    show_root = False
    watch_task: Worker[None] | None = None
    grabbed: Reactive[Offset | None] = Reactive(None)
    last_click: Reactive[dict[int, float | Literal["invalid"]]] = Reactive({})  # used for double click detection
    command_leafs: Reactive[dict[str, TreeNode[LintTreeDataType]]] = Reactive({})

    BINDINGS: ClassVar[list[BindingType]] = [
        # Movement
        Binding("left", "collapse_node", "Toggle", show=False),
        Binding("right", "expand_node", "Toggle", show=False),
        Binding("up", "cursor_up", "Cursor Up", show=False),
        Binding("down", "cursor_down", "Cursor Down", show=False),
        # Vim movement
        Binding("h", "collapse_node", "Toggle", show=False),
        Binding("l", "expand_node", "Toggle", show=False),
        Binding("k", "cursor_up", "Cursor Up", show=False),
        Binding("j", "cursor_down", "Cursor Down", show=False),
        # Controls
        Binding("r", "run", "Run"),
        Binding("ctrl+r", "exclusive_run", "Run fullscreen", show=False),
        Binding("s", "stop", "Stop", show=False),
        Binding("space", "toggle_select", "Select"),
        Binding("g", "select_git", "Select based on git changes", show=False),
        Binding("enter", "run_all", "Run selected commands"),
        Binding("c", "clear", "Clear terminal", show=False),
        Binding("q", "quit", "Quit", show=False),
    ]

    class RunCommand(Message):
        def __init__(self, node: TreeNode[LintTreeDataType]) -> None:
            self.node: TreeNode[LintTreeDataType] = node
            super().__init__()

        @property
        def control(self) -> Tree[LintTreeDataType]:
            """The tree that sent the message."""
            return self.node.tree

    class RunExclusiveCommand(Message):
        def __init__(self, node: TreeNode[LintTreeDataType]) -> None:
            self.node: TreeNode[LintTreeDataType] = node
            super().__init__()

        @property
        def control(self) -> Tree[LintTreeDataType]:
            """The tree that sent the message."""
            return self.node.tree

    class StopCommand(Message):
        def __init__(self, node: TreeNode[LintTreeDataType]) -> None:
            self.node: TreeNode[LintTreeDataType] = node
            super().__init__()

        @property
        def control(self) -> Tree[LintTreeDataType]:
            """The tree that sent the message."""
            return self.node.tree

    class RunAllCommand(Message):
        def __init__(self, nodes: list[TreeNode[LintTreeDataType]]) -> None:
            self.nodes: list[TreeNode[LintTreeDataType]] = nodes
            super().__init__()

        @property
        def control(self) -> Tree[LintTreeDataType]:
            """The tree that sent the message."""
            return self.nodes[0].tree

    class ClearTerminal(Message):
        def __init__(self, node: TreeNode[LintTreeDataType]) -> None:
            self.node: TreeNode[LintTreeDataType] = node
            super().__init__()

        @property
        def control(self) -> Tree[LintTreeDataType]:
            """The tree that sent the message."""
            return self.node.tree

    class OpenContextMenu(Message):
        def __init__(self, node: TreeNode[LintTreeDataType], event: events.Click) -> None:
            self.node: TreeNode[LintTreeDataType] = node
            self.click_event: events.Click = event
            super().__init__()

        @property
        def control(self) -> Tree[LintTreeDataType]:
            """The tree that sent the message."""
            return self.node.tree

        @property
        def is_active_node(self) -> bool:
            """Check if the node is the currently selected node."""
            return self.node.tree.cursor_node == self.node

    def __init__(
        self,
        core: "FnugCore",
        *,
        name: str | None = None,
        id: str | None = None,
        classes: str | None = None,
        disabled: bool = False,
    ):
        super().__init__("fnug", name=name, id=id, classes=classes, disabled=disabled)
        self.core = core
        self.config = core.config
        self.cwd = core.cwd

    def _get_label_region(self, line: int) -> Region | None:
        """Like parent, but offset by 2 to account for the icon."""
        region = super()._get_label_region(line)
        if region is None:
            return None
        return region._replace(x=region.x - 2)

    def update_status(self, command_id: str, status: StatusType):
        """Update the status of a command."""
        node = self.command_leafs[command_id]
        if node.data is None:
            return

        node.data.status = status
        if status == "success":
            node.data.selected = False
        update_node(node)

    def get_command(self, command_id: str) -> LintTreeDataType | None:
        """Get a command by ID."""
        if command_id not in self.command_leafs:
            return None
        return self.command_leafs[command_id].data

    def action_run(self) -> None:
        """Run a command."""
        if self.cursor_node is None:
            return
        self.post_message(self.RunCommand(self.cursor_node))

    def action_exclusive_run(self) -> None:
        """Run a command in exclusive mode."""
        if self.cursor_node is None:
            return
        self.post_message(self.RunExclusiveCommand(self.cursor_node))

    def action_stop(self) -> None:
        """Stop a running command."""
        if self.cursor_node is None:
            return
        self.post_message(self.StopCommand(self.cursor_node))

    def action_run_all(self) -> None:
        """Run all selected commands."""
        nodes = [
            node
            for node in all_commands(self.root)
            if node.data and node.data.selected and node.data.status not in ["running"]
        ]
        if len(nodes) > 0:
            self.post_message(self.RunAllCommand(nodes))

    def action_expand_node(self) -> None:
        """Expand a node (or enable it if it's a command)."""
        if self.cursor_node is None:
            return
        if self.cursor_node.data and self.cursor_node.data.type == "command":
            self.cursor_node.data.selected = True
            update_node(self.cursor_node)
        elif self.cursor_node.children:
            self.cursor_node.expand()

    def action_collapse_node(self) -> None:
        """Collapse a node (or disable it if it's a command)."""
        if self.cursor_node is None:
            return
        if self.cursor_node.data and self.cursor_node.data.type == "command":
            self.cursor_node.data.selected = False
            update_node(self.cursor_node)
        elif self.cursor_node.children:
            self.cursor_node.collapse()

    def action_toggle_select(self) -> None:
        """Toggle a node on click (recursively if with children)."""
        if self.cursor_node is None:
            return

        toggle_select_node(self.cursor_node)

    def action_select_git(self):
        """Select all git auto commands."""
        changed_commands = self.core.selected_commands()
        toggle_all_commands(self.root, changed_commands)

    def action_toggle_select_click(self, line: int, node: TreeNode[LintTreeDataType] | None = None):
        """Toggle a node on click."""
        node = node or self._get_node(line)
        self.last_click[line] = "invalid"
        if node and node.data:
            node.data.selected = not node.data.selected
            update_node(node)

    def action_clear(self):
        """Clear the terminal."""
        if self.cursor_node is None:
            return

        self.post_message(self.ClearTerminal(self.cursor_node))

    async def _right_click(self, event: events.Click, node: TreeNode[LintTreeDataType]) -> None:
        """Handle right click."""
        self.post_message(self.OpenContextMenu(node, event))

    async def _on_click(self, event: events.Click) -> None:
        # Handle double click
        meta = event.style.meta
        if "line" not in meta:
            return

        line = meta["line"]
        node = self._get_node(line)

        if node is None or node.data is None:
            return

        if event.button in [2, 3]:
            event.prevent_default()
            event.stop()
            await self._right_click(event, node)
            return

        if node.data.type == "group":
            return  # No need to handle double click on groups

        last_click = self.last_click.get(line)

        if last_click == "invalid":
            # if last "click" was from toggle_select_click, we don't want to handle it as a double click as it's either:
            # 1) already been double-clicked and handled by the rest of the method
            # 2) a single click on the "selection icon", and shouldn't be used to calculate a double click
            self.last_click.pop(line)
        elif last_click and time.time() - last_click < 0.5:
            self.action_run()
            self.last_click.pop(line, None)
        else:
            self.last_click[line] = time.time()

    def render_label(self, node: TreeNode[LintTreeDataType], base_style: Style, style: Style) -> Text:
        """Override the default label rendering to add icons and status."""
        node_label = node._label.copy()  # pyright: ignore reportPrivateUsage=false
        node_label.stylize(style)

        group_count = ("", base_style)
        dropdown = ("", base_style)

        if node._allow_expand:  # pyright: ignore reportPrivateUsage=false
            command_sum = sum_selected_commands(node)
            count_style = base_style + Style(color="#808080")

            group_count_pieces = [
                Text(" (", count_style),
                Text(str(command_sum.selected), count_style),
                Text("/", count_style),
                Text(str(command_sum.total), count_style),
                Text(")", count_style),
            ]

            if any([command_sum.running, command_sum.success, command_sum.failure]):
                status_count_pieces = [Text(" [", count_style)]

                if command_sum.success:
                    status_count_pieces.append(Text(str(command_sum.success), base_style + Style(color="green")))
                    if command_sum.running or command_sum.failure:
                        status_count_pieces.append(Text("|", count_style))

                if command_sum.running:
                    status_count_pieces.append(Text(str(command_sum.running), count_style))
                    if command_sum.failure:
                        status_count_pieces.append(Text("|", count_style))

                if command_sum.failure:
                    status_count_pieces.append(Text(str(command_sum.failure), base_style + Style(color="red")))

                status_count_pieces.append(Text("]", count_style))

                group_count_pieces = [
                    *status_count_pieces,
                    *group_count_pieces,
                ]

            group_count = Text.assemble(*group_count_pieces)
            dropdown = ("▼ ", base_style + TOGGLE_STYLE) if node.is_expanded else ("▶ ", base_style + TOGGLE_STYLE)

        command_status = getattr(node.data, "status", "")
        if command_status == "success":
            status = (" ✔ ", base_style + Style(color="green"))
        elif command_status == "failure":
            status = (" ✘ ", base_style + Style(color="red"))
        elif command_status == "running":
            status = (" 🕑", base_style + Style(color="yellow"))
        else:
            status = ("", base_style)

        selected = getattr(node.data, "selected", False)
        is_command = getattr(node.data, "type", "") == "command"
        if selected and is_command:
            selection = (
                "● ",
                base_style + Style(meta={"@mouse.up": f"toggle_select_click({node.line})"} if node.data else {}),
            )
        elif is_command:
            selection = (
                "○ ",
                base_style + Style(meta={"@mouse.up": f"toggle_select_click({node.line})"} if node.data else {}),
            )
        else:
            selection = ("", base_style)

        return Text.assemble(dropdown, selection, node_label, status, group_count)

    def _setup(self):
        self.command_leafs = attach_command(self.root, self.config, self.cwd, root=True)
        self.action_select_git()
        self.watch_task = self.run_worker(watch_auto_task(self.root, self.core.watch))

    def _on_mount(self, event: events.Mount):
        self.call_after_refresh(self._setup)

    async def _on_mouse_down(self, event: events.MouseDown) -> None:
        # We don't want mouse events on the scrollbar bubbling
        if event.x == self.size.width:
            self.capture_mouse()
            event.stop()

    def _on_mouse_capture(self, event: events.MouseCapture) -> None:
        self.grabbed = event.mouse_position

    async def _on_mouse_up(self, event: events.MouseUp) -> None:
        if self.grabbed:
            self.release_mouse()
            self.grabbed = None
        event.stop()

    def _on_mouse_move(self, event: events.MouseMove) -> None:
        if self.grabbed:
            self.styles.border_right = ("solid", "#a64c38")
            self.styles.width = event.screen_x + 1
        elif event.screen_x == self.size.width:  # Hover highlight
            self.styles.border_right = ("solid", "#a64c38")
        else:
            self.styles.border_right = ("solid", "#cf6a4c")
        event.stop()

    def _on_leave(self, _: events.Leave) -> None:
        """Clear any highlight when the mouse leaves the widget."""
        self.styles.border_right = ("solid", "#cf6a4c")

    def action_quit(self):
        """Quit the application."""
        self.app.exit()
