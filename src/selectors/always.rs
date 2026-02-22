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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::auto::Auto;

    fn make_cmd(id: &str, always: Option<bool>) -> Command {
        Command {
            id: id.to_string(),
            name: id.to_string(),
            auto: Auto {
                always,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn test_always_true_is_selected() {
        let commands = vec![make_cmd("a", Some(true)), make_cmd("b", Some(false))];
        let (selected, other) = AlwaysSelector::split_active_commands(commands).unwrap();
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].id, "a");
        assert_eq!(other.len(), 1);
        assert_eq!(other[0].id, "b");
    }
}
