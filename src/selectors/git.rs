use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use git2::Repository;
use log::debug;

use crate::commands::command::Command;
use crate::selectors::{RunnableSelector, SelectorError};

/// Discover the git repo root for a given path, using a cache to avoid
/// repeated filesystem traversal for paths in the same repo.
fn discover_repo(
    path: &Path,
    cache: &mut HashMap<PathBuf, PathBuf>,
) -> Result<PathBuf, git2::Error> {
    if let Some(cached) = cache.get(path) {
        return Ok(cached.clone());
    }
    let repo_path = Repository::discover_path(path, &[] as &[&Path])?;
    // discover_path returns the .git directory; we want the parent (worktree root)
    let repo_path = repo_path
        .parent()
        .ok_or_else(|| git2::Error::from_str("Git repo path has no parent directory"))?
        .to_path_buf();
    debug!("Discovered git repo at {}", repo_path.display());
    cache.insert(path.to_path_buf(), repo_path.clone());
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
    path_to_repo: &HashMap<PathBuf, PathBuf>,
    repo_changes: &HashMap<PathBuf, Vec<PathBuf>>,
) -> bool {
    cmd.auto.path.iter().any(|path| {
        let Some(repo_path) = path_to_repo.get(path) else {
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
                let s = change.to_string_lossy();
                cmd.auto.regex.iter().any(|pattern| pattern.is_match(&s))
            });
        if has_match {
            debug!("Path {} has git changes", path.display());
        }
        has_match
    })
}

pub(crate) struct GitSelector {}

impl RunnableSelector for GitSelector {
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
        let mut discover_cache: HashMap<PathBuf, PathBuf> = HashMap::new();
        let mut path_to_repo: HashMap<PathBuf, PathBuf> = HashMap::new();
        for cmd in &git_commands {
            for path in &cmd.auto.path {
                if !path_to_repo.contains_key(path) {
                    let repo_path = discover_repo(path, &mut discover_cache)?;
                    path_to_repo.insert(path.clone(), repo_path);
                }
            }
        }

        // 3. Collect unique repo paths
        let unique_repos: Vec<PathBuf> = path_to_repo
            .values()
            .collect::<HashSet<_>>()
            .into_iter()
            .cloned()
            .collect();

        // 4. Scan all repos in parallel (the expensive I/O part)
        let repo_changes: HashMap<PathBuf, Vec<PathBuf>> = std::thread::scope(|s| {
            let handles: Vec<_> = unique_repos
                .into_iter()
                .map(|repo_path| {
                    s.spawn(move || -> Result<(PathBuf, Vec<PathBuf>), git2::Error> {
                        let changes = scan_repo(&repo_path)?;
                        Ok((repo_path, changes))
                    })
                })
                .collect();

            let mut results = HashMap::new();
            for handle in handles {
                let (path, changes) = handle.join().unwrap()?;
                results.insert(path, changes);
            }
            Ok::<_, git2::Error>(results)
        })?;

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
