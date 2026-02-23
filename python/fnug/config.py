"""Configuration dataclasses for programmatic .fnug.yaml generation."""

from __future__ import annotations

import json
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Any


def _strip_none(obj: Any) -> Any:  # noqa: ANN401
    """Recursively remove None values from dicts and lists."""
    if isinstance(obj, dict):
        return {k: _strip_none(v) for k, v in obj.items() if v is not None}
    if isinstance(obj, list):
        return [_strip_none(item) for item in obj]
    return obj


@dataclass(slots=True, kw_only=True)
class Auto:
    """Automation rules that determine when commands should execute."""

    watch: bool | None = None
    git: bool | None = None
    path: list[str] | None = None
    regex: list[str] | None = None
    always: bool | None = None


@dataclass(slots=True, kw_only=True)
class Command:
    """A single executable command."""

    name: str
    cmd: str
    id: str | None = None
    cwd: str | None = None
    auto: Auto | None = None
    env: dict[str, str] | None = None
    depends_on: list[str] | None = None
    scrollback: int | None = None


@dataclass(slots=True, kw_only=True)
class CommandGroup:
    """A hierarchical grouping of related commands."""

    name: str
    id: str | None = None
    auto: Auto | None = None
    cwd: str | None = None
    commands: list[Command] | None = None
    children: list[CommandGroup] | None = None
    env: dict[str, str] | None = None


@dataclass(slots=True, kw_only=True)
class Config:
    """Root configuration for a .fnug.yaml file."""

    name: str
    fnug_version: str = "0.1.0"
    id: str | None = None
    auto: Auto | None = None
    cwd: str | None = None
    commands: list[Command] | None = None
    children: list[CommandGroup] | None = None
    env: dict[str, str] | None = None

    def to_dict(self) -> dict[str, Any]:
        """Convert to a dictionary with None values stripped."""
        return _strip_none(asdict(self))

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
