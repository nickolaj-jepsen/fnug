"""Configuration dataclasses for programmatic .fnug.yaml generation.

Fields are named after the config keys, except ``Config.schema``, which is written as
``$schema``. A field left as ``None`` is left out, so fnug uses its default or the
inherited value.
"""

from __future__ import annotations

import json
import re
from dataclasses import asdict, dataclass, field
from importlib import metadata
from pathlib import Path
from typing import Any

# A normalized PEP 440 version without an epoch.
_PEP440 = re.compile(
    r"(?P<release>\d+(?:\.\d+)*)"
    r"(?:(?P<pre>a|b|rc)(?P<pre_n>\d+))?"
    r"(?:\.post(?P<post>\d+))?"
    r"(?:\.dev(?P<dev>\d+))?"
    r"(?:\+(?P<local>[a-z0-9.]+))?"
)
_PRE_RELEASES = {"a": "alpha", "b": "beta", "rc": "rc"}


def semver_from_pep440(version: str) -> str:
    """Convert a normalized PEP 440 version to the Cargo version it was built from.

    This undoes maturin's conversion: ``0.1.0a13`` becomes ``0.1.0-alpha.13``,
    ``b``/``rc``/``.post``/``.dev`` parts become ``-beta.N``/``-rc.N``/``-post.N``/
    ``-dev.N``, and a local version becomes build metadata. A release with fewer than
    three numbers is padded with zeros. Anything else, such as a version with an epoch,
    is returned unchanged.
    """
    match = _PEP440.fullmatch(version)
    if match is None:
        return version
    release = match["release"].split(".")
    release += ["0"] * (3 - len(release))
    pre: list[str] = []
    if match["pre"]:
        pre += [_PRE_RELEASES[match["pre"]], match["pre_n"]]
    for part in ("post", "dev"):
        if match[part]:
            pre += [part, match[part]]
    result = ".".join(release)
    if pre:
        result += "-" + ".".join(pre)
    if match["local"]:
        result += "+" + match["local"]
    return result


def _package_fnug_version() -> str | None:
    """The installed fnug package's version as fnug reports it, or None without one."""
    try:
        return semver_from_pep440(metadata.version("fnug"))
    except metadata.PackageNotFoundError:
        return None


def _strip_none(obj: Any) -> Any:  # noqa: ANN401
    """Recursively remove None values from dicts and lists."""
    if isinstance(obj, dict):
        return {k: _strip_none(v) for k, v in obj.items() if v is not None}
    if isinstance(obj, list):
        return [_strip_none(item) for item in obj]
    return obj


@dataclass(slots=True, kw_only=True)
class Auto:
    """Automation rules that determine when commands should execute.

    Each field is inherited from the parent group unless set here.
    ``run_on_change`` makes the TUI run a command when a watched change selects it.
    """

    watch: bool | None = None
    git: bool | None = None
    path: list[str] | None = None
    regex: list[str] | None = None
    always: bool | None = None
    check: bool | None = None
    run_on_change: bool | None = None


@dataclass(slots=True, kw_only=True)
class Command:
    """A single executable command.

    ``id`` defaults to the name, with ``/`` replaced by ``-``; when several commands or
    groups would get the same default, each gets its group path instead, such as
    ``backend/test``. An explicit ``id`` must be unique and can't contain ``/``.
    ``depends_on`` takes ids, or names unique among the command's siblings or in the
    config.

    ``timeout`` is how long ``fnug check`` and MCP runs let the command run before
    killing it: whole seconds, or a duration such as ``"90s"`` or ``"5m"``; ``0``
    means no limit. ``exclusive`` keeps parallel runs from running anything alongside
    the command.
    """

    name: str
    cmd: str
    id: str | None = None
    cwd: str | None = None
    auto: Auto | None = None
    env: dict[str, str] | None = None
    depends_on: list[str] | None = None
    scrollback: int | None = None
    timeout: int | str | None = None
    exclusive: bool | None = None


@dataclass(slots=True, kw_only=True)
class CommandGroup:
    """A hierarchical grouping of related commands.

    ``id`` defaults to the name, as for a ``Command``. ``auto``, ``cwd``, ``env``,
    ``timeout`` and ``exclusive`` are defaults for everything in the group.
    """

    name: str
    id: str | None = None
    auto: Auto | None = None
    cwd: str | None = None
    commands: list[Command] | None = None
    children: list[CommandGroup] | None = None
    env: dict[str, str] | None = None
    timeout: int | str | None = None
    exclusive: bool | None = None


@dataclass(slots=True, kw_only=True)
class WorkspaceOptions:
    """Workspace discovery options for mono-repo setups."""

    paths: list[str] | None = None
    max_depth: int | None = None


@dataclass(slots=True, kw_only=True)
class Config:
    """Root configuration for a .fnug.yaml file: the root group plus file settings.

    ``schema`` is written as the ``$schema`` key, which editors use and fnug ignores.
    ``fnug_version`` defaults to the installed fnug package's version, in the form the
    binary reports (``0.1.0a13`` becomes ``0.1.0-alpha.13``), and is left out when the
    package isn't installed. With ``workspace``, the ids of each package's commands and
    groups are prefixed with the package's id, such as ``api/build``.
    """

    schema: str | None = None
    name: str
    fnug_version: str | None = field(default_factory=_package_fnug_version)
    id: str | None = None
    auto: Auto | None = None
    cwd: str | None = None
    commands: list[Command] | None = None
    children: list[CommandGroup] | None = None
    env: dict[str, str] | None = None
    timeout: int | str | None = None
    exclusive: bool | None = None
    workspace: bool | WorkspaceOptions | None = None

    def to_dict(self) -> dict[str, Any]:
        """Convert to a config dictionary, leaving out None and writing ``$schema``."""
        data = _strip_none(asdict(self))
        return {("$schema" if k == "schema" else k): v for k, v in data.items()}

    def to_yaml(self) -> str:
        """Serialize to a YAML string."""
        import yaml  # noqa: PLC0415

        return yaml.dump(
            self.to_dict(),
            default_flow_style=False,
            sort_keys=False,
        )

    def to_json(self, *, indent: int = 2) -> str:
        """Serialize to a JSON string."""
        return json.dumps(self.to_dict(), indent=indent)

    def write(self, path: str | Path) -> None:
        """Write configuration to a file.

        Format is auto-detected from the file extension:
        - .json -> JSON
        - .yaml / .yml -> YAML
        """
        path = Path(path)
        content = self.to_json() if path.suffix == ".json" else self.to_yaml()
        path.write_text(content)
