//! `--stash`: check exactly the content being committed, by setting unstaged changes to tracked
//! files aside for the run.
//!
//! Like the pre-commit framework, it saves those changes as a binary patch in the git dir,
//! checks the work tree out from the index, and applies the patch again once every command has
//! exited. It drives the git CLI, so the index `git commit -a` and `git commit <paths>` hand
//! their hooks in `GIT_INDEX_FILE` applies. Untracked files are left alone, and `git stash` is
//! never used.
//!
//! A lock file in the git dir records the patch, so a run that was killed before it put the
//! changes back is recovered by the next one. Only one run at a time recovers, holding
//! `fnug-stash.recover` in the git dir locked.

use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use log::{error, warn};
use thiserror::Error;

const LOCK_NAME: &str = "fnug-stash.lock";
const LOCK_MAGIC: &[u8] = b"fnug-stash-lock 1";
const RECOVERY_LOCK_NAME: &str = "fnug-stash.recover";

#[derive(Debug, Error)]
pub enum StashError {
    #[error("failed to run git: {0}")]
    Spawn(#[source] io::Error),

    #[error("`git {args}` failed: {stderr}")]
    Git { args: String, stderr: String },

    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: io::Error,
    },

    #[error(
        "another `fnug check --stash` (pid {pid}) is running in this repository; if it isn't, remove {}",
        lock.display()
    )]
    Locked { pid: u32, lock: PathBuf },

    #[error("{} is not a lock fnug can read; remove it if no `fnug check --stash` is running", lock.display())]
    BadLock { lock: PathBuf },

    #[error("the index has unmerged paths; resolve them before checking with --stash")]
    Unmerged,

    #[error(
        "a stopped `fnug check --stash` saved unstaged changes in {}, but the files they change have changed since, so fnug can't tell what is missing. Compare them with the patch and put back what's missing, for example with `git apply --3way {}` in {}, then remove {}",
        patch.display(),
        patch.display(),
        toplevel.display(),
        lock.display()
    )]
    Stale {
        patch: PathBuf,
        toplevel: PathBuf,
        lock: PathBuf,
    },

    #[error(
        "couldn't put back unstaged changes ({reason}). They are saved in {}; restore them with `git apply {}` in {}, then remove {}",
        patch.display(),
        patch.display(),
        toplevel.display(),
        lock.display()
    )]
    Restore {
        reason: String,
        patch: PathBuf,
        toplevel: PathBuf,
        lock: PathBuf,
    },
}

/// What putting unstaged changes back did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RestoreNote {
    /// Commands changed files that also have unstaged changes, so their changes to those files
    /// were discarded to put the unstaged ones back.
    pub discarded_fixes: bool,
    /// The work tree didn't match the saved changes afterwards, so fnug kept them here.
    pub kept_patch: Option<PathBuf>,
}

/// Unstaged changes set aside by [`stash`]. [`restore`](Self::restore) puts them back; dropping
/// the guard does too, as when a panic unwinds.
pub struct StashGuard {
    git: Git,
    lock: PathBuf,
    state: LockState,
    /// The work tree may no longer hold the unstaged changes: checkout has started.
    checked_out: bool,
    recovered: Option<RestoreNote>,
    done: bool,
}

/// What the lock file records.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct LockState {
    pid: u32,
    patch: Option<PathBuf>,
    /// The tree `patch` was made against: the index, less intent-to-add entries.
    tree: Option<String>,
    /// Intent-to-add entries removed from the index, relative to the top of the work tree.
    intent_to_add: Vec<OsString>,
}

/// Set aside the unstaged changes to tracked files in the work tree containing `cwd`, so its
/// tracked files match the index: the one in `GIT_INDEX_FILE`, if set. A lock left by a stopped
/// run is recovered first.
///
/// # Errors
///
/// Returns `StashError::Locked` if another run holds the lock, `StashError::Unmerged` if the
/// index has conflicts, `StashError::Stale` if a stopped run's changes can't be put back, and
/// another error if git fails. Whatever was set aside by then is put back.
pub fn stash(cwd: &Path) -> Result<StashGuard, StashError> {
    let git = Git::locate(cwd)?;
    let lock = git.git_dir.join(LOCK_NAME);
    let recovered = acquire(&git, &lock)?;
    let mut guard = StashGuard {
        git,
        lock,
        state: LockState {
            pid: std::process::id(),
            ..LockState::default()
        },
        checked_out: false,
        recovered,
        done: false,
    };
    if let Err(e) = guard.set_aside() {
        guard.done = true;
        if let Err(undo) = guard.put_back() {
            error!("{undo}");
        }
        return Err(e);
    }
    Ok(guard)
}

