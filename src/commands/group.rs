use crate::commands::auto::Auto;
use crate::commands::command::Command;
use std::collections::HashMap;
use std::path::PathBuf;

/// Hierarchical grouping of related commands
#[derive(Debug, Clone, Default)]
pub struct CommandGroup {
    pub id: String,
    pub name: String,
    pub auto: Auto,
    pub cwd: PathBuf,
    pub commands: Vec<Command>,
    pub children: Vec<CommandGroup>,
    pub env: HashMap<String, String>,
}

impl CommandGroup {
    /// Returns a flattened list of all commands in this group and its children
    #[must_use]
    pub fn all_commands(&self) -> Vec<&Command> {
        self.commands
            .iter()
            .chain(self.children.iter().flat_map(|child| child.all_commands()))
            .collect()
    }
}
