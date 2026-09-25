use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use git2::{Repository, RepositoryOpenFlags, StatusOptions};
use log::debug;

use crate::commands::command::Command;
use crate::selectors::matching::command_matches;
use crate::selectors::{SelectOptions, SelectionIssue};

/// What git selection found for the commands passed to [`select`].
pub(super) struct GitSelection {
    /// Per command, in input order: the existing changed files that select it, or `None` if
    /// no change does.
    pub matches: Vec<Option<Vec<PathBuf>>>,
    pub changed_files: usize,
    pub issues: Vec<SelectionIssue>,
}

/// A repo containing at least one `auto.path`, keyed by its canonical work tree.
struct RepoEntry {
    workdir: PathBuf,
    repo: Repository,
    /// The auto paths in this repo, relative to `workdir`; `None` scans the whole work tree.
    pathspecs: Option<Vec<PathBuf>>,
}

impl RepoEntry {
    /// Include `path` in the scan. The work tree root, or a path that isn't under it, lifts
    /// the limit altogether.
    fn add_path(&mut self, path: &Path) {
        let rel = path
            .strip_prefix(&self.workdir)
            .ok()
            .filter(|rel| !rel.as_os_str().is_empty());
        match (rel, &mut self.pathspecs) {
            (Some(rel), Some(specs)) => {
                if !specs.iter().any(|spec| spec == rel) {
                    specs.push(rel.to_path_buf());
                }
            }
            (None, specs) => *specs = None,
            (Some(_), None) => {}
        }
    }
}

/// A changed path in a scanned repo.
struct Change {
    path: PathBuf,
    /// It still exists and isn't a directory, such as an untracked nested repo.
    is_file: bool,
}

impl Change {
    /// A change git reports as `rel`, a path relative to `workdir` in raw bytes: git doesn't
    /// require names to be UTF-8.
    fn new(workdir: &Path, rel: &[u8]) -> Self {
        let path = workdir.join(OsStr::from_bytes(rel));
        Change {
            is_file: path.symlink_metadata().is_ok_and(|meta| !meta.is_dir()),
            path,
        }
    }
}

/// Open the repo whose work tree contains `path`, with its canonical work tree root. `path`
/// may be missing, such as a deleted directory: discovery starts from its nearest existing
/// ancestor. Fails if no repo contains `path` or the repo is bare.
fn discover(path: &Path) -> Result<RepoEntry, String> {
    let start = path.ancestors().find(|p| p.is_dir()).unwrap_or(path);
    // Not `Repository::discover`: it reopens the gitdir, so a `.git` file without
    // `core.worktree` (as `git init --separate-git-dir` writes) gets the wrong work tree.
    let repo = Repository::open_ext(start, RepositoryOpenFlags::CROSS_FS, &[] as &[&Path])
        .map_err(|e| e.message().to_string())?;
    let Some(workdir) = repo.workdir() else {
        return Err(format!(
            "bare repository at {} has no working tree",
            repo.path().display()
        ));
    };
    let workdir = workdir
        .canonicalize()
        .unwrap_or_else(|_| workdir.to_path_buf());
    debug!("Discovered git repo at {}", workdir.display());
    Ok(RepoEntry {
        workdir,
        repo,
        pathspecs: Some(Vec::new()),
    })
}

/// Collect the non-ignored changed files under a repo's pathspecs.
fn scan(entry: &RepoEntry) -> Result<Vec<Change>, git2::Error> {
    let mut opts = StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .include_ignored(false);
    if let Some(specs) = &entry.pathspecs {
        // Literal prefixes: as globs, names with `[` or `*` would misfire, and disjoint specs
        // wouldn't narrow the walk.
        opts.disable_pathspec_match(true);
        for spec in specs {
            opts.pathspec(spec.as_path());
        }
    }
    let changes: Vec<Change> = entry
        .repo
        .statuses(Some(&mut opts))?
        .iter()
        .map(|status| Change::new(&entry.workdir, status.path_bytes()))
        .collect();
    debug!(
        "Found {} changed files in {}",
        changes.len(),
        entry.workdir.display()
    );
    Ok(changes)
}

