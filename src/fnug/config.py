from pathlib import Path
from typing import Literal, Any, Self
from pydantic import TypeAdapter, BaseModel, model_validator
import yaml


class ConfigAutoRun(BaseModel):
    git: bool | None = None
    watch: bool | None = None
    always: bool | None = None
    regex: list[str] | None = None
    path: list[Path] | None = None

    def merge(self, other: Self):
        return ConfigAutoRun(
            git=self.git if self.git is not None else other.git,
            watch=self.watch if self.watch is not None else other.watch,
            always=self.always if self.always is not None else other.always,
            regex=self.regex if self.regex is not None else other.regex,
            path=self.path if self.path is not None else other.path,
        )

    @model_validator(mode="after")
    def ensure_path(self) -> Self:
        if self.git and not self.path:
            raise ValueError("git autorun requires path")
        if self.watch and not self.path:
            raise ValueError("watch autorun requires path")
        return self


class ConfigCommand(BaseModel):
    name: str
    cmd: str
    cwd: Path | None = None
    autorun: ConfigAutoRun = ConfigAutoRun()


class ConfigCommandGroup(BaseModel):
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


class ConfigRoot(ConfigCommandGroup):
    fnug_version: Literal["0.1.0"]

    def model_post_init(self, __context: Any) -> None:
        self._propagate_autorun()


RootConfigValidator = TypeAdapter(ConfigRoot)


def load_config(path: Path) -> ConfigRoot:
    if path.suffix in [".yaml", ".yml"]:
        data = yaml.safe_load(open(path, "rb").read())
        return RootConfigValidator.validate_python(data)
    return RootConfigValidator.validate_json(open(path, "rb").read())
