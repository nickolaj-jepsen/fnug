use std::collections::HashSet;
use std::fmt;
use std::path::PathBuf;

use log::{debug, warn};

use crate::commands::command::Command;
use thiserror::Error;

pub(crate) mod always;
mod git;
mod matching;
pub mod watch;

pub use matching::{match_subject, relative_to};

/// Errors that can occur during selector operations
#[derive(Error, Debug)]
pub enum SelectorError {
    /// Indicates a general git operation error
    #[error("Git operation failed: {0}")]
    Git(#[from] git2::Error),

    /// A spawned thread panicked during execution
    #[error("Thread panicked during git scan")]
    ThreadPanic,
}

pub trait RunnableSelector {
    /// Split commands into (active, inactive) based on this selector's criteria.
    ///
    /// # Errors
    ///
    /// Returns `SelectorError` if the selection logic fails.
    fn split_active_commands(
        commands: Vec<Command>,
    ) -> Result<(Vec<Command>, Vec<Command>), SelectorError>;
}

/// Which changes git selection looks at.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum GitScope {
    /// Uncommitted changes: staged, unstaged and untracked files.
    #[default]
    WorkingTree,
}

/// How [`select`] looks for changes.
#[derive(Debug, Clone, Default)]
pub struct SelectOptions {
    pub scope: GitScope,
}

/// Why a command was selected. `Always` wins when both apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectedBy {
    Always,
    Git,
}

/// A command chosen by [`select`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedCommand {
    pub id: String,
    pub by: SelectedBy,
    /// Changed files matching the command's `auto` rules that still exist: absolute, sorted and
    /// deduplicated. Filled for every git-enabled command, including always-selected ones.
    pub files: Vec<PathBuf>,
}

/// A problem that kept part of the git selection from running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectionIssue {
    /// An `auto.path` is not inside a git work tree, so it selects none of `command_ids`.
    NotInRepo {
        path: PathBuf,
        command_ids: Vec<String>,
        message: String,
    },
    /// The repo with this work tree could not be scanned, so nothing in it selects.
    ScanFailed { repo: PathBuf, message: String },
}

impl SelectionIssue {
    /// Whether the selection as a whole is unusable, rather than missing part of its input.
    #[must_use]
    pub fn is_fatal(&self) -> bool {
        match self {
            SelectionIssue::NotInRepo { .. } | SelectionIssue::ScanFailed { .. } => false,
        }
    }
}

impl fmt::Display for SelectionIssue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SelectionIssue::NotInRepo { path, message, .. } => {
                write!(f, "git selection skipped for {}: {message}", path.display())
            }
            SelectionIssue::ScanFailed { repo, message } => {
                write!(f, "git scan of {} failed: {message}", repo.display())
            }
        }
    }
}

/// The result of [`select`].
#[derive(Debug, Clone, Default)]
pub struct SelectorOutput {
    /// Selected commands, in input order.
    pub commands: Vec<SelectedCommand>,
    /// Distinct changed paths found across the scanned repos.
    pub changed_files: usize,
    pub issues: Vec<SelectionIssue>,
}

impl SelectorOutput {
    #[must_use]
    pub fn contains(&self, id: &str) -> bool {
        self.get(id).is_some()
    }

    #[must_use]
    pub fn get(&self, id: &str) -> Option<&SelectedCommand> {
        self.commands.iter().find(|c| c.id == id)
    }

    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.commands.iter().map(|c| c.id.as_str())
    }

    #[must_use]
    pub fn has_fatal(&self) -> bool {
        self.issues.iter().any(SelectionIssue::is_fatal)
    }
}

/// Select the commands whose `auto` rules apply: `always`, or `git` with a changed file under
/// one of its `auto.path` prefixes that matches one of its `auto.regex` patterns (any file, if
/// none are set). Patterns see the file's path relative to the command's `cwd`, see
/// [`match_subject`].
///
/// Never fails as a whole: a path outside any git work tree or a repo that can't be scanned
/// selects nothing and is reported in `issues`.
#[must_use]
pub fn select(commands: &[&Command], opts: &SelectOptions) -> SelectorOutput {
    let git = git::select(commands, opts);
    let selected: Vec<SelectedCommand> = commands
        .iter()
        .zip(git.matches)
        .filter_map(|(cmd, files)| {
            let by = if cmd.auto.always == Some(true) {
                SelectedBy::Always
            } else if files.is_some() {
                SelectedBy::Git
            } else {
                return None;
            };
            Some(SelectedCommand {
                id: cmd.id.clone(),
                by,
                files: files.unwrap_or_default(),
            })
        })
        .collect();
    debug!(
        "Selected {} commands ({} changed files, {} issues)",
        selected.len(),
        git.changed_files,
        git.issues.len()
    );
    SelectorOutput {
        commands: selected,
        changed_files: git.changed_files,
        issues: git.issues,
    }
}

/// Runs [`select`] on the working tree and returns the selected commands. Issues are logged
/// as warnings.
///
/// # Errors
///
/// Never; the `Result` is kept for compatibility.
pub fn get_selected_commands(commands: Vec<Command>) -> Result<Vec<Command>, SelectorError> {
    let refs: Vec<&Command> = commands.iter().collect();
    let output = select(&refs, &SelectOptions::default());
    for issue in &output.issues {
        warn!("{issue}");
    }
    let ids: HashSet<&str> = output.ids().collect();
    Ok(commands
        .into_iter()
        .filter(|c| ids.contains(c.id.as_str()))
        .collect())
}
