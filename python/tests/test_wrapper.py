"""How start() and check() call the fnug binary, checked against a fake binary."""

from __future__ import annotations

import copy
from pathlib import Path

import pytest

import fnug
from fnug import Auto, Command, Config


def demo_config(**kwargs):
    return Config(
        name="demo",
        commands=[Command(name="where", cmd="pwd", auto=Auto(always=True))],
        **kwargs,
    )


def test_check_with_config_path(fake_fnug):
    result = fnug.check(config_path="project/.fnug.yaml")

    assert result.returncode == 0
    assert fake_fnug.last()["argv"] == ["--config", "project/.fnug.yaml", "check"]


def test_check_flags_follow_the_subcommand(fake_fnug):
    fnug.check(fail_fast=True, no_tui=True, mute_success=True, log_file="fnug.log")

    assert fake_fnug.last()["argv"] == [
        "--log-file",
        "fnug.log",
        "check",
        "--fail-fast",
        "--no-tui",
        "--mute-success",
    ]


def test_start_with_config_path(fake_fnug):
    fnug.start(config_path="project/.fnug.yaml", log_file="fnug.log")

    assert fake_fnug.last()["argv"] == [
        "--config",
        "project/.fnug.yaml",
        "--log-file",
        "fnug.log",
    ]


def test_returncode_is_passed_through(fake_fnug, monkeypatch):
    monkeypatch.setenv("FNUG_FAKE_EXIT", "3")

    assert fnug.check().returncode == 3
    assert len(fake_fnug.calls()) == 1


def test_config_and_config_path_are_exclusive(fake_fnug):
    with pytest.raises(ValueError, match="config_path"):
        fnug.check(demo_config(), config_path=".fnug.yaml")

    assert fake_fnug.calls() == []


def test_config_object_is_written_to_a_temp_file(fake_fnug):
    fnug.check(demo_config(), no_tui=True)

    argv = fake_fnug.last()["argv"]
    assert argv[0] == "--config"
    assert argv[2:] == ["check", "--no-tui"]
    assert fake_fnug.last_config()["commands"] == [
        {"name": "where", "cmd": "pwd", "auto": {"always": True}},
    ]


def test_temp_config_removed(fake_fnug):
    fnug.check(demo_config())
    fnug.start(demo_config())

    for call in fake_fnug.calls():
        assert not Path(call["config"]["path"]).exists()


def test_config_object_not_mutated(fake_fnug):
    config = demo_config(cwd="sub")
    before = copy.deepcopy(config)

    fnug.check(config)

    assert config == before
    assert fake_fnug.calls()
