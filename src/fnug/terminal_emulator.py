import asyncio
import fcntl
import os
import struct
import termios
from pathlib import Path
from typing import Literal

import pyte
from rich.console import Console
from rich.text import Text
from textual.geometry import Size


class TerminalEmulator:
    def __init__(self, dimensions: Size, event: asyncio.Event):
        self.pty, self.tty = os.openpty()
        self.out = os.fdopen(self.pty, "r+b", 0)
        self.screen = pyte.HistoryScreen(dimensions.width, dimensions.height, ratio=0.25)
        self.stream = pyte.Stream(self.screen)
        self.update_ready = event
        self.finished = asyncio.Event()
        self.dimensions = dimensions

    async def reader(self):
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

    def echo(self, text: Text):
        """Convert text formatted with rich markup to standard string."""
        tmp_console = Console(file=None, highlight=False, color_system="standard")
        with tmp_console.capture() as capture:
            tmp_console.print(text, soft_wrap=True, end="")
        self.stream.feed(capture.get())
        self.stream.feed("\n\r")
        self.screen.dirty.clear()
        self.update_ready.set()

    async def run_shell(self, command: str, cwd: Path) -> bool:
        # Echo command to tty
        self.echo(Text("Running the command: ", style="dim underscore") + Text(command) + Text("\n"))

        process = await asyncio.subprocess.create_subprocess_shell(
            command,
            cwd=cwd,
            stdin=self.tty,
            start_new_session=True,
            stdout=self.tty,
            stderr=self.tty,
        )
        try:
            code = await process.wait()
        except asyncio.CancelledError:
            process.terminate()
            await process.wait()
            raise

        if code == 0:
            self.echo(Text("\nSuccess!", style="dim"))
        else:
            self.echo(Text("\nFailure!", style="bold") + Text(f" (exit code {code})", style="dim"))

        self.finished.set()
        return code == 0

    def clear(self):
        self.screen.reset()
        self.update_ready.set()

    def write(self, data: bytes):
        os.write(self.pty, data)

    def scroll(self, direction: Literal["up", "down"]):
        if direction == "up":
            self.screen.prev_page()
        else:
            self.screen.next_page()
        self.update_ready.set()

    @property
    def dimensions(self):
        return self._dimensions

    @dimensions.setter
    def dimensions(self, dimensions: Size):
        self._dimensions = dimensions
        winsize = struct.pack("HH", dimensions.height, dimensions.width)
        fcntl.ioctl(self.pty, termios.TIOCSWINSZ, winsize)
        self.screen.resize(dimensions.height, dimensions.width)