/// Recover the unstaged changes of a stopped run in the work tree containing `cwd`, if one left
/// its lock, without setting anything aside. Returns what putting them back did, if it put any
/// back.
///
/// # Errors
///
/// As for [`stash`].
pub fn recover(cwd: &Path) -> Result<Option<RestoreNote>, StashError> {
    let git = Git::locate(cwd)?;
    let lock = git.git_dir.join(LOCK_NAME);
    if read_lock(&lock)?.is_none() {
        return Ok(None);
    }
    let _recovery = RecoveryLock::take(&git.git_dir)?;
    // Read again: another run may have recovered it while this one waited
    let Some(state) = read_lock(&lock)? else {
        return Ok(None);
    };
    if state.pid != std::process::id() && pid_alive(state.pid) {
        return Ok(None);
    }
    let note = recover_stale(&git, &lock, &state)?;
    remove_file(&lock);
    Ok(note)
}

/// The lock a stopped run left in the work tree containing `cwd`, if it still has unstaged
/// changes set aside, which stay so until [`stash`] or [`recover`] runs there.
#[must_use]
pub fn pending(cwd: &Path) -> Option<PathBuf> {
    let git = Git::locate(cwd).ok()?;
    let lock = git.git_dir.join(LOCK_NAME);
    let state = read_lock(&lock).ok()??;
    (!pid_alive(state.pid) && left_set_aside(&git, &state)).then_some(lock)
}

/// Whether a live `fnug check --stash` other than this process holds the lock in the work tree
/// containing `cwd`. Until it lets go, tracked files may hold the index's content instead of
/// the user's.
#[must_use]
pub fn active(cwd: &Path) -> bool {
    lock_path(cwd).is_some_and(|lock| held(&lock))
}

/// Where a `fnug check --stash` in the work tree containing `cwd` puts its lock, or `None`
/// outside a work tree. It runs git to find the git dir, which doesn't move, so keep the
/// result and check it with [`held`].
#[must_use]
pub fn lock_path(cwd: &Path) -> Option<PathBuf> {
    Git::locate(cwd).ok().map(|git| git.git_dir.join(LOCK_NAME))
}

/// Whether a live `fnug check --stash` other than this process holds `lock`, a path from
/// [`lock_path`].
#[must_use]
pub fn held(lock: &Path) -> bool {
    matches!(
        read_lock(lock),
        Ok(Some(state)) if state.pid != std::process::id() && pid_alive(state.pid)
    )
}

/// Write a lock for `pid` in `git_dir`, as a running `--stash` does. Returns its path.
#[cfg(test)]
pub(crate) fn write_test_lock(git_dir: &Path, pid: u32) -> PathBuf {
    let lock = git_dir.join(LOCK_NAME);
    let state = LockState {
        pid,
        ..LockState::default()
    };
    write_lock(&lock, &state).unwrap();
    lock
}

/// Whether a stopped run that recorded `state` left something set aside: intent-to-add entries
/// out of the index, or a patch the files it changes don't hold. When unsure, it did.
fn left_set_aside(git: &Git, state: &LockState) -> bool {
    if !state.intent_to_add.is_empty() {
        return true;
    }
    let Some(patch) = state.patch.as_ref().filter(|p| p.is_file()) else {
        // Without the file, the stopped run had already put the changes back
        return false;
    };
    let Some(tree) = state.tree.as_deref() else {
        return true;
    };
    SavedPatch::read(git, patch)
        .and_then(|saved| saved.compare(git, tree))
        .map_or(true, |files| files != WorkTree::Patched)
}

