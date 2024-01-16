from dataclasses import dataclass
from pathlib import Path
from typing import ClassVar, Literal

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
    status: StatusType | None = None
    selected: bool = False


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
            data=LintTreeDataType(name=command_group.name, type="group", id=".".join(new_path)),
        )
    else:
        new_root = tree

    for command in command_group.commands:
        command_id = ".".join([*new_path, command.name])
        selected = False
        if command.autorun:
            if command.autorun is True:
                selected = True
            else:
                selected = detect_repo_changes(
                    cwd / command.autorun.git_root, command.autorun.sub_path, command.autorun.regex
                )
            new_root.expand()

        command_leafs[command_id] = new_root.add_leaf(
            command.name,
            data=LintTreeDataType(name=command.name, type="command", command=command, id=command_id, selected=selected),
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
        # self.root = build_command_tree(config)
        self.command_leafs = attach_command(self.root, config, cwd, root=True)
        self.root.expand()

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

    def update_selected(self, command_id: str, selected: bool):
        command = self.command_leafs[command_id]
        if command.data is None:
            return
        command.data.selected = selected
        command.refresh()

    def action_run(self) -> None:
        if self.cursor_node is None:
            return
        self.post_message(self.RunCommand(self.cursor_node))

    def action_stop(self) -> None:
        if self.cursor_node is None:
            return
        self.post_message(self.StopCommand(self.cursor_node))

    def all_nodes(self, source_node: TreeNode[LintTreeDataType] | None = None) -> list[TreeNode[LintTreeDataType]]:
        if source_node is None:
            source_node = self.root

        nodes: list[TreeNode[LintTreeDataType]] = []
        if source_node.data and source_node.data.type == "command":
            nodes.append(source_node)
        for child in source_node.children:
            nodes.extend(self.all_nodes(child))
        return nodes

    def action_run_all(self) -> None:
        nodes = [
            node
            for node in self.all_nodes()
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
        if self.cursor_node is None or self.cursor_node.data is None:
            return

        def set_children(node: TreeNode[LintTreeDataType], selected: bool):
            if node.data and node.data.type == "command":
                node.data.selected = selected
            for child in node.children:
                set_children(child, selected)

        self.cursor_node.data.selected = not self.cursor_node.data.selected
        if self.cursor_node.children:
            self.cursor_node.expand_all()
            set_children(self.cursor_node, self.cursor_node.data.selected)
        self.cursor_node.refresh()

    def action_autoselect(self, root: TreeNode[LintTreeDataType] | None = None) -> bool:
        if root is None:
            root = self.root

        has_selected = False
        if root.data and root.data.type == "command" and root.data.command and root.data.command.autorun:
            if root.data.command.autorun is True:
                selected = True
            else:
                selected = detect_repo_changes(
                    root.data.command.autorun.git_root,
                    root.data.command.autorun.sub_path,
                    root.data.command.autorun.regex,
                )
            root.data.selected = selected
            has_selected = selected or has_selected
            root.refresh()

        if root.children:
            children_selected = [self.action_autoselect(child) for child in root.children]
            has_selected = any(children_selected) or has_selected
            if any(children_selected):
                root.expand()

        return has_selected

    def action_deselect(self, command_id: str):
        cmd = self.command_leafs.get(command_id)
        if cmd and cmd.data:
            cmd.data.selected = False

    def action_select(self, command_id: str):
        cmd = self.command_leafs.get(command_id)
        if cmd and cmd.data:
            cmd.data.selected = True

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
            selection = ("● ", base_style + Style(meta={"@click": f"deselect('{node.data.id}')"} if node.data else {}))
        elif is_command:
            selection = ("○ ", base_style + Style(meta={"@click": f"select('{node.data.id}')"} if node.data else {}))
        else:
            selection = ("", base_style)

        text = Text.assemble(dropdown, selection, node_label, status)
        return text
