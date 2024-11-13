//! Git integration for detecting file changes
//!
//! This module handles the detection of changed files in git repositories
//! and matches them against command automation rules.

use crate::command_group::Command;
use git2::Repository;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Errors that can occur during git operations
#[derive(Error, Debug)]
pub enum GitError {
    /// Indicates that a git repository couldn't be found for the given path
    #[error("Unable to find git repository for path: {0}")]
    NoGitRepo(PathBuf),
    /// Indicates an invalid regex pattern in command configuration
    #[error("Invalid regex pattern: {0}")]
    Regex(#[from] regex::Error),
    /// Indicates a general git operation error
    #[error("Git operation failed: {0}")]
    Git(#[from] git2::Error),
}

/// Groups commands by their path and regex patterns
fn group_commands<'a>(
    commands: Vec<&'a Command>,
) -> HashMap<(&'a Vec<PathBuf>, &'a Vec<String>), Vec<&'a Command>> {
    let mut grouped_commands: HashMap<(&'a Vec<PathBuf>, &'a Vec<String>), Vec<&'a Command>> =
        HashMap::new();
    for command in commands {
        let key = (&command.auto.path, &command.auto.regex);
        grouped_commands.entry(key).or_default().push(command);
    }
    grouped_commands
}

/// Checks if a file path matches any of the provided regex patterns
fn matches_patterns(file_path: &str, patterns: &[String]) -> Result<bool, GitError> {
    for pattern in patterns {
        let re = regex::Regex::new(pattern)?;
        if re.is_match(file_path) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Finds commands that match changed files in a repository
fn find_matching_commands<'a>(
    repo: &Repository,
    commands: &[&'a Command],
    patterns: &[String],
) -> Result<Vec<&'a Command>, GitError> {
    let statuses = repo.statuses(None)?;
    let mut matching_commands = Vec::new();

    for status in statuses.iter() {
        if let Some(path) = status.path() {
            if matches_patterns(path, patterns)? {
                matching_commands.extend(commands);
            }
        }
    }

    Ok(matching_commands)
}

/// Locates git repositories for a set of paths
///
/// Attempts to find git repositories containing each path. If any path
/// is not contained in a git repository, returns an error.
fn find_git_repos(paths: &[PathBuf], cwd: &Path) -> Result<Vec<Repository>, GitError> {
    let mut git_repos = Vec::new();
    for path in paths {
        let repo_path = cwd.join(path);
        let repo =
            Repository::discover(repo_path).map_err(|_| GitError::NoGitRepo(path.clone()))?;
        git_repos.push(repo);
    }
    Ok(git_repos)
}

/// Finds commands that should run based on git changes
///
/// Examines git status for relevant repositories and returns commands whose
/// automation rules match the changed files. Commands with `always=true`
/// are always included regardless of git status.
///
/// # Arguments
///
/// * `commands` - List of commands to check
/// * `cwd` - Base directory for resolving relative paths
///
/// # Returns
///
/// Returns a list of commands that should be executed based on their git
/// automation rules and the current repository status.
///
///
/// # Errors
///
/// * `GitError::NoGitRepo` if a watched path isn't in a git repository
/// * `GitError::Regex` if a command contains an invalid regex pattern
/// * `GitError::Git` if a git operation fails
pub fn commands_with_changes<'a>(
    commands: Vec<&'a Command>,
    cwd: &Path,
) -> Result<Vec<&'a Command>, GitError> {
    // Partition commands into always-run and git-automated
    let (always_commands, remaining_commands): (Vec<&Command>, Vec<&Command>) =
        commands.iter().partition(|command| command.auto.always);

    let auto_commands: Vec<&Command> = remaining_commands
        .into_iter()
        .filter(|command| command.auto.git)
        .collect();

    let grouped_commands = group_commands(auto_commands);
    let mut changed_commands = Vec::new();

    // Process each group of commands
    for ((paths, patterns), commands) in grouped_commands {
        let repos = find_git_repos(paths, cwd)?;
        for repo in repos {
            let matching = find_matching_commands(&repo, &commands, patterns)?;
            changed_commands.extend(matching);
        }
    }

    // Combine git-triggered and always-run commands
    Ok(changed_commands
        .into_iter()
        .chain(always_commands)
        .collect())
}