impl StashGuard {
    /// What putting back a stopped run's unstaged changes did, if this one put any back before
    /// it started.
    #[must_use]
    pub fn recovered(&self) -> Option<&RestoreNote> {
        self.recovered.as_ref()
    }

    /// Put the unstaged changes back. Run it once no command can still write to the work tree.
    ///
    /// Where commands changed files that also have unstaged changes, the commands' changes to
    /// those files are discarded.
    ///
    /// # Errors
    ///
    /// Returns `StashError::Restore` if the changes can't be put back; they stay in the patch
    /// the error names, and the lock stays so the next run tries again.
    pub fn restore(mut self) -> Result<RestoreNote, StashError> {
        self.done = true;
        self.put_back()
    }

    fn set_aside(&mut self) -> Result<(), StashError> {
        if !self.git.run(&["ls-files", "-u", "-z"])?.is_empty() {
            return Err(StashError::Unmerged);
        }

        // Checkout would empty them, since the index holds them as empty files
        let intent_to_add = self.git.intent_to_add()?;
        if !intent_to_add.is_empty() {
            self.state.intent_to_add = intent_to_add;
            write_lock(&self.lock, &self.state)?;
            self.git.pathspec_command(
                &["rm", "--cached", "--quiet", "--pathspec-from-file=-"],
                &self.state.intent_to_add,
            )?;
        }

        let tree = self.git.run(&["write-tree"])?;
        let tree = String::from_utf8_lossy(&tree).trim().to_string();
        let Some(diff) = self.git.unstaged_diff(&tree, &[])? else {
            return Ok(());
        };
        self.state.patch = Some(save_patch(&self.git.git_dir, &diff)?);
        self.state.tree = Some(tree);
        write_lock(&self.lock, &self.state)?;

        self.checked_out = true;
        self.git.checkout_index()
    }

    /// Put back what [`set_aside`](Self::set_aside) did, as far as it got.
    fn put_back(&mut self) -> Result<RestoreNote, StashError> {
        let mut note = RestoreNote::default();
        let reapplied = match self.state.patch.clone() {
            Some(patch) if self.checked_out => {
                let tree = self.state.tree.clone().unwrap_or_default();
                self.reapply(&patch, &tree)
                    .map_err(|reason| StashError::Restore {
                        reason,
                        patch,
                        toplevel: self.git.toplevel.clone(),
                        lock: self.lock.clone(),
                    })
            }
            Some(patch) => {
                remove_file(&patch);
                Ok(note)
            }
            None => Ok(note),
        };
        self.git.add_intent_to_add(&self.state.intent_to_add);
        // On failure the lock stays, so the next run tries again
        note = reapplied?;
        remove_file(&self.lock);
        Ok(note)
    }

    /// Apply the saved unstaged changes, made against `tree`, to the checked-out work tree, and
    /// delete them if the files they change hold exactly them afterwards.
    fn reapply(&self, patch: &Path, tree: &str) -> Result<RestoreNote, String> {
        let git = |e: StashError| e.to_string();
        let saved = SavedPatch::read(&self.git, patch).map_err(git)?;
        let mut note = RestoreNote::default();
        let restored = match saved.compare(&self.git, tree).map_err(git)? {
            // Checkout failed before it changed them
            WorkTree::Patched => true,
            state => {
                if state == WorkTree::Changed {
                    // Applied over a command's changes, a hunk can land where its context
                    // repeats, so only onto the content it was made against
                    self.git.restore_paths(tree, &saved.paths).map_err(git)?;
                    note.discarded_fixes = true;
                }
                if !self.git.apply(patch).map_err(git)? {
                    return Err("`git apply` failed".into());
                }
                saved.compare(&self.git, tree).map_err(git)? == WorkTree::Patched
            }
        };
        if restored {
            remove_file(patch);
        } else {
            note.kept_patch = Some(patch.to_path_buf());
        }
        Ok(note)
    }
}

/// A patch of unstaged changes that [`stash`] saved.
struct SavedPatch {
    bytes: Vec<u8>,
    /// The files it changes, relative to the top of the work tree.
    paths: Vec<OsString>,
}

/// Where the files a [`SavedPatch`] changes stand, compared with the tree it was made against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkTree {
    /// As in the tree: the changes are set aside.
    Clean,
    /// The tree plus exactly the patch's changes.
    Patched,
    /// Anything else, such as changes by a command or by hand.
    Changed,
}

