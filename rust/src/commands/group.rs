use crate::commands::auto::Auto;
use crate::commands::command::Command;
use crate::config_file::{ConfigCommandGroup, ConfigError};
use pyo3::{pyclass, pymethods};
use std::path::PathBuf;
use uuid::Uuid;

/// Hierarchical grouping of related commands
///
/// CommandGroups form the nodes of a command tree, allowing logical organization
/// of related commands. Groups can define common settings that are inherited by
/// their children:
///
/// - Working directory - Children execute relative to parent's directory
/// - Automation rules - Children inherit and can extend parent rules
/// - File patterns - Children can add to parent's watch patterns
///
/// # Examples
///
/// ```python
/// group = CommandGroup(
///     name="backend",
///     auto=Auto(git=True, path=["backend/"]),
///     commands=[Command(name="test", cmd="cargo test")],
///     children=[CommandGroup(name="api", ...)]
/// )
/// ```
#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "stub_gen", pyo3_stub_gen::derive::gen_stub_pyclass)]
#[pyclass]
#[pyo3(get_all)]
pub struct CommandGroup {
    pub id: String,
    pub name: String,
    pub auto: Auto,
    pub cwd: PathBuf,
    pub commands: Vec<Command>,
    pub children: Vec<CommandGroup>,
}

#[cfg_attr(feature = "stub_gen", pyo3_stub_gen::derive::gen_stub_pymethods)]
#[pymethods]
impl CommandGroup {
    #[new]
    #[pyo3(signature = (name, id = Uuid::new_v4().to_string(), auto = Auto::default(), cwd = PathBuf::new(), commands = Vec::new(), children = Vec::new()))]
    fn new(
        name: String,
        id: String,
        auto: Auto,
        cwd: PathBuf,
        commands: Vec<Command>,
        children: Vec<CommandGroup>,
    ) -> Self {
        CommandGroup {
            id,
            name,
            auto,
            cwd,
            commands,
            children,
        }
    }

    fn as_yaml(&self) -> Result<String, ConfigError> {
        let config_group: ConfigCommandGroup = self.clone().into();
        config_group.as_yaml()
    }
}

impl CommandGroup {
    /// Returns a flattened list of all commands in this group and its children
    pub fn all_commands(&self) -> Vec<&Command> {
        self.commands
            .iter()
            .chain(self.children.iter().flat_map(|child| child.all_commands()))
            .collect()
    }
}
