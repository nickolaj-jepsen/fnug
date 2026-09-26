"""Serialisation of the Config dataclasses, and their parity with `fnug schema`."""

from __future__ import annotations

import json
from dataclasses import MISSING, fields, is_dataclass

import pytest
import yaml

from fnug.config import (
    Auto,
    Command,
    CommandGroup,
    Config,
    WorkspaceOptions,
)

# The dataclass for each object in the schema; "#" is the root.
SCHEMA_OBJECTS = {
    "#": Config,
    "ConfigCommandGroup": CommandGroup,
    "ConfigCommand": Command,
    "ConfigAuto": Auto,
    "WorkspaceOptions": WorkspaceOptions,
}
# Fields whose config key isn't the field name.
KEYS = {"schema": "$schema"}
JSON_TYPES = {
    "null": type(None),
    "boolean": bool,
    "string": str,
    "array": list,
    "object": dict,
}


def full_auto():
    return Auto(
        watch=True,
        git=True,
        path=["src"],
        regex=[r"\.rs$"],
        always=False,
        check=True,
        run_on_change=True,
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
        timeout="5m",
        exclusive=True,
    )
    group = CommandGroup(
        name="rust",
        id="rust",
        auto=full_auto(),
        cwd="rust",
        commands=[command],
        children=[CommandGroup(name="leaf", commands=[Command(name="x", cmd="x")])],
        env={"B": "2"},
        timeout=60,
        exclusive=False,
    )
    return Config(
        schema="https://example.com/fnug.schema.json",
        name="root",
        fnug_version="0.1.0",
        id="root",
        auto=full_auto(),
        cwd=".",
        commands=[Command(name="fmt", cmd="cargo fmt")],
        children=[group],
        env={"C": "3"},
        timeout=0,
        exclusive=False,
        workspace=WorkspaceOptions(paths=["crates/*"], max_depth=2),
    )


def set_fields(value, found):
    """Record, per dataclass, the fields that are set anywhere in ``value``."""
    if is_dataclass(value):
        names = found.setdefault(type(value), set())
        for field in fields(value):
            item = getattr(value, field.name)
            if item is not None:
                names.add(field.name)
                set_fields(item, found)
    elif isinstance(value, list):
        for item in value:
            set_fields(item, found)
    return found


def is_type(value, name):
    if name == "integer":
        return isinstance(value, int) and not isinstance(value, bool)
    return isinstance(value, JSON_TYPES[name])


def validate(value, node, schema, where="config"):
    """Assert that ``value`` matches ``node``, in the JSON Schema subset fnug uses."""
    if "$ref" in node:
        definition = schema["definitions"][node["$ref"].removeprefix("#/definitions/")]
        validate(value, definition, schema, where)
    elif "anyOf" in node:
        validate_any_of(value, node["anyOf"], schema, where)
    else:
        types = node.get("type", [])
        types = [types] if isinstance(types, str) else types
        assert not types or any(is_type(value, t) for t in types), (
            f"{where}: {value!r} is not {types}"
        )
        if "minimum" in node:
            assert value >= node["minimum"], f"{where}: {value!r} is too small"
        if isinstance(value, dict):
            validate_object(value, node, schema, where)
        elif isinstance(value, list) and "items" in node:
            for i, item in enumerate(value):
                validate(item, node["items"], schema, f"{where}[{i}]")


def mismatch(value, node, schema, where):
    """Why ``value`` doesn't match ``node``, or None if it does."""
    try:
        validate(value, node, schema, where)
    except AssertionError as error:
        return str(error)
    return None


def validate_any_of(value, options, schema, where):
    failures = []
    for option in options:
        failure = mismatch(value, option, schema, where)
        if failure is None:
            return
        failures.append(failure)
    msg = f"{where}: {value!r} matches none of {failures}"
    raise AssertionError(msg)


def validate_object(value, node, schema, where):
    properties = node.get("properties", {})
    extra = node.get("additionalProperties", True)
    for key, item in value.items():
        if key in properties:
            validate(item, properties[key], schema, f"{where}.{key}")
        else:
            assert extra is not False, f"{where}: {key!r} is not a config key"
            if isinstance(extra, dict):
                validate(item, extra, schema, f"{where}.{key}")
    missing = set(node.get("required", [])) - set(value)
    assert not missing, f"{where}: {missing} missing"


def schema_object(schema, name):
    return schema if name == "#" else schema["definitions"][name]


def test_full_config_sets_every_field():
    found = set_fields(full_config(), {})

    for cls in SCHEMA_OBJECTS.values():
        assert found[cls] == {field.name for field in fields(cls)}, cls.__name__


@pytest.mark.parametrize(
    "make_config",
    [
        full_config,
        lambda: Config(name="minimal"),
        lambda: Config(name="flags", workspace=True, timeout="1h 30m"),
    ],
    ids=["full", "minimal", "flags"],
)
def test_to_dict_matches_schema(fnug_schema, make_config):
    validate(make_config().to_dict(), fnug_schema, fnug_schema)


def test_validate_rejects_what_fnug_rejects(fnug_schema):
    rejected = [
        {"name": "x", "bogus": True},
        {"name": "x", "schema": "https://example.com"},
        {"name": "x", "commands": [{"name": "a", "cmd": "a", "timeout": -1}]},
        {"name": "x", "auto": {"run_on_change": "yes"}},
        {"commands": []},
    ]
    for data in rejected:
        with pytest.raises(AssertionError):
            validate(data, fnug_schema, fnug_schema)


@pytest.mark.parametrize(("name", "cls"), SCHEMA_OBJECTS.items())
def test_every_schema_key_has_a_field(fnug_schema, name, cls):
    node = schema_object(fnug_schema, name)
    keys = {KEYS.get(field.name, field.name) for field in fields(cls)}
    required = {
        KEYS.get(field.name, field.name)
        for field in fields(cls)
        if field.default is MISSING and field.default_factory is MISSING
    }

    assert keys == set(node["properties"])
    assert required == set(node.get("required", []))


def test_config_to_dict_strips_none():
    data = Config(
        name="demo",
        fnug_version="0.1.0",
        commands=[Command(name="hello", cmd="echo hi", auto=Auto(git=True))],
    ).to_dict()

    assert data == {
        "name": "demo",
        "fnug_version": "0.1.0",
        "commands": [{"name": "hello", "cmd": "echo hi", "auto": {"git": True}}],
    }


def test_schema_is_written_first_as_dollar_schema():
    data = Config(name="demo", schema="https://example.com/s.json").to_dict()

    assert next(iter(data)) == "$schema"
    assert data["$schema"] == "https://example.com/s.json"
    assert "schema" not in data


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
