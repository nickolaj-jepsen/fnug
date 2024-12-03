use crate::commands::command::Command;
use crate::selectors::{RunnableSelector, SelectorError};
use git2::Repository;
use regex_cache::LazyRegex;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Default)]
struct GitScanner {
    repository_cache: HashMap<PathBuf, PathBuf>,
    repo_changes_cache: HashMap<PathBuf, Vec<PathBuf>>,
}

impl GitScanner {
    fn get_repo(&mut self, path: &PathBuf) -> Result<PathBuf, git2::Error> {
        if let Some(cached_path) = self.repository_cache.get(path).cloned() {
            Ok(cached_path)
        } else {
            let repo_path = Repository::discover_path(path, &[] as &[&Path])?;
            // This is the .git directory, we want the parent directory
            let repo_path = repo_path.parent().unwrap().to_path_buf();
            self.repository_cache
                .insert(path.clone(), repo_path.clone());
            Ok(repo_path)
        }
    }

    fn get_changes(&mut self, repo: &PathBuf) -> Result<Vec<PathBuf>, git2::Error> {
        if let Some(cached_changes) = self.repo_changes_cache.get(repo).cloned() {
            Ok(cached_changes)
        } else {
            let changes = Repository::open(repo)?
                .statuses(None)?
                .iter()
                .filter(|entry| !entry.status().is_ignored())
                .map(|status| {
                    let path = status.path().unwrap();
                    Ok(PathBuf::from(path))
                })
                .collect::<Result<Vec<PathBuf>, git2::Error>>()?;
            self.repo_changes_cache
                .insert(repo.clone(), changes.clone());
            Ok(changes)
        }
    }

    fn has_changes(
        &mut self,
        path: &PathBuf,
        patterns: Vec<LazyRegex>,
    ) -> Result<bool, git2::Error> {
        let repo = self.get_repo(path)?;
        let changes = self.get_changes(&repo)?;

        let changes = changes
            .iter()
            // Get the absolute path of the change
            .map(|change| repo.join(change))
            // Remove any changes that are not in the watched path
            .filter(|change| change.starts_with(path))
            // Convert to string
            .map(|change| change.to_string_lossy().to_string())
            // Remove any changes that do not match the regex
            .filter(|change| patterns.iter().any(|pattern| pattern.is_match(change)))
            .collect::<Vec<String>>();

        Ok(!changes.is_empty())
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
                        Ok(acc || git_scanner.has_changes(path, command.auto.regex.clone())?)
                    },
                )?;

                if has_git_changes {
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
