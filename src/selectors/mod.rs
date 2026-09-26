use std::fmt;
use std::path::PathBuf;

use log::debug;

use crate::commands::command::Command;

pub(crate) mod always;
mod git;
mod ignore;
mod matching;
pub mod watch;

pub use matching::{match_subject, relative_to};

pub trait RunnableSelector {
    /// Split commands into (active, inactive) based on this selector's criteria.
    fn split_active_commands(commands: Vec<Command>) -> (Vec<Command>, Vec<Command>);
}

/// Which changes git selection looks at.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum GitScope {
    /// Uncommitted changes: staged, unstaged and untracked files.
    #[default]
    WorkingTree,
    /// Changes staged in the index, compared with `HEAD` (the empty tree before the first
    /// commit). Unstaged and untracked changes don't count.
    Staged,
    /// Changes since the merge base of `HEAD` and a revision such as `origin/main`: commits
    /// since then plus staged, unstaged and untracked changes, like a pull request's diff
    /// with the work in progress on top.
    Since(String),
}

/// A temporary index to read instead of a repo's own, such as the one git names in
/// `GIT_INDEX_FILE` while running hooks for `git commit -a` or `git commit <paths>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexOverride {
    /// The git directory of the repo the index belongs to. Other repos read their own index.
    pub git_dir: PathBuf,
    pub index_file: PathBuf,
}

/// How [`select`] looks for changes.
#[derive(Debug, Clone, Default)]
pub struct SelectOptions {
    pub scope: GitScope,
    /// Read by the staged scope only.
    pub index_override: Option<IndexOverride>,
}

impl SelectOptions {
    /// Options for `scope` that read the index git hands to hooks: `GIT_INDEX_FILE`, which
    /// belongs to the repo at `GIT_DIR`, or else to the repo containing the working directory.
    /// A relative `GIT_INDEX_FILE` is resolved against that repo's work tree, where git runs
    /// hooks, so it still applies after a hook changes directory.
    #[must_use]
    pub fn from_env(scope: GitScope) -> Self {
        let index_override = std::env::current_dir().ok().and_then(|cwd| {
            git::index_override(
                std::env::var_os("GIT_DIR").as_deref(),
                std::env::var_os("GIT_INDEX_FILE").as_deref(),
                &cwd,
            )
        });
        SelectOptions {
            scope,
            index_override,
        }
    }
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
    /// Under the staged scope, `path` may also be the working directory, whose repo must
    /// exist: then `command_ids` is empty and the issue is fatal.
    NotInRepo {
        path: PathBuf,
        command_ids: Vec<String>,
        message: String,
        fatal: bool,
    },
    /// The repo with this work tree could not be scanned, so nothing in it selects.
    ScanFailed { repo: PathBuf, message: String },
    /// The since-base scope's `base` can't be resolved in this repo, or shares no history with
    /// its `HEAD`, so nothing in it selects.
    BaseRefNotFound {
        repo: PathBuf,
        base: String,
        message: String,
    },
    /// The since-base scope found no `HEAD` commit in this repo, so nothing in it selects.
    UnbornHead { repo: PathBuf },
}

impl SelectionIssue {
    /// Whether the selection as a whole is unusable, rather than missing part of its input.
    #[must_use]
    pub fn is_fatal(&self) -> bool {
        match self {
            SelectionIssue::NotInRepo { fatal, .. } => *fatal,
            SelectionIssue::ScanFailed { .. } => false,
            SelectionIssue::BaseRefNotFound { .. } | SelectionIssue::UnbornHead { .. } => true,
        }
    }
}

impl fmt::Display for SelectionIssue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SelectionIssue::NotInRepo {
                path,
                message,
                fatal: true,
                ..
            } => write!(
                f,
                "staged changes need a git repository, but {} is not in one: {message}",
                path.display()
            ),
            SelectionIssue::NotInRepo { path, message, .. } => {
                write!(f, "git selection skipped for {}: {message}", path.display())
            }
            SelectionIssue::ScanFailed { repo, message } => {
                write!(f, "git scan of {} failed: {message}", repo.display())
            }
            SelectionIssue::BaseRefNotFound {
                repo,
                base,
                message,
            } => write!(
                f,
                "can't compare {} with base '{base}': {message} (in CI, fetch the base with \
                 full history, e.g. actions/checkout with fetch-depth: 0)",
                repo.display()
            ),
            SelectionIssue::UnbornHead { repo } => write!(
                f,
                "can't compare {} with a base: HEAD has no commits yet",
                repo.display()
            ),
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
/// selects nothing and is reported in `issues`. Issues that make the result unusable, such as
/// the staged scope outside a repo, are reported the same way; see
/// [`SelectionIssue::is_fatal`].
#[must_use]
pub fn select(commands: &[&Command], opts: &SelectOptions) -> SelectorOutput {
    let current_dir = std::env::current_dir().ok();
    let git = git::select(commands, opts, current_dir.as_deref());
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
