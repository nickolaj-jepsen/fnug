import re
import subprocess
from functools import cache
from pathlib import Path


@cache
def _detect_repo_root(path: Path) -> Path | None:
    if not path.exists():
        return None

    cmd = [
        "git",
        "rev-parse",
        "--show-toplevel",
    ]

    try:
        return Path(
            subprocess.check_output(
                cmd,
                cwd=path.as_posix(),
                stderr=subprocess.DEVNULL,
            )
            .decode()
            .strip()
        )
    except subprocess.CalledProcessError:
        return None


@cache
def _git_status(path: Path) -> list[str]:
    repo_path = _detect_repo_root(path)
    if repo_path is None:
        raise ValueError(f"{path} is not inside a git repository")

    cmd = [
        "git",
        "-C",
        repo_path.as_posix(),
        "status",
        "--porcelain=v1",
    ]

    sub_path = path.relative_to(repo_path)
    if sub_path != Path("."):
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


def detect_repo_changes(path: Path, regex: list[str] | None = None) -> bool:
    files = _git_status(path)
    if regex:
        files = [file for file in files if any(re.search(r, file) for r in regex)]
    return len(files) >= 1
