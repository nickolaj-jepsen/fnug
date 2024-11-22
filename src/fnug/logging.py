import logging
import os
from collections.abc import Callable
from datetime import datetime
from enum import IntEnum
from inspect import Traceback
from typing import TYPE_CHECKING, Any, Optional

import click
from textual._context import active_app
from textual._log import LogGroup, LogVerbosity

if TYPE_CHECKING:
    from textual_dev.client import DevtoolsClient

LEVEL_COLORS: dict[str, Callable[[str], str]] = {
    "DEBUG": lambda x: click.style(x, fg="blue"),
    "INFO": lambda x: click.style(x, fg="green"),
    "WARNING": lambda x: click.style(x, fg="yellow"),
    "ERROR": lambda x: click.style(x, fg="red"),
    "CRITICAL": lambda x: click.style(x, fg="bright_red", bold=True),
}


class LogHandler(logging.Handler):
    """A Logging handler usable in both Textual and non-Textual environments."""

    def __init__(self) -> None:
        super().__init__()

    _devtools: Optional["DevtoolsClient"] = None

    def _textual_devtools(self) -> Optional["DevtoolsClient"]:
        if not self._devtools:
            try:
                app = active_app.get()
            except LookupError:
                return None
            self._devtools = app.devtools

        if self._devtools is None or not self._devtools.is_connected:
            return None

        return self._devtools

    def emit(self, record: logging.LogRecord) -> None:
        """Emit a log record."""
        if devtools := self._textual_devtools():
            self._textual_handler(record, devtools)
        else:
            self._stderr_handler(record)

    def _textual_handler(self, record: logging.LogRecord, devtools: "DevtoolsClient") -> None:
        from textual_dev.client import DevtoolsLog

        terminal_width = devtools.console.width

        left_header = f"[{record.levelname}] {record.name}"
        right_header = (
            f"{record.module}.{record.funcName}" + "[PYTHON]" if record.filename.endswith(".py") else "[RUST]"
        )
        spacer = " " * (terminal_width - len(left_header) - len(right_header))
        header = f"{left_header}{spacer}{right_header}"

        log = DevtoolsLog(
            objects_or_string=(header, " " * terminal_width, record.getMessage(), "\n"),
            caller=Traceback(record.filename, record.lineno, record.funcName, None, None),
        )

        devtools.log(
            log, LogGroup.LOGGING, LogVerbosity.HIGH if record.levelno < logging.WARNING else LogVerbosity.HIGH
        )

    def _stderr_handler(self, record: logging.LogRecord) -> None:
        level_name = record.levelname
        message = record.msg

        # Format timestamp
        timestamp = datetime.fromtimestamp(record.created).strftime("%H:%M:%S.%f")[:-3]

        # Color the level name
        colored_level = LEVEL_COLORS.get(level_name, lambda x: x)(f"{level_name:<8}")

        # Add the name of the logger
        logger_name = click.style(record.name or "", dim=True)

        # Format the complete message
        formatted_message = f"{timestamp} {colored_level} {logger_name} {message}"

        if record.exc_info:
            # If there's an exception, add it with proper indentation
            formatted_message = f"{formatted_message}\n{record.exc_info}"

        click.echo(formatted_message, err=True)


class LogLevel(IntEnum):
    """Enum of log levels."""

    CRITICAL = logging.CRITICAL
    ERROR = logging.ERROR
    WARNING = logging.WARNING
    INFO = logging.INFO
    DEBUG = logging.DEBUG
    NOTSET = logging.NOTSET

    @classmethod
    def from_env(cls) -> int:
        """
        Get the log level from the environment variable FNUG_LOG_LEVEL.

        Otherwise, return the default log level (INFO).
        """
        env_level = os.environ.get("FNUG_LOG_LEVEL", "").strip().upper()
        if env_level in cls.__members__:
            return cls[env_level].value

        return cls.WARNING.value


def log_level_callback(_: Any, __: Any, value: str | None):
    """Set the log level based on click options callback."""
    if value is not None:
        logging.getLogger().setLevel(int(value))


def setup_logging() -> None:
    """Initialize logging."""
    logging.basicConfig(level=LogLevel.from_env(), handlers=[LogHandler()])


def get_logger(name: str = "fnug.cli") -> logging.Logger:
    """Get a logger instance with the given name."""
    return logging.getLogger(name)
