from dataclasses import dataclass
from pathlib import Path
from typing import ClassVar, Literal, Iterator

from rich.style import Style
from rich.text import Text
from textual.binding import Binding, BindingType
from textual.geometry import Region
from textual.message import Message
from textual.widgets import Tree
from textual.widgets._tree import TreeNode, TOGGLE_STYLE

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


def expand_node(command: TreeNode[LintTreeDataType]):
    """Expands a node (recursively)"""

    command.expand()
    if command.parent:
        expand_node(command.parent)


def select_node(node: TreeNode[LintTreeDataType]):
    """
    Selects a node

    Also expands all parents
    """
    if node.data is None:
        return
    node.data.selected = True
    node.refresh()
    if node.parent:
        expand_node(node.parent)


def toggle_select_node(node: TreeNode[LintTreeDataType], override_value: bool | None = None):
    """
    Toggle a node (recursively if with children)
    """
    if not node.data:
        return

    if override_value is None:
        override_value = not node.data.selected
    node.data.selected = override_value
    if override_value:
        expand_node(node)
    node.refresh()

    for child in node.children:
        toggle_select_node(child, override_value=override_value)


def all_commands(source_node: TreeNode[LintTreeDataType]) -> Iterator[TreeNode[LintTreeDataType]]:
    """
    Get all command children of a node (recursively)
    """
    for child in source_node.children:
        if child.data and child.data.type == "command":
            yield child
        yield from all_commands(child)


def select_autorun_commands(source_node: TreeNode[LintTreeDataType]) -> None:
    """
    Select all autorun commands
    """
    for children in source_node.children:
        if not children.data:
            continue

        if children.data.type == "command" and children.data.command:
            if children.data.command.autorun is True:
                select_node(children)
            elif children.data.command.autorun:
                selected = detect_repo_changes(
                    children.data.command.autorun.git_root,
                    children.data.command.autorun.sub_path,
                    children.data.command.autorun.regex,
                )
                if selected:
                    select_node(children)
        elif children.data.type == "group" and children.data.group:
            autorun = False
            if children.data.group.autorun is True:
                autorun = True
            elif children.data.group.autorun:
                autorun = detect_repo_changes(
                    children.data.group.autorun.git_root,
                    children.data.group.autorun.sub_path,
                    children.data.group.autorun.regex,
                )
            if autorun:
                toggle_select_node(children, True)
            else:
                select_autorun_commands(children)

    # for command in all_commands(source_node):
    #     if not command.data or not command.data.command or not command.data.command.autorun:
    #         continue
    #
    #     if command.data.command.autorun is True:
    #         select_node(command)
    #     else:
    #         selected = detect_repo_changes(
    #             command.data.command.autorun.git_root,
    #             command.data.command.autorun.sub_path,
    #             command.data.command.autorun.regex,
    #         )
    #         if selected:
    #             select_node(command)
    #
    #     command.refresh()


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
        if new_root.data and any(leaf.data and leaf.data.selected for leaf in child_commands.values()):
            new_root.data.selected = True
            new_root.expand()
        command_leafs.update(child_commands)
    return command_leafs


class LintTree(Tree[LintTreeDataType]):
    guide_depth = 3
    show_root = False

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
        select_autorun_commands(self.root)

    def _get_label_region(self, line: int) -> Region | None:
        """Like parent, but offset by 2 to account for the icon."""
        region = super()._get_label_region(line)
        if region is None:
            return None
        region = region._replace(x=region.x - 2)
        return region

    def update_status(self, command_id: str, status: StatusType):
        command = self.command_leafs[command_id]
        if command.data is None:
            return

        command.data.status = status
        if status == "success":
            command.data.selected = False
        command.refresh()

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
        elif self.cursor_node.children:
            self.cursor_node.expand()
        self.cursor_node.refresh()

    def action_collapse_node(self) -> None:
        if self.cursor_node is None:
            return
        if self.cursor_node.data and self.cursor_node.data.type == "command":
            self.cursor_node.data.selected = False
        elif self.cursor_node.children:
            self.cursor_node.collapse()
        self.cursor_node.refresh()

    def action_toggle_select(self) -> None:
        if self.cursor_node is None:
            return

        toggle_select_node(self.cursor_node)

    def action_autoselect(self):
        select_autorun_commands(self.root)

    def action_toggle_select_click(self, command_id: str):
        cmd = self.command_leafs.get(command_id)
        if cmd and cmd.data:
            cmd.data.selected = not cmd.data.selected

    def render_label(self, node: TreeNode[LintTreeDataType], base_style: Style, style: Style) -> Text:
        node_label = node._label.copy()  # pyright: ignore reportPrivateUsage=false
        node_label.stylize(style)

        if node._allow_expand:  # pyright: ignore reportPrivateUsage=false
            dropdown = (
                "▾ " if node.is_expanded else "▸ ",
                base_style + TOGGLE_STYLE,
            )
        else:
            dropdown = ("", base_style)

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

        text = Text.assemble(dropdown, selection, node_label, status)
        return text
