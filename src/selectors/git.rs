use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use git2::{Diff, DiffOptions, ErrorCode, Index, Repository, RepositoryOpenFlags, StatusOptions};
use log::debug;

use crate::commands::command::Command;
use crate::selectors::matching::command_matches;
use crate::selectors::{GitScope, IndexOverride, SelectOptions, SelectionIssue};

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
    /// Git gets them as literal paths: as globs, names with `[` or `*` would misfire, and
    /// disjoint specs wouldn't narrow the walk.
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

/// The temporary index named by `index_file` (`GIT_INDEX_FILE`), for the repo at `git_dir`
/// (`GIT_DIR`) or else the one containing `cwd`. A relative `git_dir` resolves against `cwd`,
/// and a relative `index_file` against the repo's work tree.
pub(super) fn index_override(
    git_dir: Option<&OsStr>,
    index_file: Option<&OsStr>,
    cwd: &Path,
) -> Option<IndexOverride> {
    let index_file = index_file.filter(|file| !file.is_empty())?;
    let named = git_dir
        .filter(|dir| !dir.is_empty())
        .and_then(|dir| Repository::open(cwd.join(dir)).ok());
    let repo = match named {
        Some(repo) => repo,
        None => Repository::open_ext(cwd, RepositoryOpenFlags::CROSS_FS, &[] as &[&Path]).ok()?,
    };
    let base = repo.workdir().unwrap_or_else(|| repo.path());
    Some(IndexOverride {
        git_dir: repo.path().canonicalize().ok()?,
        index_file: base.join(index_file),
    })
}

/// The index file to read for `entry` under the staged scope: the override's, if it belongs to
/// this repo, or else `None` for the repo's own.
fn index_file_for(entry: &RepoEntry, opts: &SelectOptions) -> Option<PathBuf> {
    let over = opts.index_override.as_ref()?;
    let own = entry.repo.path().canonicalize().ok()?;
    (over.git_dir.canonicalize().ok()? == own).then(|| over.index_file.clone())
}

/// Collect the changed files under a repo's pathspecs within `scope`, reading `index_file`
/// instead of the repo's index if given.
fn scan(
    entry: &RepoEntry,
    scope: &GitScope,
    index_file: Option<&Path>,
) -> Result<Vec<Change>, git2::Error> {
    let changes = match scope {
        GitScope::WorkingTree => scan_working_tree(entry)?,
        GitScope::Staged => scan_staged(entry, index_file)?,
    };
    debug!(
        "Found {} changed files in {}",
        changes.len(),
        entry.workdir.display()
    );
    Ok(changes)
}

fn scan_working_tree(entry: &RepoEntry) -> Result<Vec<Change>, git2::Error> {
    let mut opts = StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .include_ignored(false);
    if let Some(specs) = &entry.pathspecs {
        opts.disable_pathspec_match(true);
        for spec in specs {
            opts.pathspec(spec.as_path());
        }
    }
    Ok(entry
        .repo
        .statuses(Some(&mut opts))?
        .iter()
        .map(|status| Change::new(&entry.workdir, status.path_bytes()))
        .collect())
}

fn diff_options(entry: &RepoEntry) -> DiffOptions {
    let mut opts = DiffOptions::new();
    if let Some(specs) = &entry.pathspecs {
        opts.disable_pathspec_match(true);
        for spec in specs {
            opts.pathspec(spec.as_path());
        }
    }
    opts
}

fn scan_staged(entry: &RepoEntry, index_file: Option<&Path>) -> Result<Vec<Change>, git2::Error> {
    let head_tree = match entry.repo.head() {
        Ok(head) => Some(head.peel_to_tree()?),
        Err(e) if matches!(e.code(), ErrorCode::UnbornBranch | ErrorCode::NotFound) => None,
        Err(e) => return Err(e),
    };
    let index = match index_file {
        // libgit2 opens a missing file as an empty index, where every file looks deleted.
        Some(file) if !file.is_file() => {
            return Err(git2::Error::from_str(&format!(
                "index file {} does not exist",
                file.display()
            )));
        }
        Some(file) => Index::open(file)?,
        None => entry.repo.index()?,
    };
    let diff = entry.repo.diff_tree_to_index(
        head_tree.as_ref(),
        Some(&index),
        Some(&mut diff_options(entry)),
    )?;
    Ok(diff_changes(entry, &diff))
}

/// Both sides of every delta, so renames and deletions count too.
fn diff_changes(entry: &RepoEntry, diff: &Diff) -> Vec<Change> {
    let mut paths: Vec<&[u8]> = diff
        .deltas()
        .flat_map(|delta| [delta.old_file().path_bytes(), delta.new_file().path_bytes()])
        .flatten()
        .collect();
    paths.sort_unstable();
    paths.dedup();
    paths
        .into_iter()
        .map(|path| Change::new(&entry.workdir, path))
        .collect()
}