impl SavedPatch {
    fn read(git: &Git, patch: &Path) -> Result<Self, StashError> {
        let bytes = fs::read(patch)
            .map_err(|e| io_error(format!("failed to read {}", patch.display()), e))?;
        let paths = git.patch_paths(patch)?;
        Ok(Self { bytes, paths })
    }

    /// Compare the diff of its files against `tree` byte for byte with the patch. Files it
    /// doesn't change don't count, so commands may have changed those.
    fn compare(&self, git: &Git, tree: &str) -> Result<WorkTree, StashError> {
        Ok(match git.unstaged_diff(tree, &self.paths)? {
            None => WorkTree::Clean,
            Some(diff) if diff == self.bytes => WorkTree::Patched,
            Some(_) => WorkTree::Changed,
        })
    }
}

impl Drop for StashGuard {
    fn drop(&mut self) {
        if !self.done {
            self.done = true;
            if let Err(e) = self.put_back() {
                error!("{e}");
            }
        }
    }
}

/// Take the lock at `lock`, recovering a stopped run's changes first. Returns what putting them
/// back did, if it put any back.
fn acquire(git: &Git, lock: &Path) -> Result<Option<RestoreNote>, StashError> {
    let ours = LockState {
        pid: std::process::id(),
        ..LockState::default()
    };
    let mut recovery = None;
    // A second attempt after the lock went away; another run can take it in between
    for _ in 0..2 {
        match OpenOptions::new().write(true).create_new(true).open(lock) {
            Ok(mut file) => {
                if let Err(e) = file
                    .write_all(&encode_lock(&ours))
                    .and_then(|()| file.sync_all())
                {
                    remove_file(lock);
                    return Err(io_error(format!("failed to write {}", lock.display()), e));
                }
                return Ok(None);
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                if recovery.is_none() {
                    recovery = Some(RecoveryLock::take(&git.git_dir)?);
                }
                let Some(state) = read_lock(lock)? else {
                    continue;
                };
                if state.pid != std::process::id() && pid_alive(state.pid) {
                    return Err(StashError::Locked {
                        pid: state.pid,
                        lock: lock.to_path_buf(),
                    });
                }
                let recovered = recover_stale(git, lock, &state)?;
                // Replaced rather than removed, so no other run can take it in between
                write_lock(lock, &ours)?;
                return Ok(recovered);
            }
            Err(e) => {
                return Err(io_error(format!("failed to create {}", lock.display()), e));
            }
        }
    }
    let pid = read_lock(lock)?.map_or(0, |s| s.pid);
    Err(StashError::Locked {
        pid,
        lock: lock.to_path_buf(),
    })
}

/// An exclusive `flock` on `fnug-stash.recover` in the git dir, held while a stopped run's lock
/// is recovered, so two runs never put the same changes back. The kernel releases it when fnug
/// exits, however it exits.
struct RecoveryLock {
    _file: File,
}

impl RecoveryLock {
    /// Wait for the lock and take it.
    fn take(git_dir: &Path) -> Result<Self, StashError> {
        let path = git_dir.join(RECOVERY_LOCK_NAME);
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(|e| io_error(format!("failed to open {}", path.display()), e))?;
        // SAFETY: flock on a file descriptor that `file` owns.
        while unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
            let e = io::Error::last_os_error();
            if e.kind() != io::ErrorKind::Interrupted {
                return Err(io_error(format!("failed to lock {}", path.display()), e));
            }
        }
        Ok(Self { _file: file })
    }
}

