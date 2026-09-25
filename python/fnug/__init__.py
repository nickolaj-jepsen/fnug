"""Fnug - A TUI command runner based on git changes."""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
import tempfile
from contextlib import contextmanager
from pathlib import Path
from typing import TYPE_CHECKING

from fnug.config import Auto, Command, CommandGroup, Config, WorkspaceOptions

if TYPE_CHECKING:
    from collections.abc import Iterator

__all__ = [
    "Auto",
    "Command",
    "CommandGroup",
    "Config",
    "WorkspaceOptions",
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
    if sys.executable:
        candidate = Path(sys.executable).parent / "fnug"
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
    return subprocess.run([binary, *args], check=False)  # noqa: S603


@contextmanager
def _config_tempfile(config: Config) -> Iterator[str]:
    """Write a Config to a temporary JSON file, yield its path, and delete it after.

    fnug resolves the root ``cwd`` against the config file's directory, so the
    written copy pins it to the caller's working directory: an unset ``cwd`` becomes
    that directory and a relative one is resolved against it. ``config`` itself is
    not modified.

    Raises:
        ValueError: If ``config.workspace`` is enabled. Workspace discovery would
            search the temporary directory.
    """
    if config.workspace:
        msg = (
            "A Config with 'workspace' set can't be passed directly: fnug would "
            "look for workspace packages next to its temporary file. Write it with "
            "Config.write() and pass config_path instead."
        )
        raise ValueError(msg)
    data = config.to_dict()
    data["cwd"] = str((Path.cwd() / data.get("cwd", ".")).resolve())
    # Outside the project, so the file itself never counts as a changed file.
    fd, path = tempfile.mkstemp(suffix=".fnug.json")
    try:
        with os.fdopen(fd, "w") as file:
            json.dump(data, file)
        yield path
    finally:
        Path(path).unlink(missing_ok=True)


def _global_args(
    config: Config | None,
    config_path: str | Path | None,
    *,
    log_file: str | Path | None,
    no_workspace: bool,
) -> list[str]:
    """Build the flags that go before any subcommand.

    The ``--config`` flag for an in-memory ``config`` is not included; it is added
    once the temporary file exists.

    Raises:
        ValueError: If both config and config_path are provided.
    """
    if config is not None and config_path is not None:
        msg = "Cannot specify both 'config' and 'config_path'"
        raise ValueError(msg)
    args: list[str] = []
    if config_path is not None:
        args.extend(["--config", str(config_path)])
    if log_file is not None:
        args.extend(["--log-file", str(log_file)])
    if no_workspace:
        args.append("--no-workspace")
    return args


def _run_with_config(
    config: Config | None,
    *args: str,
) -> subprocess.CompletedProcess[bytes]:
    """Run fnug with ``args``, passing ``config`` through a temporary file if given."""
    if config is None:
        return run(*args)
    with _config_tempfile(config) as path:
        return run("--config", path, *args)


def start(
    config: Config | None = None,
    *,
    config_path: str | Path | None = None,
    log_file: str | Path | None = None,
    no_workspace: bool = False,
) -> subprocess.CompletedProcess[bytes]:
    """Launch the fnug TUI.

    Args:
        config: A Config to use. It is written to a temporary file, and its root
            ``cwd`` is resolved against the caller's working directory (the default).
        config_path: Path to an existing .fnug.yaml file.
        log_file: Path for file logging.
        no_workspace: Don't resolve upward to a parent workspace root.

    Returns:
        The completed process result.

    Raises:
        ValueError: If both config and config_path are provided, or config has
            ``workspace`` enabled.
    """
    args = _global_args(
        config,
        config_path,
        log_file=log_file,
        no_workspace=no_workspace,
    )
    return _run_with_config(config, *args)


def check(  # noqa: PLR0913
    config: Config | None = None,
    *,
    config_path: str | Path | None = None,
    fail_fast: bool = False,
    no_tui: bool = False,
    mute_success: bool = False,
    all_: bool = False,
    no_workspace: bool = False,
    log_file: str | Path | None = None,
) -> subprocess.CompletedProcess[bytes]:
    """Run fnug in headless check mode.

    Args:
        config: A Config to use. It is written to a temporary file, and its root
            ``cwd`` is resolved against the caller's working directory (the default).
        config_path: Path to an existing .fnug.yaml file.
        fail_fast: Stop on first failure.
        no_tui: Never prompt to open the TUI on failure.
        mute_success: Suppress output for commands that pass.
        all_: Pass ``--all`` to ``fnug check``.
        no_workspace: Don't resolve upward to a parent workspace root.
        log_file: Path for file logging.

    Returns:
        The completed process result.

    Raises:
        ValueError: If both config and config_path are provided, or config has
            ``workspace`` enabled.
    """
    args = _global_args(
        config,
        config_path,
        log_file=log_file,
        no_workspace=no_workspace,
    )
    args.append("check")
    if fail_fast:
        args.append("--fail-fast")
    if no_tui:
        args.append("--no-tui")
    if mute_success:
        args.append("--mute-success")
    if all_:
        args.append("--all")
    return _run_with_config(config, *args)


def main() -> None:
    """Entry point that forwards sys.argv to the fnug binary."""
    result = run(*sys.argv[1:])
    sys.exit(result.returncode)
