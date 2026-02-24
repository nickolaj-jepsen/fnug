use log::debug;

use crate::commands::command::Command;
use thiserror::Error;

pub(crate) mod always;
mod git;
pub mod watch;

/// Errors that can occur during selector operations
#[derive(Error, Debug)]
pub enum SelectorError {
    /// Indicates a general git operation error
    #[error("Git operation failed: {0}")]
    Git(#[from] git2::Error),
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

/// Runs all selectors and returns the selected commands.
///
/// # Errors
///
/// Returns `SelectorError::Git` if git operations fail during selection.
pub fn get_selected_commands(commands: Vec<Command>) -> Result<Vec<Command>, SelectorError> {
    let (always_commands, unselected) = always::AlwaysSelector::split_active_commands(commands)?;
    let (git_commands, _) = git::GitSelector::split_active_commands(unselected)?;

    debug!(
        "Selected {} always + {} git commands",
        always_commands.len(),
        git_commands.len()
    );

    Ok(always_commands.into_iter().chain(git_commands).collect())
}