/// Under the staged scope, the repo whose index git commits must exist: the override's, or
/// else the one containing `current_dir`. Returns the fatal issue if it doesn't.
fn check_current_repo(opts: &SelectOptions, current_dir: Option<&Path>) -> Option<SelectionIssue> {
    if opts.scope != GitScope::Staged || opts.index_override.is_some() {
        return None;
    }
    let (path, message) = match current_dir {
        Some(dir) => match discover(dir) {
            Ok(_) => return None,
            Err(message) => (dir.to_path_buf(), message),
        },
        None => (
            PathBuf::new(),
            "the working directory is unknown".to_string(),
        ),
    };
    Some(SelectionIssue::NotInRepo {
        path,
        command_ids: Vec::new(),
        message,
        fatal: true,
    })
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
                fatal: false,
            }),
        }
    }
    (repos, path_repo)
}

/// Scan `repos` in parallel. A repo that fails becomes a `ScanFailed` issue and `None`.
fn scan_repos(
    repos: Vec<RepoEntry>,
    opts: &SelectOptions,
    issues: &mut Vec<SelectionIssue>,
) -> Vec<Option<Vec<Change>>> {
    let workdirs: Vec<PathBuf> = repos.iter().map(|r| r.workdir.clone()).collect();
    let results: Vec<_> = std::thread::scope(|s| {
        let handles: Vec<_> = repos
            .into_iter()
            .map(|entry| {
                let index_file = match opts.scope {
                    GitScope::Staged => index_file_for(&entry, opts),
                    GitScope::WorkingTree => None,
                };
                s.spawn(move || scan(&entry, &opts.scope, index_file.as_deref()))
            })
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
/// `current_dir` is the process working directory.
pub(super) fn select(
    commands: &[&Command],
    opts: &SelectOptions,
    current_dir: Option<&Path>,
) -> GitSelection {
    let mut issues: Vec<SelectionIssue> =
        check_current_repo(opts, current_dir).into_iter().collect();
    let (repos, path_repo) = discover_repos(commands, &mut issues);
    let scanned = scan_repos(repos, opts, &mut issues);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn canonical_tempdir() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        (tmp, root)
    }

    #[test]
    fn index_override_resolves_relative_index_in_work_tree() {
        let (_tmp, root) = canonical_tempdir();
        Repository::init(&root).unwrap();
        std::fs::create_dir(root.join("sub")).unwrap();

        let over = index_override(None, Some(OsStr::new(".git/index")), &root.join("sub")).unwrap();
        assert_eq!(over.git_dir, root.join(".git"));
        assert_eq!(over.index_file, root.join(".git/index"));
    }

    #[test]
    fn index_override_prefers_git_dir() {
        let (_tmp, root) = canonical_tempdir();
        for name in ["a", "b"] {
            Repository::init(root.join(name)).unwrap();
        }
        let git_dir = root.join("b/.git");

        let over = index_override(
            Some(git_dir.as_os_str()),
            Some(OsStr::new("next-index.lock")),
            &root.join("a"),
        )
        .unwrap();
        assert_eq!(over.git_dir, git_dir);
        assert_eq!(over.index_file, root.join("b/next-index.lock"));

        let absolute = root.join("b/.git/index.lock");
        let over = index_override(None, Some(absolute.as_os_str()), &root.join("a")).unwrap();
        assert_eq!(over.git_dir, root.join("a/.git"));
        assert_eq!(over.index_file, absolute);
    }

    #[test]
    fn index_override_needs_index_file() {
        let (_tmp, root) = canonical_tempdir();
        Repository::init(&root).unwrap();
        assert_eq!(index_override(None, None, &root), None);
        assert_eq!(index_override(None, Some(OsStr::new("")), &root), None);
    }

    fn staged() -> SelectOptions {
        SelectOptions {
            scope: GitScope::Staged,
            index_override: None,
        }
    }

    #[test]
    fn staged_outside_repo_is_fatal() {
        let (_tmp, root) = canonical_tempdir();
        if Repository::open_ext(&root, RepositoryOpenFlags::CROSS_FS, &[] as &[&Path]).is_ok() {
            eprintln!("skipping: the temp dir is inside a git repo");
            return;
        }

        let issues = select(&[], &staged(), Some(&root)).issues;
        assert!(
            matches!(
                issues.as_slice(),
                [SelectionIssue::NotInRepo { path, command_ids, fatal: true, .. }]
                    if path == &root && command_ids.is_empty()
            ),
            "{issues:?}"
        );
        assert!(issues[0].is_fatal());

        assert!(
            select(&[], &SelectOptions::default(), Some(&root))
                .issues
                .is_empty()
        );
    }

    #[test]
    fn staged_inside_repo_is_not_fatal() {
        let (_tmp, root) = canonical_tempdir();
        Repository::init(&root).unwrap();
        assert!(select(&[], &staged(), Some(&root)).issues.is_empty());
    }
}
