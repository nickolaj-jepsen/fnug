import asyncio
from typing import ClassVar

from rich.console import Console, ConsoleOptions, ConsoleRenderable, RenderResult
from rich.text import Text
from textual import events
from textual.binding import Binding, BindingType
from textual.events import Key
from textual.keys import Keys
from textual.message import Message
from textual.reactive import reactive
from textual.scrollbar import ScrollBar
from textual.widget import Widget

from fnug.terminal_emulator import TerminalEmulator

CTRL_KEYS: dict[str, str] = {
    Keys.Up: "\x1bOA",
    Keys.Down: "\x1bOB",
    Keys.Right: "\x1bOC",
    Keys.Left: "\x1bOD",
    Keys.Home: "\x1bOH",
    Keys.End: "\x1b[F",
    Keys.Insert: "\x1b[2~",
    Keys.Delete: "\x1b[3~",
    Keys.PageUp: "\x1b[5~",
    Keys.PageDown: "\x1b[6~",
    Keys.BackTab: "\x1b[Z",
    Keys.ControlF1: "\x1bOP",
    Keys.ControlF2: "\x1bOQ",
    Keys.ControlF3: "\x1bOR",
    Keys.ControlF4: "\x1bOS",
    Keys.ControlF5: "\x1b[15~",
    Keys.ControlF6: "\x1b[17~",
    Keys.ControlF7: "\x1b[18~",
    Keys.ControlF8: "\x1b[19~",
    Keys.ControlF9: "\x1b[20~",
    Keys.ControlF10: "\x1b[21~",
    Keys.ControlF11: "\x1b[23~",
    Keys.ControlF12: "\x1b[24~",
    Keys.ControlF13: "\x1b[25~",
    Keys.ControlF14: "\x1b[26~",
    Keys.ControlF15: "\x1b[28~",
    Keys.ControlF16: "\x1b[29~",
    Keys.ControlF17: "\x1b[31~",
    Keys.ControlF18: "\x1b[32~",
    Keys.ControlF19: "\x1b[33~",
    Keys.ControlF20: "\x1b[34~",
}


class TerminalDisplay(ConsoleRenderable):
    """Rich display for the terminal."""

    def __init__(self, lines: list[Text]):
        self.lines: list[Text] = lines

    def __rich_console__(self, console: Console, options: ConsoleOptions) -> RenderResult:
        """Render the terminal display."""
        yield from self.lines


class Terminal(Widget, can_focus=False):
    """Terminal textual widget."""

    emulator: TerminalEmulator | None = None
    show_vertical_scrollbar = reactive(True)

    BINDINGS: ClassVar[list[BindingType]] = [
        Binding("shift+tab", "unfocus", "Switch focus"),
    ]

    class OpenContextMenu(Message):
        def __init__(self, this: "Terminal", event: events.Click) -> None:
            self.this: "Terminal" = this
            self.click_event: events.Click = event
            super().__init__()

        @property
        def control(self) -> "Terminal":
            """The tree that sent the message."""
            return self.this

    def __init__(
        self,
        name: str | None = None,
        id: str | None = None,
        classes: str | None = None,
    ) -> None:
        self.terminal_display = TerminalDisplay([Text()])

        super().__init__(name=name, id=id, classes=classes)

    def render(self):
        """Render the terminal display."""
        return self.terminal_display

    def clear(self):
        """Clear the terminal display."""
        self.terminal_display = TerminalDisplay([Text()])
        self.refresh()

    def update_scrollbar(self, scrollbar: ScrollBar):
        """Update the position of the scrollbar."""
        if self.emulator is None:
            return

        scrollbar.position = len(self.emulator.screen.history.top)
        scrollbar.window_virtual_size = (
            len(self.emulator.screen.history.top)
            + self.emulator.screen.lines
            + len(self.emulator.screen.history.bottom)
        )
        scrollbar.refresh()

    async def attach_emulator(self, emulator: TerminalEmulator, scrollbar: ScrollBar):
        """Attach a terminal emulator to this widget."""
        self.emulator = emulator
        self.can_focus = emulator.can_focus
        try:
            async for screen in emulator.render():
                self.terminal_display = TerminalDisplay(screen)
                self.update_scrollbar(scrollbar)
                self.refresh()
        except asyncio.CancelledError:
            pass

    async def _on_key(self, event: Key) -> None:
        if self.emulator is None:
            return

        if event.key == "shift+tab":
            self.app.set_focus(None)
            return

        event.stop()
        char = CTRL_KEYS.get(event.key) or event.character
        if char:
            self.emulator.write(char.encode())

    def _on_mouse_scroll_down(self, event: events.MouseScrollDown) -> None:
        if self.emulator:
            self.emulator.scroll("down")

    def _on_mouse_scroll_up(self, event: events.MouseScrollUp) -> None:
        if self.emulator:
            self.emulator.scroll("up")

    async def _on_click(self, event: events.Click):
        if self.emulator is None:
            return

        if event.button in [2, 3]:
            self.post_message(self.OpenContextMenu(self, event))
        else:
            self.emulator.click(event.x + 1, event.y + 1)
