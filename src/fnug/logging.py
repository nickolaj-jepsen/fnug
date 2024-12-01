import logging
import os
from collections.abc import Callable
from datetime import datetime
from enum import IntEnum
from typing import Any

import click
from textual.logging import TextualHandler

LEVEL_COLORS: dict[str, Callable[[str], str]] = {
    "DEBUG": lambda x: click.style(x, fg="blue"),
    "INFO": lambda x: click.style(x, fg="green"),
    "WARNING": lambda x: click.style(x, fg="yellow"),
    "ERROR": lambda x: click.style(x, fg="red"),
    "CRITICAL": lambda x: click.style(x, fg="bright_red", bold=True),
}


class _ColoredFormatter(logging.Formatter):
    def format(self, record: logging.LogRecord) -> str:
        # Save original values
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
            formatted_message = f"{formatted_message}\n{self.formatException(record.exc_info)}"

        return formatted_message


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
    handler = TextualHandler()
    handler.setFormatter(_ColoredFormatter())
    logging.basicConfig(
        format="%(asctime)s - %(name)s - %(levelname)s - %(message)s", level=LogLevel.from_env(), handlers=[handler]
    )


def get_logger(name: str = "fnug.cli") -> logging.Logger:
    """Get a logger instance with the given name."""
    return logging.getLogger(name)
