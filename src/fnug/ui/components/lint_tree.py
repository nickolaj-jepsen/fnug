from dataclasses import dataclass
from typing import ClassVar, Literal

from rich.style import Style
from rich.text import Text
from textual.binding import Binding, BindingType
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
            selected = detect_repo_changes(command.autorun.git_root, command.autorun.sub_path, command.autorun.regex)

        command_leafs[command_id] = new_root.add_leaf(
            command.name,
            data=LintTreeDataType(name=command.name, type="command", command=command, id=command_id, selected=selected),
        )
    for child in command_group.children:
        command_leafs.update(attach_command(new_root, child, path=new_path))
    new_root.expand()
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
        *,
        name: str | None = None,
        id: str | None = None,
        classes: str | None = None,
        disabled: bool = False,
    ):
        super().__init__("fnug", name=name, id=id, classes=classes, disabled=disabled)
        # self.root = build_command_tree(config)
        self.command_leafs = attach_command(self.root, config, root=True)
        self.root.expand()

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

        self.cursor_node.data.selected = not self.cursor_node.data.selected
        if self.cursor_node.children:
            if self.cursor_node.data.selected:
                self.cursor_node.expand_all()
            else:
                self.cursor_node.collapse_all()
        self.cursor_node.refresh()

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
            selection = ("● ", base_style)
        elif is_command:
            selection = ("○ ", base_style)
        else:
            selection = ("", base_style)

        text = Text.assemble(dropdown, selection, node_label, status)
        return text
