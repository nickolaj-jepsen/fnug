import re
import subprocess
from functools import cache
from pathlib import Path


@cache
def _git_status(repo_path: Path, sub_path: Path | None = None) -> list[str]:
    cmd = [
        "git",
        "-C",
        repo_path.as_posix(),
        "status",
        "--porcelain=v1",
    ]

    if sub_path:
        cmd.append(sub_path.as_posix())

    lines = (
        subprocess.check_output(
            cmd,
            stderr=subprocess.DEVNULL,
        )
        .decode()
        .strip()
        .splitlines()
    )

    return [line[3:] for line in lines]


def detect_repo_changes(repo_path: Path, sub_path: Path | None = None, regex: list[str] | None = None) -> bool:
    files = _git_status(repo_path, sub_path)
    if regex:
        files = [file for file in files if any(re.search(r, file) for r in regex)]
    return len(files) >= 1
