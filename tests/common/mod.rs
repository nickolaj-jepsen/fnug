//! Helpers shared by the integration test files.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use fnug::commands::group::CommandGroup;

/// Write `content` to `.fnug.yaml` in `dir` and return the file's path.
pub fn write_config(dir: &Path, content: &str) -> PathBuf {
    let path = dir.join(".fnug.yaml");
    std::fs::write(&path, content).unwrap();
    path
}

/// Write `content` as the config in `dir` and load it without workspace resolution.
pub fn load(dir: &Path, content: &str) -> (CommandGroup, PathBuf) {
    let path = write_config(dir, content);
    fnug::load_config(path.to_str(), true).unwrap()
}

/// Poll `condition` until it holds or `timeout` passes; returns whether it held.
pub fn wait_until(timeout: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if condition() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Wait for a command to write its pid to `path`.
pub fn read_pid(path: &Path) -> i32 {
    let mut pid = None;
    assert!(
        wait_until(Duration::from_secs(10), || {
            pid = std::fs::read_to_string(path)
                .ok()
                .and_then(|s| s.trim().parse().ok());
            pid.is_some()
        }),
        "{} was never written",
        path.display()
    );
    pid.unwrap()
}

/// Whether `pid` is a live process. A zombie counts as dead, since whoever reaps orphans may
/// take a while.
pub fn process_alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks that the pid exists.
    if unsafe { libc::kill(pid, 0) } != 0 {
        return false;
    }
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        // The state follows the parenthesised command name.
        Ok(stat) => stat
            .rsplit_once(')')
            .is_none_or(|(_, rest)| !rest.trim_start().starts_with('Z')),
        Err(_) => true,
    }
}

/// Stage everything in the repo's work tree and commit it.
pub fn commit_all(repo: &git2::Repository) {
    let mut index = repo.index().unwrap();
    index
        .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
        .unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let sig = git2::Signature::now("test", "test@example.com").unwrap();
    let parents: Vec<git2::Commit> = repo
        .head()
        .ok()
        .and_then(|h| h.peel_to_commit().ok())
        .into_iter()
        .collect();
    let parents: Vec<&git2::Commit> = parents.iter().collect();
    repo.commit(Some("HEAD"), &sig, &sig, "commit", &tree, &parents)
        .unwrap();
}
