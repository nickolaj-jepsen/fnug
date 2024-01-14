import asyncio
import re
from typing import ClassVar

from pyte.screens import Char
from rich.console import Console, ConsoleOptions, ConsoleRenderable
from rich.text import Text
from rich.style import Style
from textual.binding import Binding, BindingType
from textual.events import Key, MouseMove

from textual.widget import Widget


from pyte import Screen

from fnug.terminal_emulator import TerminalEmulator


CTRL_KEYS = {
    "up": "\x1bOA",
    "down": "\x1bOB",
    "right": "\x1bOC",
    "left": "\x1bOD",
    "home": "\x1bOH",
    "end": "\x1b[F",
    "delete": "\x1b[3~",
    "pageup": "\x1b[5~",
    "pagedown": "\x1b[6~",
    "shift+tab": "\x1b[Z",
    "f1": "\x1bOP",
    "f2": "\x1bOQ",
    "f3": "\x1bOR",
    "f4": "\x1bOS",
    "f5": "\x1b[15~",
    "f6": "\x1b[17~",
    "f7": "\x1b[18~",
    "f8": "\x1b[19~",
    "f9": "\x1b[20~",
    "f10": "\x1b[21~",
    "f11": "\x1b[23~",
    "f12": "\x1b[24~",
    "f13": "\x1b[25~",
    "f14": "\x1b[26~",
    "f15": "\x1b[28~",
    "f16": "\x1b[29~",
    "f17": "\x1b[31~",
    "f18": "\x1b[32~",
    "f19": "\x1b[33~",
    "f20": "\x1b[34~",
}


def color_translator(color: str) -> str:
    if color == "brown":
        return "yellow"

    if re.match("[0-9a-f]{6}", color, re.IGNORECASE):
        return f"#{color}"

    return color.replace("bright", "bright_")


def style_from_pyte(char: Char) -> Style:
    foreground = color_translator(char.fg)
    background = color_translator(char.bg)

    style = Style(
        color=foreground,
        bgcolor=background,
        bold=char.bold,
        italic=char.italics,
        underline=char.underscore,
        blink=char.blink,
        strike=char.strikethrough,
        reverse=char.reverse,
    )

    return style


def pyte2rich(screen: Screen) -> list[Text]:
    lines: list[Text] = []
    last_char: Char
    last_style: Style
    for y in range(screen.lines):
        line_text = Text()
        line = screen.buffer[y]
        style_change_pos: int = 0
        for x in range(screen.columns):
            char: Char = line[x]

            line_text.append(char.data)

            if x > 0:
                last_char = line[x - 1]
                style_is_equal = char[1:] == last_char[1:]  # compare everything except the data

                # if style changed, stylize it with rich
                if not style_is_equal or x == screen.columns - 1:
                    last_style = style_from_pyte(last_char)
                    line_text.stylize(last_style, style_change_pos, x + 1)
                    style_change_pos = x

            if screen.cursor.x == x and screen.cursor.y == y:
                line_text.stylize("reverse", x, x + 1)

        lines.append(line_text)
    return lines


class TerminalDisplay(ConsoleRenderable):
    """Rich display for the terminal."""

    def __init__(self, lines: list[Text]):
        self.lines: list[Text] = lines

    def __rich_console__(self, console: Console, options: ConsoleOptions):
        for line in self.lines:
            yield line


class Terminal(Widget, can_focus=True):
    """Terminal textual widget."""

    emulator: TerminalEmulator | None = None

    BINDINGS: ClassVar[list[BindingType]] = [
        Binding("tab", "unfocus", "Switch focus"),
    ]

    def __init__(
        self,
        name: str | None = None,
        id: str | None = None,
        classes: str | None = None,
    ) -> None:
        self._display = TerminalDisplay([Text()])

        super().__init__(name=name, id=id, classes=classes)

    def render(self):
        return self._display

    def clear(self):
        self._display = TerminalDisplay([Text()])
        self.refresh()

    async def attach_emulator(self, emulator: TerminalEmulator, event: asyncio.Event):
        self.emulator = emulator
        while True:
            try:
                event.clear()
                self._display = TerminalDisplay(pyte2rich(emulator.screen))
                self.refresh()
                await event.wait()
            except asyncio.CancelledError:
                break

    async def on_key(self, event: Key) -> None:
        if self.emulator is None:
            return

        if event.key == "tab":
            self.app.set_focus(None)
            return

        event.stop()
        char = CTRL_KEYS.get(event.key) or event.character
        if char:
            self.emulator.write(char.encode())

    def on_mouse_scroll_down(self, event: MouseMove) -> None:
        if self.emulator:
            self.emulator.scroll("down")

    def on_mouse_scroll_up(self, event: MouseMove) -> None:
        if self.emulator:
            self.emulator.scroll("up")
