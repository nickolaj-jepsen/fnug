"""Fnug - A TUI command runner based on git changes."""

from __future__ import annotations

import os
import shutil
import subprocess
import sys
import tempfile
from contextlib import contextmanager
from pathlib import Path
from typing import TYPE_CHECKING

from fnug.config import Auto, Command, CommandGroup, Config

if TYPE_CHECKING:
    from collections.abc import Iterator

__all__ = [
    "Auto",
    "Command",
    "CommandGroup",
    "Config",
    "check",
    "main",
    "run",
    "start",
]


def _find_binary() -> str:
    """Locate the fnug binary.

    Checks the Python scripts directory first (where maturin installs it),
    then falls back to PATH lookup.
    """
    # Check in the scripts directory next to the Python executable
    if sys.executable:
        bin_dir = Path(sys.executable).parent
        for name in ("fnug", "fnug.exe"):
            candidate = bin_dir / name
            if candidate.is_file():
                return str(candidate)

    # Check in the Scripts directory on Windows
    if sys.platform == "win32" and sys.prefix:
        candidate = Path(sys.prefix) / "Scripts" / "fnug.exe"
        if candidate.is_file():
            return str(candidate)

    # Fall back to PATH
    found = shutil.which("fnug")
    if found:
        return found

    msg = (
        "Could not find the fnug binary. "
        "Make sure fnug is installed (pip install fnug)."
    )
    raise FileNotFoundError(msg)


def run(*args: str) -> subprocess.CompletedProcess[bytes]:
    """Run the fnug binary with the given arguments.

    Args:
        *args: Command-line arguments to pass to fnug.

    Returns:
        The completed process result.
    """
    binary = _find_binary()
    return subprocess.run(  # noqa: S603
        [binary, *args],
        env={**os.environ, "FNUG_PYTHON_WRAPPER": "1"},
        check=False,
    )


@contextmanager
def _config_tempfile(config: Config) -> Iterator[str]:
    """Write a Config to a temp file, yield its path, clean up after."""
    tmp = tempfile.NamedTemporaryFile(  # noqa: SIM115
        mode="w",
        suffix=".fnug.yaml",
        delete=False,
    )
    try:
        tmp.write(config.to_yaml())
        tmp.close()
        yield tmp.name
    finally:
        Path(tmp.name).unlink(missing_ok=True)


def _resolve_config_args(
    config: Config | None,
    config_path: str | Path | None,
) -> list[str]:
    """Validate config arguments and return CLI args (without tempfile)."""
    if config is not None and config_path is not None:
        msg = "Cannot specify both 'config' and 'config_path'"
        raise ValueError(msg)
    if config_path is not None:
        return ["--config", str(config_path)]
    return []


def start(
    config: Config | None = None,
    *,
    config_path: str | Path | None = None,
    log_file: str | Path | None = None,
) -> subprocess.CompletedProcess[bytes]:
    """Launch the fnug TUI.

    Args:
        config: A Config dataclass to use (written to a temp file).
        config_path: Path to an existing .fnug.yaml file.
        log_file: Path for file logging.

    Returns:
        The completed process result.

    Raises:
        ValueError: If both config and config_path are provided.
    """
    args = _resolve_config_args(config, config_path)
    if log_file is not None:
        args.extend(["--log-file", str(log_file)])

    if config is not None:
        with _config_tempfile(config) as path:
            return run("--config", path, *args)
    return run(*args)


def check(  # noqa: PLR0913
    config: Config | None = None,
    *,
    config_path: str | Path | None = None,
    fail_fast: bool = False,
    no_tui: bool = False,
    mute_success: bool = False,
    log_file: str | Path | None = None,
) -> subprocess.CompletedProcess[bytes]:
    """Run fnug in headless check mode.

    Args:
        config: A Config dataclass to use (written to a temp file).
        config_path: Path to an existing .fnug.yaml file.
        fail_fast: Stop on first failure.
        no_tui: Never prompt to open the TUI on failure.
        mute_success: Suppress output for commands that pass.
        log_file: Path for file logging.

    Returns:
        The completed process result.

    Raises:
        ValueError: If both config and config_path are provided.
    """
    args = _resolve_config_args(config, config_path)
    if log_file is not None:
        args.extend(["--log-file", str(log_file)])

    check_args: list[str] = []
    if fail_fast:
        check_args.append("--fail-fast")
    if no_tui:
        check_args.append("--no-tui")
    if mute_success:
        check_args.append("--mute-success")

    if config is not None:
        with _config_tempfile(config) as path:
            return run("--config", path, *args, "check", *check_args)
    return run(*args, "check", *check_args)


def main() -> None:
    """Entry point that forwards sys.argv to the fnug binary."""
    result = run(*sys.argv[1:])
    sys.exit(result.returncode)
