from textual import events, on
from textual.app import ComposeResult
from textual.containers import Vertical
from textual.message import Message
from textual.screen import ModalScreen
from textual.widgets import Label


class ContextMenuItem(Label):
    """A context menu item."""

    class Clicked(Message):
        def __init__(self, element: Label) -> None:
            self.element: Label = element
            super().__init__()

        @property
        def control(self) -> Label:
            """The tree that sent the message."""
            return self.element

    async def _on_click(self, event: events.Click) -> None:
        event.stop()
        self.post_message(self.Clicked(self))


class ContextMenu(ModalScreen[str | None]):
    """A context menu."""

    DEFAULT_CSS = """
    ContextMenu {
      background: rgba(0,0,0,0.35);
    }

    #container {
        background: $background;
    }

    .options {
      width: 100%;
      padding: 0 1;
    }

    .options:hover {
      background: $boost;
      text-style: underline;
    }
    """

    def __init__(self, options: dict[str, str], click_event: events.Click) -> None:
        self.options = options
        self.width = max(len(label) for label in options.values()) + 2
        self.height = len(options)
        self.offset_x = click_event.screen_x
        self.offset_y = click_event.screen_y
        super().__init__()

    def _on_mount(self, event: events.Mount) -> None:
        vertical = self.query_one("#container", Vertical)
        vertical.styles.height = self.height
        vertical.styles.width = self.width
        vertical.styles.offset = (self.offset_x + 1, self.offset_y + 1)

    def compose(self) -> ComposeResult:  # noqa: D102
        with Vertical(id="container"):
            yield from [ContextMenuItem(label, id=option, classes="options") for option, label in self.options.items()]

    @on(ContextMenuItem.Clicked)
    def _label_clicked(self, event: ContextMenuItem.Clicked) -> None:
        self.dismiss(event.element.id)

    async def _on_click(self, event: events.Click) -> None:
        self.dismiss(None)
