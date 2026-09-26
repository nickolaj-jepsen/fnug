//! The git CLI in tests, isolated from the user's git config and from a git hook that runs the
//! tests.

use std::path::Path;
use std::process::Command;

/// Environment variables through which an outer git process, such as one running the tests from
/// a hook, would point git and fnug at its own repository.
const OUTER_REPO_VARS: &[&str] = &["GIT_DIR", "GIT_INDEX_FILE", "GIT_WORK_TREE", "GIT_PREFIX"];

/// Make `command`, git or fnug, ignore the global and system git config and any outer repository,
/// and commit as a fixed identity.
pub fn isolate(command: &mut Command) -> &mut Command {
    for var in OUTER_REPO_VARS {
        command.env_remove(var);
    }
    command
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
}

/// An isolated `git` command in `dir`.
pub fn command(dir: &Path) -> Command {
    let mut command = Command::new("git");
    isolate(&mut command).current_dir(dir);
    command
}

/// Run git with `args` in `dir` and return its stdout. Panics if git fails.
pub fn git(dir: &Path, args: &[&str]) -> String {
    let output = command(dir).args(args).output().unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Whether the git CLI runs here. Tests that need it skip themselves when it doesn't, as in the
/// nix build sandbox.
pub fn available() -> bool {
    let found = Command::new("git")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    if !found {
        eprintln!("skipping: git is not available");
    }
    found
}

/// A new repo in `dir` whose hooks, in `.git/hooks`, run even when the user's config sets
/// `core.hooksPath`.
pub fn init(dir: &Path) {
    git(dir, &["init", "-q"]);
    git(dir, &["config", "core.hooksPath", ".git/hooks"]);
}
