from pathlib import Path
from typing import Literal
from pydantic import TypeAdapter, BaseModel
import yaml


class ConfigAutoRun(BaseModel):
    type: Literal["git"]
    git_root: Path
    regex: list[str]
    sub_path: Path | None = None


class ConfigCommand(BaseModel):
    name: str
    cmd: str
    cwd: Path | None = None
    autorun: ConfigAutoRun | bool | None = None


class ConfigCommandGroup(BaseModel):
    name: str
    commands: list[ConfigCommand] = []
    children: list["ConfigCommandGroup"] = []
    autorun: ConfigAutoRun | bool | None = None


class ConfigRoot(ConfigCommandGroup):
    fnug_version: Literal["0.1.0"]


RootConfigValidator = TypeAdapter(ConfigRoot)


def load_config(path: Path) -> ConfigRoot:
    if path.suffix in [".yaml", ".yml"]:
        data = yaml.safe_load(open(path, "rb").read())
        return RootConfigValidator.validate_python(data)
    return RootConfigValidator.validate_json(open(path, "rb").read())
