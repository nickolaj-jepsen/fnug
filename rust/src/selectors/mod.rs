use crate::commands::command::Command;
use pyo3::exceptions::PyValueError;
use pyo3::PyErr;
use thiserror::Error;

mod always;
mod git;
mod watch;

/// Errors that can occur during selector operations
#[derive(Error, Debug)]
pub enum SelectorError {
    /// Indicates a general git operation error
    #[error("Git operation failed: {0}")]
    Git(#[from] git2::Error),
}

impl From<SelectorError> for PyErr {
    fn from(error: SelectorError) -> Self {
        match error {
            SelectorError::Git(err) => {
                PyValueError::new_err(format!("Error running git command: {:?}", err))
            }
        }
    }
}

pub trait RunnableSelector {
    fn split_active_commands(
        commands: Vec<Command>,
    ) -> Result<(Vec<Command>, Vec<Command>), SelectorError>;
}

/// Runs all selectors and returns the selected commands
///
/// Each selector will be run in order, if a selector returns a command, we can skip this command in the next selectors, as it has already been selected.
pub fn get_selected_commands(commands: Vec<Command>) -> Result<Vec<Command>, SelectorError> {
    let (always_commands, unselected) = always::AlwaysSelector::split_active_commands(commands)?;
    let (git_commands, _) = git::GitSelector::split_active_commands(unselected)?;

    // Combine the commands from both selectors
    Ok(always_commands.into_iter().chain(git_commands).collect())
}
