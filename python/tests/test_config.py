"""Serialisation of the Config dataclasses."""

from __future__ import annotations

import json

import yaml

from fnug.config import (
    Auto,
    Command,
    CommandGroup,
    Config,
    WorkspaceOptions,
)

# Keys the Rust config parser accepts at each level (src/config_file.rs).
GROUP_KEYS = {"id", "name", "auto", "cwd", "commands", "children", "env"}
ROOT_KEYS = GROUP_KEYS | {"fnug_version", "workspace"}
COMMAND_KEYS = {"id", "name", "cwd", "cmd", "auto", "env", "depends_on", "scrollback"}
AUTO_KEYS = {"watch", "git", "path", "regex", "always", "check"}
WORKSPACE_KEYS = {"paths", "max_depth"}


def full_auto():
    return Auto(
        watch=True,
        git=True,
        path=["src"],
        regex=[r"\.rs$"],
        always=False,
        check=True,
    )


def full_config():
    command = Command(
        name="test",
        cmd="cargo test",
        id="test",
        cwd="sub",
        auto=full_auto(),
        env={"A": "1"},
        depends_on=["fmt"],
        scrollback=100,
    )
    group = CommandGroup(
        name="rust",
        id="rust",
        auto=full_auto(),
        cwd="rust",
        commands=[command],
        children=[CommandGroup(name="leaf", commands=[Command(name="x", cmd="x")])],
        env={"B": "2"},
    )
    return Config(
        name="root",
        id="root",
        auto=full_auto(),
        cwd=".",
        commands=[Command(name="fmt", cmd="cargo fmt")],
        children=[group],
        env={"C": "3"},
        workspace=WorkspaceOptions(paths=["crates/*"], max_depth=2),
    )


def assert_known_keys(group, allowed):
    assert set(group) <= allowed
    if "auto" in group:
        assert set(group["auto"]) <= AUTO_KEYS
    for command in group.get("commands", []):
        assert set(command) <= COMMAND_KEYS
        if "auto" in command:
            assert set(command["auto"]) <= AUTO_KEYS
    for child in group.get("children", []):
        assert_known_keys(child, GROUP_KEYS)


def assert_no_none(value):
    assert value is not None
    if isinstance(value, dict):
        for item in value.values():
            assert_no_none(item)
    elif isinstance(value, list):
        for item in value:
            assert_no_none(item)


def test_config_to_dict_strips_none():
    data = Config(
        name="demo",
        commands=[Command(name="hello", cmd="echo hi", auto=Auto(git=True))],
    ).to_dict()

    assert data == {
        "name": "demo",
        "fnug_version": "0.1.0",
        "commands": [{"name": "hello", "cmd": "echo hi", "auto": {"git": True}}],
    }


def test_to_dict_emits_only_known_keys():
    data = full_config().to_dict()

    assert_no_none(data)
    assert_known_keys(data, ROOT_KEYS)
    assert set(data["workspace"]) <= WORKSPACE_KEYS


def test_to_json_roundtrip():
    config = full_config()

    assert json.loads(config.to_json()) == config.to_dict()


def test_to_yaml_roundtrip():
    config = full_config()

    assert yaml.safe_load(config.to_yaml()) == config.to_dict()


def test_write_by_suffix(tmp_path):
    config = full_config()
    json_path = tmp_path / ".fnug.json"
    yaml_path = tmp_path / ".fnug.yaml"

    config.write(json_path)
    config.write(yaml_path)

    assert json.loads(json_path.read_text()) == config.to_dict()
    assert yaml.safe_load(yaml_path.read_text()) == config.to_dict()
    assert not yaml_path.read_text().lstrip().startswith("{")
