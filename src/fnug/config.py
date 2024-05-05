from pathlib import Path
from typing import Any, Literal
from uuid import uuid4

import yaml
from pydantic import BaseModel, Field, TypeAdapter, model_validator


class ConfigAuto(BaseModel):
    """Config for auto selecting/running commands."""

    git: bool | None = None
    watch: bool | None = None
    always: bool | None = None
    regex: list[str] | None = None
    path: list[Path] | None = None

    def merge(self, other: "ConfigAuto"):
        """Merge two auto configs."""
        return ConfigAuto(
            git=self.git if self.git is not None else other.git,
            watch=self.watch if self.watch is not None else other.watch,
            always=self.always if self.always is not None else other.always,
            regex=self.regex if self.regex is not None else other.regex,
            path=self.path if self.path is not None else other.path,
        )

    @model_validator(mode="after")
    def ensure_path(self):
        """Ensure that path is set if git or watch is set."""
        if self.git and not self.path:
            raise ValueError("git auto requires path")
        if self.watch and not self.path:
            raise ValueError("watch auto requires path")
        return self


class ConfigCommand(BaseModel):
    """A command to run."""

    id: str = Field(default_factory=lambda: uuid4().hex)
    name: str
    cmd: str
    cwd: Path | None = None
    interactive: bool = False
    auto: ConfigAuto = ConfigAuto()


class ConfigCommandGroup(BaseModel):
    """A group of commands or subgroups."""

    id: str = Field(default_factory=lambda: uuid4().hex)
    name: str
    commands: list[ConfigCommand] = []
    children: list["ConfigCommandGroup"] = []
    auto: ConfigAuto = ConfigAuto()

    def _propagate_auto(self):
        """Propagate auto settings to all children."""
        for command in self.commands:
            command.auto = command.auto.merge(self.auto)

        for child in self.children:
            child.auto = child.auto.merge(self.auto)
            child._propagate_auto()

    @model_validator(mode="after")
    def _no_empty_groups(self):
        """Ensure that groups are not empty."""
        if not self.commands and not self.children:
            raise ValueError("A group must have at least one command or subgroup.")
        return self


class Config(ConfigCommandGroup):
    """The root config object."""

    fnug_version: Literal["0.1.0"]

    def model_post_init(self, __context: Any) -> None:
        """Post-init hook to propagate auto settings."""
        self._propagate_auto()


ConfigValidator = TypeAdapter(Config)


def load_config(path: Path) -> Config:
    """Load a config file."""
    if path.suffix in [".yaml", ".yml"]:
        data = yaml.safe_load(Path.open(path, "rb").read())
        return ConfigValidator.validate_python(data)
    return ConfigValidator.validate_json(Path.open(path, "rb").read())
