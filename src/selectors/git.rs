use std::collections::HashMap;
use std::path::{Path, PathBuf};

use git2::Repository;
use log::debug;
use regex_cache::LazyRegex;

use crate::commands::command::Command;
use crate::selectors::{RunnableSelector, SelectorError};

#[derive(Default)]
struct GitScanner {
    repository_cache: HashMap<PathBuf, PathBuf>,
    repo_changes_cache: HashMap<PathBuf, Vec<PathBuf>>,
}

impl GitScanner {
    fn get_repo(&mut self, path: &Path) -> Result<PathBuf, git2::Error> {
        if let Some(cached_path) = self.repository_cache.get(path).cloned() {
            Ok(cached_path)
        } else {
            let repo_path = Repository::discover_path(path, &[] as &[&Path])?;
            // This is the .git directory, we want the parent directory
            let repo_path = repo_path
                .parent()
                .ok_or_else(|| git2::Error::from_str("Git repo path has no parent directory"))?
                .to_path_buf();
            debug!("Discovered git repo at {}", repo_path.display());
            self.repository_cache
                .insert(path.to_path_buf(), repo_path.clone());
            Ok(repo_path)
        }
    }

    fn get_changes(&mut self, repo: &Path) -> Result<Vec<PathBuf>, git2::Error> {
        if let Some(cached_changes) = self.repo_changes_cache.get(repo).cloned() {
            Ok(cached_changes)
        } else {
            let changes: Vec<PathBuf> = Repository::open(repo)?
                .statuses(None)?
                .iter()
                .filter(|entry| !entry.status().is_ignored())
                .filter_map(|status| status.path().map(PathBuf::from))
                .collect();
            debug!(
                "Found {} changed files in {}",
                changes.len(),
                repo.display()
            );
            self.repo_changes_cache
                .insert(repo.to_path_buf(), changes.clone());
            Ok(changes)
        }
    }

    fn has_changes(&mut self, path: &Path, patterns: &[LazyRegex]) -> Result<bool, git2::Error> {
        let repo = self.get_repo(path)?;
        let changes = self.get_changes(&repo)?;

        let has_match = changes
            .iter()
            .map(|change| repo.join(change))
            .filter(|change| change.starts_with(path))
            .any(|change| {
                let s = change.to_string_lossy();
                patterns.iter().any(|pattern| pattern.is_match(&s))
            });

        debug!(
            "Path {} {} git changes",
            path.display(),
            if has_match { "has" } else { "has no" }
        );
        Ok(has_match)
    }
}

pub(crate) struct GitSelector {}

impl RunnableSelector for GitSelector {
    fn split_active_commands(
        commands: Vec<Command>,
    ) -> Result<(Vec<Command>, Vec<Command>), SelectorError> {
        let mut git_scanner = GitScanner::default();

        let mut with_git = Vec::new();
        let mut other = Vec::new();

        for command in commands {
            if command.auto.git.unwrap_or(false) {
                let has_git_changes = command.auto.path.iter().try_fold(
                    false,
                    |acc, path| -> Result<bool, SelectorError> {
                        Ok(acc || git_scanner.has_changes(path, &command.auto.regex)?)
                    },
                )?;

                if has_git_changes {
                    debug!("Git-selected command '{}'", command.name);
                    with_git.push(command);
                } else {
                    other.push(command);
                }
            } else {
                other.push(command);
            }
        }

        Ok((with_git, other))
    }
}
