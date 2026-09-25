"""How start() and check() call the fnug binary, checked against a fake binary."""

from __future__ import annotations

import copy
from pathlib import Path

import pytest

import fnug
from fnug import Auto, Command, Config
from fnug.config import WorkspaceOptions


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


def test_temp_config_is_json(fake_fnug):
    fnug.check(demo_config())

    assert fake_fnug.last()["config"]["path"].endswith(".fnug.json")


def test_temp_config_root_cwd_is_caller_cwd(fake_fnug, tmp_path, monkeypatch):
    monkeypatch.chdir(tmp_path)

    fnug.check(demo_config())

    assert fake_fnug.last_config()["cwd"] == str(tmp_path.resolve())


def test_relative_cwd_resolved_against_caller(fake_fnug, tmp_path, monkeypatch):
    (tmp_path / "sub").mkdir()
    monkeypatch.chdir(tmp_path)

    fnug.start(demo_config(cwd="sub"))

    assert fake_fnug.last_config()["cwd"] == str((tmp_path / "sub").resolve())


def test_absolute_cwd_kept(fake_fnug, tmp_path, monkeypatch):
    project = tmp_path / "project"
    project.mkdir()
    monkeypatch.chdir(tmp_path)

    fnug.check(demo_config(cwd=str(project)))

    assert fake_fnug.last_config()["cwd"] == str(project.resolve())


@pytest.mark.parametrize("workspace", [True, WorkspaceOptions(paths=["crates/*"])])
def test_workspace_with_config_object_raises(fake_fnug, workspace):
    with pytest.raises(ValueError, match="workspace"):
        fnug.check(demo_config(workspace=workspace))

    assert fake_fnug.calls() == []


def test_disabled_workspace_is_allowed(fake_fnug):
    fnug.check(demo_config(workspace=False))

    assert fake_fnug.last_config()["workspace"] is False


def test_check_flag_mapping(fake_fnug):
    fnug.check(all_=True, no_workspace=True, fail_fast=True)

    assert fake_fnug.last()["argv"] == [
        "--no-workspace",
        "check",
        "--fail-fast",
        "--all",
    ]


def test_start_no_workspace(fake_fnug):
    fnug.start(no_workspace=True)

    assert fake_fnug.last()["argv"] == ["--no-workspace"]


def test_no_python_wrapper_env(fake_fnug, monkeypatch):
    monkeypatch.delenv("FNUG_PYTHON_WRAPPER", raising=False)

    fnug.check()

    assert "FNUG_PYTHON_WRAPPER" not in fake_fnug.last()["env"]


def test_workspace_options_exported():
    assert fnug.WorkspaceOptions is WorkspaceOptions
    assert "WorkspaceOptions" in fnug.__all__
