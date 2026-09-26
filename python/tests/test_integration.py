"""End-to-end runs of the wrapper against the real fnug binary."""

from __future__ import annotations

import os
import shutil
import subprocess
from typing import TYPE_CHECKING

import pytest

import fnug
from fnug import Auto, Command, CommandGroup, Config, WorkspaceOptions

if TYPE_CHECKING:
    from pathlib import Path

pytestmark = pytest.mark.integration


@pytest.fixture
def git_repo(tmp_path):
    git = shutil.which("git")
    if git is None:
        pytest.skip("git is not installed")
    repo = tmp_path / "repo"
    repo.mkdir()
    subprocess.run([git, "init", "--quiet", str(repo)], check=True)
    return repo.resolve()


def _git(repo: Path, *args: str) -> None:
    subprocess.run(["git", *args], cwd=repo, check=True)  # noqa: S607


@pytest.fixture
def isolated_git(monkeypatch):
    """Ignore the user's git config, and commit as a fixed identity."""
    monkeypatch.setenv("GIT_CONFIG_GLOBAL", os.devnull)
    monkeypatch.setenv("GIT_CONFIG_NOSYSTEM", "1")
    for role in ("AUTHOR", "COMMITTER"):
        monkeypatch.setenv(f"GIT_{role}_NAME", "test")
        monkeypatch.setenv(f"GIT_{role}_EMAIL", "test@example.com")
    for var in ("GIT_DIR", "GIT_INDEX_FILE", "GIT_WORK_TREE"):
        monkeypatch.delenv(var, raising=False)


@pytest.mark.usefixtures("real_fnug", "isolated_git")
def test_integration_base_and_stash_with_config_object(git_repo, monkeypatch, capfd):
    src = git_repo / "src"
    src.mkdir()
    (src / "a.txt").write_text("ok\n")
    _git(git_repo, "add", "-A")
    _git(git_repo, "commit", "-qm", "init")
    monkeypatch.chdir(git_repo)
    config = Config(
        name="demo",
        commands=[
            Command(
                name="lint",
                cmd="! grep -rn BAD src/",
                auto=Auto(git=True, path=["src"]),
            ),
        ],
    )

    (src / "a.txt").write_text("ok\nfine\n")
    result = fnug.check(config, base="HEAD", no_tui=True)
    output = capfd.readouterr()
    assert result.returncode == 0, output
    assert "lint PASS" in output.err

    # BAD is staged, and gone from the work tree
    (src / "a.txt").write_text("ok\nBAD\n")
    _git(git_repo, "add", "src/a.txt")
    (src / "a.txt").write_text("ok\n")
    result = fnug.check(config, staged=True, stash=True, no_tui=True)
    output = capfd.readouterr()
    assert result.returncode == 1, output
    assert "src/a.txt:2:BAD" in output.err
    assert (src / "a.txt").read_text() == "ok\n"


@pytest.mark.usefixtures("real_fnug")
def test_integration_git_selection_from_caller_repo(git_repo, monkeypatch, capfd):
    (git_repo / "changed.txt").write_text("hello\n")
    monkeypatch.chdir(git_repo)
    config = Config(
        name="demo",
        commands=[Command(name="where", cmd="pwd", auto=Auto(git=True))],
    )

    result = fnug.check(config, no_tui=True)

    output = capfd.readouterr()
    assert result.returncode == 0, output
    assert str(git_repo) in output.out + output.err


@pytest.mark.parametrize(
    "workspace", [True, WorkspaceOptions(paths=["packages/*"])], ids=["walk", "paths"]
)
@pytest.mark.usefixtures("real_fnug")
def test_integration_workspace_config_object(tmp_path, monkeypatch, capfd, workspace):
    package = tmp_path / "packages" / "api"
    package.mkdir(parents=True)
    (package / ".fnug.yaml").write_text(
        "name: api\ncommands:\n  - name: hello\n    cmd: echo hello from $PWD\n"
    )
    monkeypatch.chdir(tmp_path)
    config = Config(
        name="root",
        workspace=workspace,
        commands=[Command(name="where", cmd="echo root at $PWD")],
    )

    result = fnug.check(config, all_=True, no_tui=True)

    output = capfd.readouterr()
    assert result.returncode == 0, output
    assert f"hello from {package.resolve()}" in output.out, output
    assert f"root at {tmp_path.resolve()}" in output.out, output


def test_integration_default_fnug_version_is_the_binarys(real_fnug):
    default = Config(name="demo").fnug_version
    if default is None:
        pytest.skip("the fnug package is not installed")

    result = subprocess.run(
        [real_fnug, "--version"], check=True, capture_output=True, text=True
    )

    assert result.stdout.split() == ["fnug", default]


@pytest.mark.usefixtures("real_fnug")
def test_integration_config_keys_are_accepted(tmp_path, monkeypatch, capfd):
    monkeypatch.chdir(tmp_path)
    config = Config(
        schema="https://example.com/fnug.schema.json",
        name="demo",
        timeout="5m",
        exclusive=False,
        children=[
            CommandGroup(
                name="group",
                timeout=60,
                exclusive=False,
                commands=[
                    Command(
                        name="ok",
                        cmd="true",
                        timeout=10,
                        exclusive=True,
                        auto=Auto(always=True, watch=True, run_on_change=True),
                    ),
                ],
            ),
        ],
    )

    result = fnug.check(config, all_=True, no_tui=True)

    output = capfd.readouterr()
    assert result.returncode == 0, output
    assert "ok PASS" in output.err
