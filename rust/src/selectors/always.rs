use crate::commands::command::Command;

use super::{RunnableSelector, SelectorError};

pub(crate) struct AlwaysSelector {}

impl RunnableSelector for AlwaysSelector {
    fn split_active_commands(
        commands: Vec<Command>,
    ) -> Result<(Vec<Command>, Vec<Command>), SelectorError> {
        let (always, other): (Vec<Command>, Vec<Command>) = commands
            .into_iter()
            .partition(|command| command.auto.always.unwrap_or(false));
        Ok((always, other))
    }
}
