import re

from pyte import Screen
from pyte.screens import Char
from rich.style import Style
from rich.text import Text


def color_translator(color: str) -> str:
    """Translate a pyte color to a rich color."""
    if re.match("[0-9a-f]{6}", color, re.IGNORECASE):
        return f"#{color}"

    return color.replace("bright", "bright_").replace("brown", "yellow")


def style_from_pyte(char: Char) -> Style:
    """Create a rich style from a pyte character."""
    foreground = color_translator(char.fg)
    background = "#1e1e1e" if char.bg == "default" else color_translator(char.bg)

    return Style(
        color=foreground,
        bgcolor=background,
        bold=char.bold,
        italic=char.italics,
        underline=char.underscore,
        blink=char.blink,
        strike=char.strikethrough,
        reverse=char.reverse,
    )


def pyte2rich(screen: Screen) -> list[Text]:
    """Convert a pyte screen to a list of rich text ready to be rendered."""
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
