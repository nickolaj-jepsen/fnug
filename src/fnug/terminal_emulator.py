import asyncio
import fcntl
import math
import os
import struct
import termios
from pathlib import Path
from typing import Literal

import pyte
from rich.console import Console
from rich.text import Text
from textual.geometry import Size


def start_message(commands: str) -> Text:
    """Create a start message for a command."""
    return Text.assemble(
        *[Text.assemble(Text("❱ ", style="#cf6a4c"), Text(command)) for command in commands.strip().split("\n")]
    )


def success_message() -> Text:
    """Create a success message."""
    return Text.assemble(Text("\n"), Text("❱ ", style="#cf6a4c"), Text("Success"), Text(" ✔", style="green"))


def failure_message(exit_code: int) -> Text:
    """Create a failure message."""
    return Text.assemble(
        Text("\n"),
        Text("❱ ", style="#cf6a4c"),
        Text("Command failed"),
        Text(" ✘", style="red"),
        Text(f" (exit code {exit_code})", style="#808080"),
    )


def stopped_message() -> Text:
    """Create a failure message."""
    return Text.assemble(Text("\n"), Text("❱ ", style="#cf6a4c"), Text("Stopped"), Text(" ✘", style="red"))


def any_key_message() -> Text:
    """Create a message to press any key."""
    return Text.assemble(Text("❱ ", style="#cf6a4c"), Text("Press any key to continue"))


class FixedHistoryScreen(pyte.HistoryScreen):
    """
    Exactly like pyte.HistoryScreen but allows scrolling to the top of the buffer.

    This is done by loosening the condition for when to allow scrolling up.
    """

    def prev_page(self) -> None:
        """Scroll the screen up by one page."""
        if self.history.top:
            mid = min(len(self.history.top), int(math.ceil(self.lines * self.history.ratio)))

            self.history.bottom.extendleft(self.buffer[y] for y in range(self.lines - 1, self.lines - mid - 1, -1))
            self.history = self.history._replace(position=self.history.position - mid)

            for y in range(self.lines - 1, mid - 1, -1):
                self.buffer[y] = self.buffer[y - mid]
            for y in range(mid - 1, -1, -1):
                self.buffer[y] = self.history.top.pop()

            self.dirty = set(range(self.lines))


class TerminalEmulator:
    """A terminal emulator."""

    def __init__(self, dimensions: Size, event: asyncio.Event, can_focus: bool = False):
        self.pty, self.tty = os.openpty()
        self.out = os.fdopen(self.pty, "r+b", 0)
        self.screen = FixedHistoryScreen(dimensions.width, dimensions.height, history=5000, ratio=0.25)
        self.stream = pyte.Stream(self.screen)
        self.update_ready = event
        self.finished = asyncio.Event()
        self.dimensions = dimensions
        self.can_focus = can_focus

    async def reader(self):
        """Read data from the pty and feed it to the terminal."""
        loop = asyncio.get_running_loop()

        def on_output():
            self.stream.feed(self.out.read(65536).decode())
            self.screen.dirty.clear()
            self.update_ready.set()

        loop.add_reader(self.out, on_output)

        try:
            await self.finished.wait()
        finally:
            loop.remove_reader(self.out)

    def echo(self, text: str | Text | list[Text]):
        """Echo text to the terminal."""
        if isinstance(text, str):
            text = Text(text)

        if isinstance(text, list):
            for line in text:
                self.echo(line)
        else:
            tmp_console = Console(color_system="truecolor", file=None, highlight=False)
            with tmp_console.capture() as capture:
                tmp_console.print(text, soft_wrap=True, end="")
            self.stream.feed(capture.get())

        self.stream.feed("\n\r")
        self.screen.dirty.clear()
        self.update_ready.set()

    async def run_shell(self, command: str, cwd: Path) -> bool:
        """Run a shell command in a subprocess, and send the output to the tty."""
        # Echo command to tty
        self.echo(start_message(command))

        process = await asyncio.subprocess.create_subprocess_shell(
            command,
            cwd=cwd,
            stdin=self.tty,
            start_new_session=True,
            stdout=self.tty,
            stderr=self.tty,
            env={**os.environ, "TERM": "xterm-256color"},
        )
        try:
            code = await process.wait()
        except asyncio.CancelledError:
            process.terminate()
            await process.wait()
            raise

        if code == 0:
            self.echo(success_message())
        else:
            self.echo(failure_message(code))

        self.finished.set()
        return code == 0

    def clear(self):
        """Clear the terminal."""
        self.screen.reset()
        self.update_ready.set()

    def write(self, data: bytes):
        """Write data to the terminal."""
        os.write(self.pty, data)

    def scroll(self, direction: Literal["up", "down"]):
        """Move the scroll position up or down."""
        if direction == "up":
            self.screen.prev_page()
        else:
            self.screen.next_page()
        self.update_ready.set()

    def click(self, x: int, y: int):
        """Emulate a mouse click at the given position."""
        self.out.write(f"\x1b[<0;{x};{y}M".encode())
        self.out.write(f"\x1b[<0;{x};{y}m".encode())
        self.screen.dirty.clear()
        self.update_ready.set()

    @property
    def dimensions(self):
        """The dimensions of the terminal."""
        return self._dimensions

    @dimensions.setter
    def dimensions(self, dimensions: Size):
        self._dimensions = dimensions
        winsize = struct.pack("HH", dimensions.height, dimensions.width)
        fcntl.ioctl(self.pty, termios.TIOCSWINSZ, winsize)
        self.screen.resize(dimensions.height, dimensions.width)