/// Put back the changes a stopped run recorded in `state`, leaving its lock to the caller.
/// Returns what putting them back did, if they were set aside.
///
/// Only when the files they change still match the index they were set aside from; when those
/// files hold something else, it refuses with `StashError::Stale`.
fn recover_stale(
    git: &Git,
    lock: &Path,
    state: &LockState,
) -> Result<Option<RestoreNote>, StashError> {
    let mut note = None;
    // Without the file, the stopped run had already put the changes back
    if let Some(patch) = state.patch.as_ref().filter(|p| p.is_file()) {
        let refuse = || StashError::Stale {
            patch: patch.clone(),
            toplevel: git.toplevel.clone(),
            lock: lock.to_path_buf(),
        };
        let tree = state.tree.as_deref().ok_or_else(refuse)?;
        let saved = SavedPatch::read(git, patch)?;
        let restored = match saved.compare(git, tree)? {
            // Stopped before its checkout, or after putting the changes back
            WorkTree::Patched => true,
            WorkTree::Clean => {
                if !git.apply(patch)? {
                    return Err(refuse());
                }
                let restored = saved.compare(git, tree)? == WorkTree::Patched;
                note = Some(RestoreNote {
                    kept_patch: (!restored).then(|| patch.clone()),
                    ..RestoreNote::default()
                });
                restored
            }
            WorkTree::Changed => return Err(refuse()),
        };
        if restored {
            remove_file(patch);
        }
    }
    git.add_intent_to_add(&state.intent_to_add);
    Ok(note)
}

/// Runs git on one work tree, named explicitly, from its top and in its own process group, so
/// the terminal's Ctrl+C can't stop it halfway through rewriting the work tree.
struct Git {
    toplevel: PathBuf,
    git_dir: PathBuf,
}

impl Git {
    /// The work tree containing `cwd`.
    fn locate(cwd: &Path) -> Result<Self, StashError> {
        let found = locate_from(cwd, false)?;
        // git hands hooks an absolute GIT_DIR when `.git` is a file, as in a linked worktree,
        // and without GIT_WORK_TREE git takes `cwd`, such as the config's directory, as the
        // top. Found without it, the same git dir tells the real top.
        if std::env::var_os("GIT_DIR").is_some_and(|d| !d.is_empty())
            && std::env::var_os("GIT_WORK_TREE").is_none()
        {
            let rediscovered = locate_from(cwd, true)
                .ok()
                .filter(|git| same_file(&git.git_dir, &found.git_dir));
            if let Some(rediscovered) = rediscovered {
                return Ok(rediscovered);
            }
        }
        Ok(found)
    }

    fn command<S: AsRef<OsStr>>(&self, args: &[S]) -> Command {
        let mut command = Command::new("git");
        command
            .args(args)
            .current_dir(&self.toplevel)
            .env("GIT_DIR", &self.git_dir)
            .env("GIT_WORK_TREE", &self.toplevel)
            .stdin(Stdio::null())
            .process_group(0);
        command
    }

    fn output<S: AsRef<OsStr>>(&self, args: &[S]) -> Result<Output, StashError> {
        self.command(args).output().map_err(StashError::Spawn)
    }

    /// Run git and return its stdout, failing unless it exits 0.
    fn run<S: AsRef<OsStr>>(&self, args: &[S]) -> Result<Vec<u8>, StashError> {
        let output = self.output(args)?;
        if output.status.success() {
            Ok(output.stdout)
        } else {
            Err(git_error(args, &output))
        }
    }

    /// Run git with `paths` as NUL-separated literal pathspecs on stdin.
    fn pathspec_command(&self, args: &[&str], paths: &[OsString]) -> Result<(), StashError> {
        let mut command = self.command(args);
        command
            .arg("--pathspec-file-nul")
            .env("GIT_LITERAL_PATHSPECS", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let mut child = command.spawn().map_err(StashError::Spawn)?;
        let mut input = Vec::new();
        for path in paths {
            input.extend_from_slice(path.as_bytes());
            input.push(0);
        }
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(&input)
                .map_err(|e| io_error("failed to pass paths to git".into(), e))?;
        }
        let output = child.wait_with_output().map_err(StashError::Spawn)?;
        if output.status.success() {
            Ok(())
        } else {
            Err(git_error(args, &output))
        }
    }

    /// Intent-to-add entries in the index (`git add -N`), which have no content yet.
    fn intent_to_add(&self) -> Result<Vec<OsString>, StashError> {
        let out = self.run(&[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=no",
            "--no-renames",
            "--ignore-submodules",
        ])?;
        Ok(out
            .split(|&b| b == 0)
            .filter_map(|entry| entry.strip_prefix(b" A "))
            .map(|path| OsString::from_vec(path.to_vec()))
            .collect())
    }

