"""End-to-end runs of the wrapper against the real fnug binary."""

from __future__ import annotations

import os
import shutil
import subprocess
import sys
from pathlib import Path

import pytest

import fnug
from fnug import Auto, Command, Config

pytestmark = pytest.mark.integration


@pytest.fixture
def real_fnug(monkeypatch):
    """FNUG_TEST_BINARY, or the binary `maturin develop` installed next to Python.

    PATH is not searched, so a stale global install is never tested by accident.
    """
    binary = os.environ.get("FNUG_TEST_BINARY") or str(
        Path(sys.executable).parent / "fnug"
    )
    if not Path(binary).is_file():
        pytest.skip("no fnug binary: run `maturin develop` or set FNUG_TEST_BINARY")
    monkeypatch.setattr(fnug, "_find_binary", lambda: binary)
    return binary


@pytest.fixture
def git_repo(tmp_path):
    git = shutil.which("git")
    if git is None:
        pytest.skip("git is not installed")
    repo = tmp_path / "repo"
    repo.mkdir()
    subprocess.run([git, "init", "--quiet", str(repo)], check=True)
    return repo.resolve()


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
