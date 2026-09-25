"""Fixtures for the wrapper tests: a fake fnug binary that records its calls."""

from __future__ import annotations

import json
import sys
from dataclasses import dataclass
from typing import TYPE_CHECKING, Any

import pytest
import yaml

import fnug

if TYPE_CHECKING:
    from pathlib import Path

# Copies the config while it runs, because the wrapper deletes its temp file afterwards.
FAKE_BINARY = """\
#!{python}
import json
import os
import sys
from pathlib import Path

args = sys.argv[1:]
config = None
if "--config" in args:
    path = Path(args[args.index("--config") + 1])
    text = path.read_text() if path.is_file() else None
    config = {{"path": str(path), "text": text}}
record = {{"argv": args, "cwd": os.getcwd(), "env": dict(os.environ), "config": config}}
with open(os.environ["FNUG_FAKE_LOG"], "a") as log:
    log.write(json.dumps(record) + "\\n")
sys.exit(int(os.environ.get("FNUG_FAKE_EXIT", "0")))
"""


@dataclass
class FakeFnug:
    """The fake binary and the calls it has recorded."""

    path: Path
    log: Path

    def calls(self) -> list[dict[str, Any]]:
        if not self.log.exists():
            return []
        return [json.loads(line) for line in self.log.read_text().splitlines()]

    def last(self) -> dict[str, Any]:
        calls = self.calls()
        assert calls, "the fake fnug binary was never run"
        return calls[-1]

    def last_config(self) -> dict[str, Any]:
        """The config file passed to the last call, parsed by its suffix."""
        config = self.last()["config"]
        assert config is not None, "the last call had no --config"
        if config["path"].endswith(".json"):
            return json.loads(config["text"])
        return yaml.safe_load(config["text"])


@pytest.fixture
def fake_fnug(
    tmp_path_factory: pytest.TempPathFactory,
    monkeypatch: pytest.MonkeyPatch,
) -> FakeFnug:
    root = tmp_path_factory.mktemp("fake-fnug")
    binary = root / "fnug"
    binary.write_text(FAKE_BINARY.format(python=sys.executable))
    binary.chmod(0o755)
    log = root / "calls.jsonl"
    monkeypatch.setenv("FNUG_FAKE_LOG", str(log))
    monkeypatch.delenv("FNUG_FAKE_EXIT", raising=False)
    monkeypatch.setattr(fnug, "_find_binary", lambda: str(binary))
    return FakeFnug(binary, log)