    /// Mark `paths` intent-to-add again, where they still exist. Failures are only logged: the
    /// files themselves are intact.
    fn add_intent_to_add(&self, paths: &[OsString]) {
        let existing: Vec<OsString> = paths
            .iter()
            .filter(|p| self.toplevel.join(p).symlink_metadata().is_ok())
            .cloned()
            .collect();
        if existing.is_empty() {
            return;
        }
        if let Err(e) = self.pathspec_command(&["add", "-N", "--pathspec-from-file=-"], &existing) {
            warn!("Failed to mark files as intent-to-add again (`git add -N`): {e}");
        }
    }

    /// The unstaged changes to tracked files, as a binary patch against `tree`, or `None` if
    /// there are none. Only those to `paths`, unless it is empty.
    fn unstaged_diff(&self, tree: &str, paths: &[OsString]) -> Result<Option<Vec<u8>>, StashError> {
        let flags = [
            "diff-index",
            "--ignore-submodules",
            "--binary",
            "--exit-code",
            "--no-color",
            "--no-ext-diff",
            tree,
            "--",
        ]
        .map(OsStr::new);
        let mut diff = Vec::new();
        // Each batch's diff follows the previous one's, as in a single diff
        for batch in command_line_batches(paths) {
            let mut args = flags.to_vec();
            args.extend(batch.iter().map(OsString::as_os_str));
            let output = self
                .command(&args)
                .env("GIT_LITERAL_PATHSPECS", "1")
                .output()
                .map_err(StashError::Spawn)?;
            match output.status.code() {
                // Empty with exit 1 happens for line-ending-only differences
                Some(0 | 1) => diff.extend_from_slice(&output.stdout),
                _ => return Err(git_error(&flags, &output)),
            }
        }
        Ok((!diff.is_empty()).then_some(diff))
    }

    /// The files `patch` changes, relative to the top of the work tree.
    fn patch_paths(&self, patch: &Path) -> Result<Vec<OsString>, StashError> {
        let out = self.run(&[
            OsStr::new("apply"),
            OsStr::new("--numstat"),
            OsStr::new("-z"),
            patch.as_os_str(),
        ])?;
        // `<added>\t<deleted>\t<path>` per file
        let paths: Vec<OsString> = out
            .split(|&b| b == 0)
            .filter_map(|record| record.splitn(3, |&b| b == b'\t').nth(2))
            .filter(|path| !path.is_empty())
            .map(|path| OsString::from_vec(path.to_vec()))
            .collect();
        if paths.is_empty() {
            return Err(StashError::Git {
                args: format!("apply --numstat {}", patch.display()),
                stderr: "the patch changes no files".into(),
            });
        }
        Ok(paths)
    }

    /// Check the tracked files out from the index, without running hooks or touching
    /// submodules.
    fn checkout_index(&self) -> Result<(), StashError> {
        self.run(&[
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "submodule.recurse=0",
            "checkout",
            "--",
            ".",
        ])
        .map(drop)
    }

    /// Write `paths` to the work tree as `tree` has them, leaving the index alone.
    fn restore_paths(&self, tree: &str, paths: &[OsString]) -> Result<(), StashError> {
        let source = format!("--source={tree}");
        self.pathspec_command(
            &[
                "-c",
                "submodule.recurse=0",
                "restore",
                &source,
                "--worktree",
                "--pathspec-from-file=-",
            ],
            paths,
        )
    }