/// Discover the repo of every `auto.path` of the git-enabled `commands`, once per path.
/// Returns the distinct repos and the index of each path's repo; paths outside a work tree
/// become a `NotInRepo` issue naming the commands that use them.
fn discover_repos<'a>(
    commands: &[&'a Command],
    issues: &mut Vec<SelectionIssue>,
) -> (Vec<RepoEntry>, HashMap<&'a Path, usize>) {
    let mut paths: Vec<(&Path, Vec<String>)> = Vec::new();
    for cmd in commands.iter().filter(|c| c.auto.git == Some(true)) {
        for path in cmd.auto.paths() {
            match paths.iter_mut().find(|(p, _)| p == path) {
                Some((_, ids)) if !ids.contains(&cmd.id) => ids.push(cmd.id.clone()),
                Some(_) => {}
                None => paths.push((path, vec![cmd.id.clone()])),
            }
        }
    }

    let mut repos: Vec<RepoEntry> = Vec::new();
    let mut path_repo = HashMap::new();
    for (path, command_ids) in paths {
        match discover(path) {
            Ok(entry) => {
                let index = repos
                    .iter()
                    .position(|r| r.workdir == entry.workdir)
                    .unwrap_or_else(|| {
                        repos.push(entry);
                        repos.len() - 1
                    });
                repos[index].add_path(path);
                path_repo.insert(path, index);
            }
            Err(message) => issues.push(SelectionIssue::NotInRepo {
                path: path.to_path_buf(),
                command_ids,
                message,
            }),
        }
    }
    (repos, path_repo)
}

/// Scan `repos` in parallel. A repo that fails becomes a `ScanFailed` issue and `None`.
fn scan_repos(repos: Vec<RepoEntry>, issues: &mut Vec<SelectionIssue>) -> Vec<Option<Vec<Change>>> {
    let workdirs: Vec<PathBuf> = repos.iter().map(|r| r.workdir.clone()).collect();
    let results: Vec<_> = std::thread::scope(|s| {
        let handles: Vec<_> = repos
            .into_iter()
            .map(|entry| s.spawn(move || scan(&entry)))
            .collect();
        handles
            .into_iter()
            .map(std::thread::ScopedJoinHandle::join)
            .collect()
    });
    results
        .into_iter()
        .zip(workdirs)
        .map(|(result, repo)| {
            let message = match result {
                Ok(Ok(changes)) => return Some(changes),
                Ok(Err(e)) => e.message().to_string(),
                Err(_) => "the scan panicked".to_string(),
            };
            issues.push(SelectionIssue::ScanFailed { repo, message });
            None
        })
        .collect()
}

/// The existing changed files that select `cmd`, or `None` if no change does.
fn command_files(
    cmd: &Command,
    path_repo: &HashMap<&Path, usize>,
    scanned: &[Option<Vec<Change>>],
) -> Option<Vec<PathBuf>> {
    let mut selected = false;
    let mut files = Vec::new();
    for path in cmd.auto.paths() {
        let Some(Some(changes)) = path_repo.get(path.as_path()).map(|&i| &scanned[i]) else {
            continue;
        };
        for change in changes
            .iter()
            .filter(|c| command_matches(cmd, path, &c.path))
        {
            selected = true;
            if change.is_file {
                files.push(change.path.clone());
            }
        }
    }
    if !selected {
        return None;
    }
    debug!("Git-selected command '{}'", cmd.name);
    files.sort();
    files.dedup();
    Some(files)
}

/// Scan every repo that holds an `auto.path` of a git-enabled command, and match the changes.
pub(super) fn select(commands: &[&Command], _opts: &SelectOptions) -> GitSelection {
    let mut issues = Vec::new();
    let (repos, path_repo) = discover_repos(commands, &mut issues);
    let scanned = scan_repos(repos, &mut issues);
    let changed_files = scanned
        .iter()
        .flatten()
        .flatten()
        .map(|c| c.path.as_path())
        .collect::<HashSet<_>>()
        .len();
    let matches = commands
        .iter()
        .map(|cmd| {
            (cmd.auto.git == Some(true))
                .then(|| command_files(cmd, &path_repo, &scanned))
                .flatten()
        })
        .collect();
    GitSelection {
        matches,
        changed_files,
        issues,
    }
}
