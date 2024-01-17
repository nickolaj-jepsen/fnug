import asyncio
import re
from collections import defaultdict
from dataclasses import dataclass
from pathlib import Path
from typing import ClassVar, Literal, Iterator, DefaultDict

from rich.style import Style
from rich.text import Text
from textual import events
from textual.binding import Binding, BindingType
from textual.geometry import Region, Offset
from textual.message import Message
from textual.reactive import Reactive
from textual.widgets import Tree
from textual.widgets._tree import TreeNode, TOGGLE_STYLE
from watchfiles import awatch  # pyright: ignore reportUnknownVariableType

from fnug.config import ConfigRoot, ConfigCommandGroup, ConfigCommand
from fnug.git import detect_repo_changes

StatusType = Literal["success", "failure", "running", "pending"]


@dataclass
class LintTreeDataType:
    id: str
    name: str
    type: Literal["group", "command"]
    command: ConfigCommand | None = None
    group: ConfigCommandGroup | None = None
    status: StatusType | None = None
    selected: bool = False


def update_node(node: TreeNode[LintTreeDataType]):
    """Updates a node (recursively)"""

    node.expand()
    node.refresh()
    if node.parent:
        update_node(node.parent)


def select_node(node: TreeNode[LintTreeDataType]):
    """
    Selects a node

    Also expands all parents
    """
    if node.data is None:
        return
    node.data.selected = True
    update_node(node)


def toggle_select_node(node: TreeNode[LintTreeDataType], override_value: bool | None = None):
    """
    Toggle a node (recursively if with children)
    """
    if not node.data:
        return

    if override_value is None:
        override_value = not node.data.selected
    node.data.selected = override_value
    update_node(node)

    for child in node.children:
        if child.data:
            continue

        toggle_select_node(child, override_value=override_value)


def all_nodes(source_node: TreeNode[LintTreeDataType]) -> Iterator[TreeNode[LintTreeDataType]]:
    """
    Get all nodes (recursively)
    """
    yield source_node
    for child in source_node.children:
        yield from all_nodes(child)


def all_commands(source_node: TreeNode[LintTreeDataType]) -> Iterator[TreeNode[LintTreeDataType]]:
    """
    Get all command children of a node (recursively)
    """
    for child in source_node.children:
        if child.data and child.data.type == "command":
            yield child
        yield from all_commands(child)


def select_git_autorun(cwd: Path, node: TreeNode[LintTreeDataType]):
    if not node.data or not node.data.command:
        return

    autorun = node.data.command.autorun

    if autorun.always is True:
        return select_node(node)

    if autorun.git and autorun.path:
        for path in autorun.path:
            if detect_repo_changes(cwd / path, autorun.regex):
                return select_node(node)


@dataclass
class CommandSum:
    selected: int = 0
    running: int = 0
    success: int = 0
    failure: int = 0
    total: int = 0


def sum_selected_commands(source_node: TreeNode[LintTreeDataType]) -> CommandSum:
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
    command_group: ConfigCommandGroup,
    cwd: Path,
    path: list[str] | None = None,
    root: bool = False,
) -> dict[str, TreeNode[LintTreeDataType]]:
    command_leafs: dict[str, TreeNode[LintTreeDataType]] = {}
    new_path = [command_group.name] if path is None else [*path, command_group.name]

    if not root:
        new_root = tree.add(
            command_group.name,
            data=LintTreeDataType(name=command_group.name, group=command_group, type="group", id=".".join(new_path)),
        )
    else:
        new_root = tree

    for command in command_group.commands:
        command_id = ".".join([*new_path, command.name])
        command_leafs[command_id] = new_root.add_leaf(
            command.name,
            data=LintTreeDataType(name=command.name, type="command", command=command, id=command_id),
        )
    for child in command_group.children:
        child_commands = attach_command(new_root, child, cwd, path=new_path)
        command_leafs.update(child_commands)
    return command_leafs


async def watch_autorun_task(command_nodes: Iterator[TreeNode[LintTreeDataType]], cwd: Path):
    paths: DefaultDict[Path, list[TreeNode[LintTreeDataType]]] = defaultdict(list)

    for node in command_nodes:
        if not node.data or not node.data.command or not node.data.command.autorun.path:
            continue

        for path in node.data.command.autorun.path:
            paths[cwd / path].append(node)

    async for change_set in awatch(*paths.keys(), step=500, debounce=5000):
        for _, change_str in change_set:
            change = Path(change_str)
            # Only trigger if the path is in the tree
            nested_active_nodes = [nodes for path, nodes in paths.items() if path in change.parents]
            active_nodes = [node for nodes in nested_active_nodes for node in nodes]  # Flatten

            for node in active_nodes:
                if not node.data or not node.data.command:
                    continue

                if node.data.command.autorun.regex:
                    if any(re.search(r, change_str) for r in node.data.command.autorun.regex):
                        select_node(node)
                else:
                    select_node(node)