    /// Apply `patch` to the work tree; returns whether it applied.
    fn apply(&self, patch: &Path) -> Result<bool, StashError> {
        // Retried without line-ending conversion, which can make a patch not apply
        for config in [&[][..], &["-c", "core.autocrlf=false"]] {
            let mut args: Vec<&OsStr> = config.iter().map(OsStr::new).collect();
            args.extend(["apply", "--whitespace=nowarn"].map(OsStr::new));
            args.push(patch.as_os_str());
            if self.output(&args)?.status.success() {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

/// The work tree git finds from `cwd` with the environment fnug inherited, or with `GIT_DIR`
/// removed when `ignore_git_dir` is set.
fn locate_from(cwd: &Path, ignore_git_dir: bool) -> Result<Git, StashError> {
    const ARGS: [&str; 3] = ["rev-parse", "--show-toplevel", "--absolute-git-dir"];
    let mut command = Command::new("git");
    command
        .args(ARGS)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .process_group(0);
    if ignore_git_dir {
        command.env_remove("GIT_DIR");
    }
    let output = command.output().map_err(StashError::Spawn)?;
    if !output.status.success() {
        return Err(git_error(&ARGS, &output));
    }
    let mut lines = output.stdout.split(|&b| b == b'\n');
    let (Some(toplevel), Some(git_dir)) = (lines.next(), lines.next()) else {
        return Err(StashError::Git {
            args: ARGS.join(" "),
            stderr: "unexpected output".into(),
        });
    };
    Ok(Git {
        toplevel: PathBuf::from(OsStr::from_bytes(toplevel)),
        git_dir: PathBuf::from(OsStr::from_bytes(git_dir)),
    })
}

/// Whether `a` and `b` name the same existing file or directory.
fn same_file(a: &Path, b: &Path) -> bool {
    matches!((a.canonicalize(), b.canonicalize()), (Ok(a), Ok(b)) if a == b)
}

/// `paths` split into runs short enough for one command line, in order; a single empty run if
/// there are none.
fn command_line_batches(paths: &[OsString]) -> Vec<&[OsString]> {
    const MAX_BYTES: usize = 32 * 1024;
    let mut batches = Vec::new();
    let (mut start, mut bytes) = (0, 0);
    for (i, path) in paths.iter().enumerate() {
        let len = path.len() + 1;
        if i > start && bytes + len > MAX_BYTES {
            batches.push(&paths[start..i]);
            (start, bytes) = (i, 0);
        }
        bytes += len;
    }
    batches.push(&paths[start..]);
    batches
}

fn git_error<S: AsRef<OsStr>>(args: &[S], output: &Output) -> StashError {
    StashError::Git {
        args: args
            .iter()
            .map(|a| a.as_ref().to_string_lossy())
            .collect::<Vec<_>>()
            .join(" "),
        stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
    }
}

fn io_error(context: String, source: io::Error) -> StashError {
    StashError::Io { context, source }
}

/// Write `diff` to a new file under `git_dir` and flush it to disk before the work tree loses
/// the changes. Returns its path.
fn save_patch(git_dir: &Path, diff: &[u8]) -> Result<PathBuf, StashError> {
    let dir = git_dir.join("fnug");
    fs::create_dir_all(&dir)
        .map_err(|e| io_error(format!("failed to create {}", dir.display()), e))?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let path = dir.join(format!("stash-{stamp}-{}.patch", std::process::id()));
    let write = || -> io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        file.write_all(diff)?;
        file.sync_all()?;
        File::open(&dir)?.sync_all()
    };
    write().map_err(|e| io_error(format!("failed to save {}", path.display()), e))?;
    Ok(path)
}

/// Replace the lock's content atomically, so a crash leaves either the old or the new state.
fn write_lock(lock: &Path, state: &LockState) -> Result<(), StashError> {
    let tmp = lock.with_extension("lock.tmp");
    let write = || -> io::Result<()> {
        let mut file = File::create(&tmp)?;
        file.write_all(&encode_lock(state))?;
        file.sync_all()?;
        fs::rename(&tmp, lock)?;
        if let Some(dir) = lock.parent() {
            File::open(dir)?.sync_all()?;
        }
        Ok(())
    };
    write().map_err(|e| io_error(format!("failed to write {}", lock.display()), e))
}

/// NUL-separated fields, since paths may hold any byte but NUL.
fn encode_lock(state: &LockState) -> Vec<u8> {
    let mut fields: Vec<&[u8]> = vec![LOCK_MAGIC];
    let pid = state.pid.to_string();
    fields.extend([b"pid".as_slice(), pid.as_bytes()]);
    if let Some(patch) = &state.patch {
        fields.extend([b"patch".as_slice(), patch.as_os_str().as_bytes()]);
    }
    if let Some(tree) = &state.tree {
        fields.extend([b"tree".as_slice(), tree.as_bytes()]);
    }
    for path in &state.intent_to_add {
        fields.extend([b"intent-to-add".as_slice(), path.as_bytes()]);
    }
    let mut out = fields.join(&0u8);
    out.push(0);
    out
}

fn decode_lock(bytes: &[u8]) -> Option<LockState> {
    let mut fields = bytes.strip_suffix(&[0])?.split(|&b| b == 0);
    if fields.next()? != LOCK_MAGIC {
        return None;
    }
    let mut state = LockState::default();
    while let Some(key) = fields.next() {
        let value = fields.next()?;
        match key {
            b"pid" => state.pid = std::str::from_utf8(value).ok()?.parse().ok()?,
            b"patch" => state.patch = Some(PathBuf::from(OsStr::from_bytes(value))),
            b"tree" => state.tree = Some(std::str::from_utf8(value).ok()?.to_string()),
            b"intent-to-add" => state.intent_to_add.push(OsString::from_vec(value.to_vec())),
            _ => {}
        }
    }
    (state.pid != 0).then_some(state)
}

/// The lock's state, or `None` if there is no lock.
fn read_lock(lock: &Path) -> Result<Option<LockState>, StashError> {
    match fs::read(lock) {
        Ok(bytes) => decode_lock(&bytes)
            .map(Some)
            .ok_or_else(|| StashError::BadLock {
                lock: lock.to_path_buf(),
            }),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io_error(format!("failed to read {}", lock.display()), e)),
    }
}

fn pid_alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // SAFETY: signal 0 only checks that the process exists and may be signalled.
    let found = unsafe { libc::kill(pid, 0) } == 0;
    found || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

fn remove_file(path: &Path) {
    if let Err(e) = fs::remove_file(path)
        && e.kind() != io::ErrorKind::NotFound
    {
        warn!("Failed to remove {}: {e}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_round_trips() {
        let state = LockState {
            pid: 42,
            patch: Some(PathBuf::from("/repo/.git/fnug/stash-1-42.patch")),
            tree: Some("4b825dc642cb6eb9a060e54bf8d69288fbee4904".into()),
            intent_to_add: vec![
                OsString::from("new file.txt"),
                OsString::from_vec(b"\xffx".to_vec()),
            ],
        };
        assert_eq!(decode_lock(&encode_lock(&state)), Some(state));
        let bare = LockState {
            pid: 7,
            ..LockState::default()
        };
        assert_eq!(decode_lock(&encode_lock(&bare)), Some(bare));
    }

    #[test]
    fn active_only_while_another_live_run_holds_the_lock() {
        // git would look there instead, as in a hook of a linked worktree
        if std::env::var_os("GIT_DIR").is_some() {
            eprintln!("skipping: GIT_DIR is set");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        git2::Repository::init(dir.path()).unwrap();
        let git_dir = dir.path().join(".git");
        assert!(!active(dir.path()));

        let mut other = Command::new("sleep").arg("60").spawn().unwrap();
        let lock = write_test_lock(&git_dir, other.id());
        assert!(active(dir.path()));
        write_test_lock(&git_dir, std::process::id());
        assert!(!active(dir.path()), "counted its own lock");

        write_test_lock(&git_dir, other.id());
        other.kill().unwrap();
        other.wait().unwrap();
        assert!(!active(dir.path()), "counted a stopped run's lock");
        remove_file(&lock);
    }

    #[test]
    fn garbage_lock_is_rejected() {
        assert_eq!(decode_lock(b""), None);
        assert_eq!(decode_lock(b"pid\x0012\x00"), None);
        assert_eq!(decode_lock(b"fnug-stash-lock 1\x00pid\x00x\x00"), None);
    }

    #[test]
    fn batches_split_long_path_lists_in_order() {
        assert_eq!(command_line_batches(&[]), [&[] as &[OsString]]);
        let paths: Vec<OsString> = (0..3000)
            .map(|i| OsString::from(format!("{i:0>20}")))
            .collect();
        let batches = command_line_batches(&paths);
        assert!(batches.len() > 1);
        assert!(batches.iter().all(|b| !b.is_empty()));
        assert_eq!(batches.concat(), paths);
    }
}
