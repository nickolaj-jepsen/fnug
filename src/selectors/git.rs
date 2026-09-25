use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use git2::{Repository, RepositoryOpenFlags};
use log::{debug, warn};

use crate::commands::command::Command;
use crate::selectors::{RunnableSelector, SelectorError};

/// Discover the work tree root of the git repo containing `path`.
/// Fails if no repo contains `path` or the repo is bare.
fn discover_repo(path: &Path) -> Result<PathBuf, git2::Error> {
    // Not `Repository::discover`: it reopens the gitdir, so a `.git` file without
    // `core.worktree` (as `git init --separate-git-dir` writes) gets the wrong work tree.
    let repo = Repository::open_ext(path, RepositoryOpenFlags::CROSS_FS, &[] as &[&Path])?;
    let repo_path = repo
        .workdir()
        .ok_or_else(|| {
            git2::Error::from_str(&format!(
                "bare repository at {} has no working tree",
                repo.path().display()
            ))
        })?
        .to_path_buf();
    debug!("Discovered git repo at {}", repo_path.display());
    Ok(repo_path)
}

/// Open a repo and collect all non-ignored changed file paths.
/// This is the expensive I/O operation we want to parallelize.
fn scan_repo(repo_path: &Path) -> Result<Vec<PathBuf>, git2::Error> {
    let changes: Vec<PathBuf> = Repository::open(repo_path)?
        .statuses(None)?
        .iter()
        .filter(|entry| !entry.status().is_ignored())
        .filter_map(|status| status.path().map(PathBuf::from))
        .collect();
    debug!(
        "Found {} changed files in {}",
        changes.len(),
        repo_path.display()
    );
    Ok(changes)
}

/// Check whether a command has matching git changes given pre-scanned repo data.
fn command_has_changes(
    cmd: &Command,
    path_to_repo: &HashMap<PathBuf, Option<PathBuf>>,
    repo_changes: &HashMap<PathBuf, Vec<PathBuf>>,
) -> bool {
    cmd.auto.paths().iter().any(|path| {
        let Some(Some(repo_path)) = path_to_repo.get(path) else {
            return false;
        };
        let Some(changes) = repo_changes.get(repo_path) else {
            return false;
        };
        let has_match = changes
            .iter()
            .map(|change| repo_path.join(change))
            .filter(|change| change.starts_with(path))
            .any(|change| {
                let regexes = cmd.auto.regexes();
                if regexes.is_empty() {
                    return true;
                }
                let s = change.to_string_lossy();
                regexes.iter().any(|pattern| pattern.is_match(&s))
            });
        if has_match {
            debug!("Path {} has git changes", path.display());
        }
        has_match
    })
}

pub(crate) struct GitSelector {}

impl RunnableSelector for GitSelector {
    /// Paths outside any work tree and repos that fail to scan are skipped with a warning:
    /// they select nothing, but never fail the selection.
    fn split_active_commands(
        commands: Vec<Command>,
    ) -> Result<(Vec<Command>, Vec<Command>), SelectorError> {
        // 1. Separate git-enabled commands from non-git commands
        let (git_commands, non_git): (Vec<_>, Vec<_>) = commands
            .into_iter()
            .partition(|c| c.auto.git.unwrap_or(false));

        if git_commands.is_empty() {
            return Ok((vec![], non_git));
        }

        // 2. Discover repos for each command's paths (sequential, fast filesystem traversal)
        let mut path_to_repo: HashMap<PathBuf, Option<PathBuf>> = HashMap::new();
        for cmd in &git_commands {
            for path in cmd.auto.paths() {
                path_to_repo.entry(path.clone()).or_insert_with(|| {
                    discover_repo(path)
                        .inspect_err(|e| {
                            warn!(
                                "Git selection skipped for {}: {}",
                                path.display(),
                                e.message()
                            );
                        })
                        .ok()
                });
            }
        }

        // 3. Collect unique repo paths
        let unique_repos: Vec<PathBuf> = path_to_repo
            .values()
            .flatten()
            .collect::<HashSet<_>>()
            .into_iter()
            .cloned()
            .collect();

        // 4. Scan all repos in parallel (the expensive I/O part); a failed scan selects nothing
        let repo_changes: HashMap<PathBuf, Vec<PathBuf>> = std::thread::scope(|s| {
            let handles: Vec<_> = unique_repos
                .into_iter()
                .map(|repo_path| {
                    let handle = s.spawn({
                        let repo_path = repo_path.clone();
                        move || scan_repo(&repo_path)
                    });
                    (repo_path, handle)
                })
                .collect();

            let mut results = HashMap::new();
            for (repo_path, handle) in handles {
                match handle.join() {
                    Ok(Ok(changes)) => {
                        results.insert(repo_path, changes);
                    }
                    Ok(Err(e)) => warn!(
                        "Git scan of {} failed: {}",
                        repo_path.display(),
                        e.message()
                    ),
                    Err(_) => warn!("Git scan of {} panicked", repo_path.display()),
                }
            }
            results
        });

        // 5. Match each command's patterns against its repo's cached changes
        let mut with_git = Vec::new();
        let mut without_git = Vec::new();
        for cmd in git_commands {
            if command_has_changes(&cmd, &path_to_repo, &repo_changes) {
                debug!("Git-selected command '{}'", cmd.name);
                with_git.push(cmd);
            } else {
                without_git.push(cmd);
            }
        }

        Ok((with_git, without_git.into_iter().chain(non_git).collect()))
    }
}