class LintTree(Tree[LintTreeDataType]):
    guide_depth = 3
    show_root = False
    watch_task: asyncio.Task[None] | None = None
    grabbed: Reactive[Offset | None] = Reactive(None)

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
        Binding("r", "run", "(Re)run command"),
        Binding("s", "stop", "Stop command"),
        Binding("space", "toggle_select", "Select"),
        Binding("a", "autoselect", "Auto select"),
        Binding("enter", "run_all", "Run all selected commands"),
    ]

    class RunCommand(Message):
        def __init__(self, node: TreeNode[LintTreeDataType]) -> None:
            self.node: TreeNode[LintTreeDataType] = node
            super().__init__()

        @property
        def control(self) -> Tree[LintTreeDataType]:
            return self.node.tree

    class StopCommand(Message):
        def __init__(self, node: TreeNode[LintTreeDataType]) -> None:
            self.node: TreeNode[LintTreeDataType] = node
            super().__init__()

        @property
        def control(self) -> Tree[LintTreeDataType]:
            return self.node.tree

    class RunAllCommand(Message):
        def __init__(self, nodes: list[TreeNode[LintTreeDataType]]) -> None:
            self.nodes: list[TreeNode[LintTreeDataType]] = nodes
            super().__init__()

        @property
        def control(self) -> Tree[LintTreeDataType]:
            return self.nodes[0].tree

    class Resize(Message):
        def __init__(self, tree: Tree[LintTreeDataType]) -> None:
            self.tree: Tree[LintTreeDataType] = tree
            super().__init__()

        @property
        def control(self) -> Tree[LintTreeDataType]:
            return self.tree

    def __init__(
        self,
        config: ConfigRoot,
        cwd: Path,
        *,
        name: str | None = None,
        id: str | None = None,
        classes: str | None = None,
        disabled: bool = False,
    ):
        super().__init__("fnug", name=name, id=id, classes=classes, disabled=disabled)
        self.command_leafs = attach_command(self.root, config, cwd, root=True)
        self.cwd = cwd
        self.action_autoselect()

    def _get_label_region(self, line: int) -> Region | None:
        """Like parent, but offset by 2 to account for the icon."""
        region = super()._get_label_region(line)
        if region is None:
            return None
        region = region._replace(x=region.x - 2)
        return region

    def update_status(self, command_id: str, status: StatusType):
        node = self.command_leafs[command_id]
        if node.data is None:
            return

        node.data.status = status
        if status == "success":
            node.data.selected = False
        update_node(node)

    def action_run(self) -> None:
        if self.cursor_node is None:
            return
        self.post_message(self.RunCommand(self.cursor_node))

    def action_stop(self) -> None:
        if self.cursor_node is None:
            return
        self.post_message(self.StopCommand(self.cursor_node))

    def action_run_all(self) -> None:
        nodes = [
            node
            for node in all_commands(self.root)
            if node.data and node.data.selected and node.data.status not in ["running"]
        ]
        if len(nodes) > 0:
            self.post_message(self.RunAllCommand(nodes))

    def action_expand_node(self) -> None:
        if self.cursor_node is None:
            return
        if self.cursor_node.data and self.cursor_node.data.type == "command":
            self.cursor_node.data.selected = True
            update_node(self.cursor_node)
        elif self.cursor_node.children:
            self.cursor_node.expand()

    def action_collapse_node(self) -> None:
        if self.cursor_node is None:
            return
        if self.cursor_node.data and self.cursor_node.data.type == "command":
            self.cursor_node.data.selected = False
            update_node(self.cursor_node)
        elif self.cursor_node.children:
            self.cursor_node.collapse()

    def action_toggle_select(self) -> None:
        if self.cursor_node is None:
            return

        toggle_select_node(self.cursor_node)

    def action_autoselect(self):
        for command in all_commands(self.root):
            select_git_autorun(self.cwd, command)

    def action_toggle_select_click(self, command_id: str):
        node = self.command_leafs.get(command_id)
        if node and node.data:
            node.data.selected = not node.data.selected
            update_node(node)

    def render_label(self, node: TreeNode[LintTreeDataType], base_style: Style, style: Style) -> Text:
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
            if node.is_expanded:
                dropdown = ("▼ ", base_style + TOGGLE_STYLE)
            else:
                dropdown = ("▶ ", base_style + TOGGLE_STYLE)

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
                base_style + Style(meta={"@mouse.up": f"toggle_select_click('{node.data.id}')"} if node.data else {}),
            )
        elif is_command:
            selection = (
                "○ ",
                base_style + Style(meta={"@mouse.up": f"toggle_select_click('{node.data.id}')"} if node.data else {}),
            )
        else:
            selection = ("", base_style)

        text = Text.assemble(dropdown, selection, node_label, status, group_count)
        return text

    def on_mount(self):
        self.watch_task = asyncio.create_task(watch_autorun_task(all_commands(self.root), self.cwd))

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
            self.post_message(self.Resize(self))
        elif event.screen_x == self.size.width:  # Hover highlight
            self.styles.border_right = ("solid", "#a64c38")
        else:
            self.styles.border_right = ("solid", "#cf6a4c")
        event.stop()

    def _on_leave(self, event: events.Leave) -> None:
        """Clear any highlight when the mouse leaves the widget"""
        self.styles.border_right = ("solid", "#cf6a4c")
