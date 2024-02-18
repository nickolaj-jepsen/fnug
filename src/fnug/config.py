from pathlib import Path
from typing import Any, Literal
from uuid import uuid4

import yaml
from pydantic import BaseModel, Field, TypeAdapter, model_validator


class ConfigAutoRun(BaseModel):
    """Config for autorun."""

    git: bool | None = None
    watch: bool | None = None
    always: bool | None = None
    regex: list[str] | None = None
    path: list[Path] | None = None

    def merge(self, other: "ConfigAutoRun"):
        """Merge two autorun configs."""
        return ConfigAutoRun(
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
            raise ValueError("git autorun requires path")
        if self.watch and not self.path:
            raise ValueError("watch autorun requires path")
        return self


class ConfigCommand(BaseModel):
    """A command to run."""

    id: str = Field(default_factory=lambda: uuid4().hex)
    name: str
    cmd: str
    cwd: Path | None = None
    interactive: bool = False
    autorun: ConfigAutoRun = ConfigAutoRun()


class ConfigCommandGroup(BaseModel):
    """A group of commands or subgroups."""

    id: str = Field(default_factory=lambda: uuid4().hex)
    name: str
    commands: list[ConfigCommand] = []
    children: list["ConfigCommandGroup"] = []
    autorun: ConfigAutoRun = ConfigAutoRun()

    def _propagate_autorun(self):
        """Propagate autorun settings to all children."""
        for command in self.commands:
            command.autorun = command.autorun.merge(self.autorun)

        for child in self.children:
            child.autorun = child.autorun.merge(self.autorun)
            child._propagate_autorun()


class Config(ConfigCommandGroup):
    """The root config object."""

    fnug_version: Literal["0.1.0"]

    def model_post_init(self, __context: Any) -> None:
        """Post-init hook to propagate autorun settings."""
        self._propagate_autorun()


ConfigValidator = TypeAdapter(Config)


def load_config(path: Path) -> Config:
    """Load a config file."""
    if path.suffix in [".yaml", ".yml"]:
        data = yaml.safe_load(Path.open(path, "rb").read())
        return ConfigValidator.validate_python(data)
    return ConfigValidator.validate_json(Path.open(path, "rb").read())
