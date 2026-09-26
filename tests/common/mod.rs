//! Helpers shared by the integration test files.
#![allow(dead_code)]

pub mod git;
pub mod pty;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::time::{Duration, Instant};

use fnug::commands::group::CommandGroup;

/// How long a test waits for a process, or for a file its commands write, before failing.
pub const TIMEOUT: Duration = Duration::from_secs(10);

// ─── files and configs ───

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

/// Write `content` to `path` under `dir`, creating its parent directories.
pub fn write(dir: &Path, path: &str, content: impl AsRef<[u8]>) {
    let path = dir.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

/// Write an executable file to `path`, creating its parent directories.
pub fn write_executable(path: &Path, content: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

pub fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap()
}

// ─── the fnug binary ───

/// The fnug binary with `args`, in `dir`, with stdin closed, git isolated as [`git::isolate`]
/// does, and `FNUG_LOG` unset, since it would change the stderr threshold.
pub fn fnug_command(dir: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_fnug"));
    git::isolate(&mut command)
        .current_dir(dir)
        .args(args)
        .env_remove("FNUG_LOG")
        .stdin(Stdio::null());
    command
}

/// Run [`fnug_command`] to completion.
pub fn fnug(dir: &Path, args: &[&str]) -> Output {
    fnug_command(dir, args).output().unwrap()
}

/// `fnug --no-workspace check --no-tui` with `args`, set up as [`fnug_command`] does.
pub fn check_command(dir: &Path, args: &[&str]) -> Command {
    let mut command = fnug_command(dir, &["--no-workspace", "check", "--no-tui"]);
    command.args(args);
    command
}

pub fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

// ─── processes ───

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

/// Wait for `child` to exit. Kills it and panics if it is still running after [`TIMEOUT`].
pub fn wait_exit(child: &mut Child) -> ExitStatus {
    let mut status = None;
    let exited = wait_until(TIMEOUT, || {
        status = child.try_wait().unwrap();
        status.is_some()
    });
    if !exited {
        let _ = child.kill();
        panic!("process {} did not exit within {TIMEOUT:?}", child.id());
    }
    status.unwrap()
}

/// Wait for a command to write its pid to `path`.
pub fn read_pid(path: &Path) -> i32 {
    let mut pid = None;
    assert!(
        wait_until(TIMEOUT, || {
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

/// Send `signal` to `pid`, one of the test's own descendants.
pub fn signal(pid: u32, signal: libc::c_int) {
    // SAFETY: plain syscall.
    unsafe { libc::kill(pid.cast_signed(), signal) };
}

/// Kills a process when dropped, so a failed assertion doesn't leave it running.
pub struct KillOnDrop(pub i32);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        // SAFETY: plain syscall; the pid is one of the test's own descendants.
        unsafe { libc::kill(self.0, libc::SIGKILL) };
    }
}

// ─── repositories made with libgit2 ───

/// Stage everything in the repo's work tree, deletions included, and commit it.
pub fn commit_all(repo: &git2::Repository) {
    let mut index = repo.index().unwrap();
    index
        .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
        .unwrap();
    index.update_all(["*"], None).unwrap();
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

/// A repo whose git dir, `gitdir`, lives outside its work tree, `workdir`, linked by a `.git`
/// file as in a submodule.
pub fn init_gitlink_repo(gitdir: &Path, workdir: &Path) -> git2::Repository {
    git2::Repository::init_opts(
        gitdir,
        git2::RepositoryInitOptions::new()
            .no_dotgit_dir(true)
            .workdir_path(workdir),
    )
    .unwrap()
}
