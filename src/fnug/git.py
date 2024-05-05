import re
from functools import cache
from pathlib import Path

from pygit2 import Repository, discover_repository


@cache
def _get_repo(path: Path) -> Repository | None:
    repo_path = discover_repository(path.as_posix())
    if not repo_path:
        return None
    return Repository(repo_path)


def _git_status(path: Path) -> list[str]:
    repo = _get_repo(path)
    if repo is None:
        raise ValueError(f"{path} is not inside a git repository")
    return list(repo.status().keys())


def detect_repo_changes(path: Path, regex: list[str] | None = None) -> bool:
    """Detect if a git repository has changes."""
    files = _git_status(path)
    if regex:
        files = [file for file in files if any(re.search(r, file) for r in regex)]
    return len(files) >= 1
