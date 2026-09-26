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
from typing import TYPE_CHECKING, Literal

from fnug.config import Auto, Command, CommandGroup, Config, WorkspaceOptions

if TYPE_CHECKING:
    from collections.abc import Iterator, Sequence

LogLevel = Literal["off", "error", "warn", "info", "debug", "trace"]

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
def _config_tempfile(config: Config, root: Path) -> Iterator[str]:
    """Write a Config to a temporary JSON file, yield its path, and delete it after.

    The written copy pins the root ``cwd`` to the absolute directory ``root``: an unset
    ``cwd`` becomes ``root`` and a relative one is resolved against it. ``config``
    itself is not modified.
    """
    data = config.to_dict()
    data["cwd"] = str((root / data.get("cwd", ".")).resolve())
    # Outside the project, so the file itself never counts as a changed file.
    fd, path = tempfile.mkstemp(suffix=".fnug.json")
    try:
        with os.fdopen(fd, "w") as file:
            json.dump(data, file)
        yield path
    finally:
        Path(path).unlink(missing_ok=True)


def _global_args(
    *,
    log_file: str | Path | None,
    log_level: LogLevel | None,
    no_workspace: bool,
) -> list[str]:
    """Build the flags before any subcommand, except ``--config`` and ``--root``."""
    args: list[str] = []
    if log_file is not None:
        args.extend(["--log-file", str(log_file)])
    if log_level is not None:
        args.extend(["--log-level", log_level])
    if no_workspace:
        args.append("--no-workspace")
    return args


def _run_with_config(
    config: Config | None,
    config_path: str | Path | None,
    root: str | Path | None,
    *args: str,
) -> subprocess.CompletedProcess[bytes]:
    """Run fnug with ``--config`` and ``--root`` for the given config, then ``args``.

    ``config`` is passed through a temporary file, and ``--root`` is then ``root``
    resolved against the caller's working directory, or that directory itself, so fnug
    acts from there rather than from the temporary file's directory.

    Raises:
        ValueError: If both config and config_path are provided.
    """
    if config is not None and config_path is not None:
        msg = "Cannot specify both 'config' and 'config_path'"
        raise ValueError(msg)
    if config is None:
        flags: list[str] = []
        if config_path is not None:
            flags.extend(["--config", str(config_path)])
        if root is not None:
            flags.extend(["--root", str(root)])
        return run(*flags, *args)
    root_dir = (Path.cwd() / (root if root is not None else ".")).resolve()
    with _config_tempfile(config, root_dir) as path:
        return run("--config", path, "--root", str(root_dir), *args)


def start(  # noqa: PLR0913
    config: Config | None = None,
    *,
    config_path: str | Path | None = None,
    root: str | Path | None = None,
    log_file: str | Path | None = None,
    log_level: LogLevel | None = None,
    no_workspace: bool = False,
) -> subprocess.CompletedProcess[bytes]:
    """Launch the fnug TUI.

    Args:
        config: A Config to use. It is written to a temporary file, and its paths
            and workspace discovery are resolved against ``root``, or else the
            caller's working directory.
        config_path: Path to an existing .fnug.yaml file.
        root: Resolve the config's paths and workspace against this directory
            instead of the config's own; without a config, search for one from it.
        log_file: Path for file logging.
        log_level: ``"off"``, ``"error"``, ``"warn"``, ``"info"``, ``"debug"`` or
            ``"trace"``.
        no_workspace: Don't resolve upward to a parent workspace root.

    Returns:
        The completed process result.

    Raises:
        ValueError: If both config and config_path are provided.
    """
    args = _global_args(
        log_file=log_file,
        log_level=log_level,
        no_workspace=no_workspace,
    )
    return _run_with_config(config, config_path, root, *args)


def check(  # noqa: PLR0913
    config: Config | None = None,
    *,
    config_path: str | Path | None = None,
    targets: Sequence[str] = (),
    all_: bool = False,
    include_manual: bool = False,
    base: str | None = None,
    staged: bool = False,
    stash: bool = False,
    fail_fast: bool = False,
    no_tui: bool = False,
    mute_success: bool = False,
    jobs: int | None = None,
    timeout: str | int | None = None,
    allow_modifications: bool = False,
    no_workspace: bool = False,
    root: str | Path | None = None,
    log_file: str | Path | None = None,
    log_level: LogLevel | None = None,
) -> subprocess.CompletedProcess[bytes]:
    """Run fnug in headless check mode.

    Without ``targets``, ``all_``, ``base`` or ``staged``, it runs the commands that
    uncommitted changes select. Exit code 0 means every command passed, 1 that a
    check failed, and 2 that fnug couldn't run them.

    Args:
        config: A Config to use. It is written to a temporary file, and its paths
            and workspace discovery are resolved against ``root``, or else the
            caller's working directory.
        config_path: Path to an existing .fnug.yaml file.
        targets: Run these commands, by id or name, after their dependencies.
        all_: Run every command, except those with ``auto.check: false`` unless
            ``include_manual`` is set.
        include_manual: Also run commands with ``auto.check: false``.
        base: Select by the changes since the merge base of HEAD and this ref,
            such as ``"origin/main"``.
        staged: Select by the changes staged for the next commit.
        stash: With ``staged``, set unstaged changes aside while commands run.
        fail_fast: Stop on first failure.
        no_tui: Never prompt to open the TUI on failure.
        mute_success: Suppress output for commands that pass.
        jobs: Run up to this many commands at once; 0 means one per CPU.
        timeout: Kill commands that run longer than this, in seconds or as a
            duration such as ``"5m"``, unless their config sets ``timeout``.
        allow_modifications: Don't fail commands that change tracked files.
        no_workspace: Don't resolve upward to a parent workspace root.
        root: Resolve the config's paths and workspace against this directory
            instead of the config's own; without a config, search for one from it.
        log_file: Path for file logging.
        log_level: ``"off"``, ``"error"``, ``"warn"``, ``"info"``, ``"debug"`` or
            ``"trace"``.

    Returns:
        The completed process result.

    Raises:
        ValueError: If both config and config_path are provided.
    """
    args = _global_args(
        log_file=log_file,
        log_level=log_level,
        no_workspace=no_workspace,
    )
    args.append("check")
    flags = {
        "--fail-fast": fail_fast,
        "--no-tui": no_tui,
        "--mute-success": mute_success,
        "--all": all_,
        "--include-manual": include_manual,
        "--staged": staged,
        "--stash": stash,
        "--allow-modifications": allow_modifications,
    }
    args.extend(flag for flag, enabled in flags.items() if enabled)
    if base is not None:
        args.extend(["--base", base])
    if jobs is not None:
        args.extend(["--jobs", str(jobs)])
    if timeout is not None:
        args.extend(["--timeout", str(timeout)])
    if targets:
        args.extend(["--", *targets])
    return _run_with_config(config, config_path, root, *args)


def main() -> None:
    """Entry point that forwards sys.argv to the fnug binary."""
    result = run(*sys.argv[1:])
    sys.exit(result.returncode)
