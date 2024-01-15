from pathlib import Path
from typing import Literal
from pydantic import TypeAdapter, BaseModel


class ConfigAutoRun(BaseModel):
    type: Literal["git"]
    git_root: Path
    regex: list[str]
    sub_path: Path | None = None


class ConfigCommand(BaseModel):
    name: str
    cmd: str
    cwd: Path | None = None
    autorun: ConfigAutoRun | None = None


class ConfigCommandGroup(BaseModel):
    name: str
    commands: list[ConfigCommand]
    children: list["ConfigCommandGroup"] = []


class ConfigRoot(ConfigCommandGroup):
    fnug_version: Literal["0.1.0"]


RootConfigValidator = TypeAdapter(ConfigRoot)


def load_config(path: Path) -> ConfigRoot:
    return RootConfigValidator.validate_json(open(path, "rb").read())
