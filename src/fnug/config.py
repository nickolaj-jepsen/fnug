from pathlib import Path
from typing import Any, Literal
from uuid import uuid4

import yaml
from pydantic import BaseModel, Field, TypeAdapter, field_validator, model_validator


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


class Dependency(BaseModel):
    """A dependency between commands."""

    path: Path
    always: bool = False
    once: bool = False


class _InternalDependency(Dependency):
    """Like dependency, but with the command object."""

    command: "ConfigCommand"


class ConfigCommand(BaseModel):
    """A command to run."""

    id: str = Field(default_factory=lambda: uuid4().hex)
    name: str
    cmd: str
    cwd: Path | None = None
    interactive: bool = False
    auto: ConfigAuto = ConfigAuto()
    raw_dependencies: list[Dependency] = Field(default_factory=list, alias="depends")
    dependencies: list[_InternalDependency] = Field(default_factory=list, exclude=True, alias="__internal_dependencies")
    path: Path = Field(default_factory=Path)

    @field_validator("raw_dependencies", mode="before")
    @classmethod
    def _parse_simple_dependencies(cls, v: Any) -> list[Dependency]:
        result: list[Dependency] = []
        for dep in v:
            if isinstance(dep, str):
                result.append(Dependency(path=Path(dep)))
            else:
                result.append(dep)

        return result


class ConfigCommandGroup(BaseModel):
    """A group of commands or subgroups."""

    id: str = Field(default_factory=lambda: uuid4().hex)
    name: str
    commands: list[ConfigCommand] = []
    children: list["ConfigCommandGroup"] = []
    auto: ConfigAuto = ConfigAuto()
    path: Path = Field(default_factory=Path)

    def _propagate_auto(self):
        """Propagate auto settings to all children."""
        for command in self.commands:
            command.auto = command.auto.merge(self.auto)

        for child in self.children:
            child.auto = child.auto.merge(self.auto)
            child._propagate_auto()

    def all_commands(self) -> list[ConfigCommand]:
        """Get all commands."""
        commands = list(self.commands)
        for group in self.children:
            commands.extend(group.all_commands())
        return commands

    def _dependency_map(self, path: Path = Path()) -> dict[Path, ConfigCommand]:
        """Get all commands."""
        commands: dict[Path, ConfigCommand] = {}
        for command in self.commands:
            commands[path / command.name] = command
        for group in self.children:
            commands.update(group._dependency_map(path / group.name))

        return commands

    def _resolve_dependencies(self, commands: dict[Path, ConfigCommand] | None = None):
        """Resolve dependencies."""
        commands = commands or self._dependency_map()

        for path, command in commands.items():
            for dep in command.raw_dependencies:
                dep_path = path.parent / dep.path
                if dep_path not in commands:
                    raise ValueError(f"Dependency {dep} not found for command {command.name}")

                command.dependencies.append(_InternalDependency(**dep.model_dump(), command=commands[dep_path]))


class Config(ConfigCommandGroup):
    """The root config object."""

    fnug_version: Literal["0.1.0"]

    def model_post_init(self, __context: Any) -> None:
        """Post-init hook to propagate autorun settings."""
        self._resolve_dependencies()
        self._propagate_auto()


ConfigValidator = TypeAdapter(Config)


def load_config(path: Path) -> Config:
    """Load a config file."""
    if path.suffix in [".yaml", ".yml"]:
        data = yaml.safe_load(Path.open(path, "rb").read())
        return ConfigValidator.validate_python(data)
    return ConfigValidator.validate_json(Path.open(path, "rb").read())
