from unittest.mock import Mock

from rich.text import Text
from textual.widgets._tree import NodeID, Tree, TreeNode

from fnug.ui.components.lint_tree import LintTreeDataType, select_node, toggle_select_node, update_node


def _create_node(parent=None):
    node = TreeNode(Tree(""), parent, NodeID(1), Text(""), data=LintTreeDataType("1", "1", "command"))
    if parent:
        parent._children.append(node)
    return node


class TestUpdateNode:
    def test_refresh(self):
        node = _create_node()
        node.refresh = Mock()

        update_node(node)

        assert node.refresh.called is True

    def test_expand(self):
        node = _create_node()
        node.expand = Mock()

        update_node(node)

        assert node.expand.called is True

    def test_with_parent_and_grandparent(self):
        grandparent = _create_node()
        parent = _create_node(parent=grandparent)
        node = _create_node(parent=parent)
        node.refresh = Mock()
        parent.refresh = Mock()
        grandparent.refresh = Mock()

        update_node(node)

        assert node.refresh.called is True
        assert parent.refresh.called is True
        assert grandparent.refresh.called is True

    def test_dont_refresh_children(self):
        node = _create_node()
        child = _create_node(parent=node)
        node.refresh = Mock()
        child.refresh = Mock()

        update_node(node)

        assert node.refresh.called is True
        assert child.refresh.called is False


class TestSelectNode:
    def test_select_node(self):
        node = _create_node()

        select_node(node)
        assert node.data.selected is True

    def test_node_is_updated(self):
        node = _create_node()
        node.refresh = Mock()

        select_node(node)

        assert node.refresh.called is True


class TestToggleSelectNode:
    def test_simple(self):
        node = _create_node()

        toggle_select_node(node)

        assert node.data.selected is True

    def test_children(self):
        node = _create_node()
        child = _create_node(parent=node)

        toggle_select_node(node)

        assert node.data.selected is True
        assert child.data.selected is True
