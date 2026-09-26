//! Failing commands that change tracked files, such as a formatter that rewrites what it checks.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use git2::{ObjectType, Oid, Repository, RepositoryOpenFlags, StatusOptions};
use log::warn;

use crate::runner::{ExecHook, Failure, Outcome, PlannedCommand};

/// An [`ExecHook`] that fails a passing command that changed a tracked file in the git work tree
/// of its working directory: it modified or deleted a clean file, changed the content of a
/// modified one, or reverted one. Untracked and ignored files don't count, and outside a work
/// tree nothing is checked.
///
/// A change made while the command ran counts even when another command running alongside made
/// it.
pub struct ModificationGuard {
    fallback_cwd: PathBuf,
}

impl ModificationGuard {
    /// Commands without their own cwd run in `cwd`. Changed files under it are reported relative
    /// to it, others by absolute path.
    #[must_use]
    pub fn new(cwd: &Path) -> Self {
        Self {
            fallback_cwd: cwd.to_path_buf(),
        }
    }

    fn relative(&self, path: PathBuf) -> PathBuf {
        match path.strip_prefix(&self.fallback_cwd) {
            Ok(rel) if !rel.as_os_str().is_empty() => rel.to_path_buf(),
            _ => path,
        }
    }
}

impl ExecHook for ModificationGuard {
    type Token = Option<Snapshot>;

    fn before(&self, cmd: &PlannedCommand) -> Option<Snapshot> {
        Snapshot::take(cmd.command.effective_cwd(&self.fallback_cwd))
    }

    fn after(&self, _: &PlannedCommand, before: Option<Snapshot>, outcome: &mut Outcome) {
        let Some(before) = before else {
            return;
        };
        if *outcome != Outcome::Passed {
            return;
        }
        let Some(after) = Snapshot::take(&before.workdir) else {
            return;
        };
        let mut changed: Vec<PathBuf> = before
            .files
            .iter()
            .filter(|&(path, hash)| after.files.get(path) != Some(hash))
            .map(|(path, _)| path)
            .chain(
                after
                    .files
                    .keys()
                    .filter(|path| !before.files.contains_key(*path)),
            )
            .cloned()
            .collect();
        if changed.is_empty() {
            return;
        }
        changed.sort();
        changed.dedup();
        *outcome = Outcome::Failed(Failure::Modified(
            changed.into_iter().map(|p| self.relative(p)).collect(),
        ));
    }
}

/// The tracked files of a work tree that differ from `HEAD` or the index, with a hash of each
/// one's content, or `None` for a missing file.
pub struct Snapshot {
    workdir: PathBuf,
    files: HashMap<PathBuf, Option<Oid>>,
}

impl Snapshot {
    /// Snapshot the work tree containing `dir`, or `None` if there is none or it can't be read.
    fn take(dir: &Path) -> Option<Self> {
        let start = dir.ancestors().find(|p| p.is_dir())?;
        let repo =
            Repository::open_ext(start, RepositoryOpenFlags::CROSS_FS, &[] as &[&Path]).ok()?;
        let workdir = repo.workdir()?;
        let workdir = workdir
            .canonicalize()
            .unwrap_or_else(|_| workdir.to_path_buf());
        let mut opts = StatusOptions::new();
        opts.include_untracked(false)
            .include_ignored(false)
            .exclude_submodules(true);
        let statuses = match repo.statuses(Some(&mut opts)) {
            Ok(statuses) => statuses,
            Err(e) => {
                warn!(
                    "Can't tell whether commands change files in {}: {}",
                    workdir.display(),
                    e.message()
                );
                return None;
            }
        };
        let files = statuses
            .iter()
            .map(|entry| {
                let path = workdir.join(OsStr::from_bytes(entry.path_bytes()));
                let hash = content_hash(&path);
                (path, hash)
            })
            .collect();
        Some(Self { workdir, files })
    }
}

/// The blob id git would give the file at `path`: its content, or a symlink's target.
fn content_hash(path: &Path) -> Option<Oid> {
    let meta = path.symlink_metadata().ok()?;
    if meta.is_symlink() {
        let target = std::fs::read_link(path).ok()?;
        Oid::hash_object(ObjectType::Blob, target.as_os_str().as_bytes()).ok()
    } else {
        Oid::hash_file(ObjectType::Blob, path).ok()
    }
}
