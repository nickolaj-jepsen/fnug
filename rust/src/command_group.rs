//! Command execution framework with hierarchical organization
//!
//! This module implements a tree-based command organization system where commands can be grouped
//! and nested. Each command or group can have automation rules that determine when they should
//! be executed based on file system changes or git status.
//!
//! The inheritance system allows automation rules and working directories to flow down from
//! parent groups to their children, while still allowing override at any level.

use crate::config_file::{ConfigAuto, ConfigCommand, ConfigCommandGroup};
use pyo3::{pyclass, pymethods};
use pyo3_stub_gen::derive::{gen_stub_pyclass, gen_stub_pymethods};
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Represents inheritable configuration settings for commands and groups
struct InheritableSettings {
    cwd: PathBuf,
    auto: Auto,
}

impl InheritableSettings {
    /// Merges the current settings with a parent configuration
    fn merge_with_parent(&self, parent: &InheritableSettings) -> InheritableSettings {
        InheritableSettings {
            cwd: if self.cwd == PathBuf::from(".") {
                parent.cwd.clone()
            } else {
                self.cwd.clone()
            },
            auto: Auto {
                watch: self.auto.watch || parent.auto.watch,
                git: self.auto.git || parent.auto.git,
                regex: if self.auto.regex.is_empty() {
                    parent.auto.regex.clone()
                } else {
                    self.auto.regex.clone()
                },
                always: self.auto.always || parent.auto.always,
                path: if self.auto.path.is_empty() {
                    parent.auto.path.clone()
                } else {
                    self.auto.path.clone()
                },
            },
        }
    }
}

/// Automation rules that determine when commands should execute
///
/// # Examples
///
/// ```python
/// # Watch for git changes in specific paths matching regex patterns
/// auto = Auto(
///     watch=True,
///     git=True,
///     path=["src/", "tests/"],
///     regex=[".*\\.rs$", ".*\\.toml$"]
/// )
/// ```
#[derive(Default, Debug, Clone)]
#[gen_stub_pyclass]
#[pyclass]
#[pyo3(get_all)]
pub struct Auto {
    pub watch: bool,
    pub git: bool,
    pub path: Vec<PathBuf>,
    pub regex: Vec<String>,
    pub always: bool,
}

#[gen_stub_pymethods]
#[pymethods]
impl Auto {
    #[new]
    #[pyo3(signature = (watch = false, git = false, path = Vec::new(), regex = Vec::new(), always = false))]
    fn new(watch: bool, git: bool, path: Vec<PathBuf>, regex: Vec<String>, always: bool) -> Self {
        Auto {
            watch,
            git,
            path,
            regex,
            always,
        }
    }
}

impl Auto {
    // Create an Auto from a configuration
    //
    // path is resolved relative to the current working directory
    fn from_config(config: &ConfigAuto, cwd: &Path) -> Self {
        Auto {
            watch: config.watch.unwrap_or(false),
            git: config.git.unwrap_or(false),
            path: config
                .path
                .clone()
                .unwrap_or_default()
                .into_iter()
                .map(|p| cwd.join(p).canonicalize().unwrap())
                .collect(),
            regex: config.regex.clone().unwrap_or_default(),
            always: config.always.unwrap_or(false),
        }
    }
}

/// A single executable task with its configuration and automation rules
///
/// Commands are the leaf nodes in the command tree. Each command has:
/// - A unique identifier
/// - A working directory (inherited from parent group if not specified)
/// - Automation rules (merged with parent group rules)
/// - An executable shell command
///
/// # Examples
///
/// ```python
/// cmd = Command(
///     name="build",
///     cmd="cargo build",
/// )
/// ```
#[derive(Debug, Clone)]
#[gen_stub_pyclass]
#[pyclass]
#[pyo3(get_all)]
pub struct Command {
    id: String,
    name: String,
    cmd: String,
    cwd: PathBuf,
    pub(crate) interactive: bool,
    pub auto: Auto,
}

#[gen_stub_pymethods]
#[pymethods]
impl Command {
    #[new]
    #[pyo3(signature = (name, cmd, id = Uuid::new_v4().to_string(), cwd = PathBuf::from("."), interactive = false, auto = Auto::default()))]
    fn new(
        name: String,
        cmd: String,
        id: String,
        cwd: PathBuf,
        interactive: bool,
        auto: Auto,
    ) -> Self {
        Command {
            id,
            name,
            cmd,
            cwd,
            interactive,
            auto,
        }
    }
    fn __eq__(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Command {
    fn from_config(config: ConfigCommand, parent_cwd: &Path) -> Self {
        Command {
            id: config.id.unwrap_or_else(|| Uuid::new_v4().to_string()),
            name: config.name,
            cmd: config.cmd,
            cwd: config.cwd.unwrap_or_else(|| parent_cwd.to_path_buf()),
            interactive: config.interactive.unwrap_or(false),
            auto: config
                .auto
                .map(|a| Auto::from_config(&a, parent_cwd))
                .unwrap_or_default(),
        }
    }
}

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
#[derive(Debug, Clone)]
#[gen_stub_pyclass]
#[pyclass]
#[pyo3(get_all)]
pub struct CommandGroup {
    id: String,
    name: String,
    auto: Auto,
    cwd: PathBuf,
    commands: Vec<Command>,
    children: Vec<CommandGroup>,
}

#[gen_stub_pymethods]
#[pymethods]
impl CommandGroup {
    #[new]
    #[pyo3(signature = (name, id = Uuid::new_v4().to_string(), auto = Auto::default(), cwd = PathBuf::from("."), commands = Vec::new(), children = Vec::new()))]
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
}

impl CommandGroup {
    /// Returns a flattened list of all commands in this group and its children
    pub fn all_commands(&self) -> Vec<&Command> {
        self.commands
            .iter()
            .chain(self.children.iter().flat_map(|child| child.all_commands()))
            .collect()
    }

    // Create a CommandGroup from a configuration
    pub fn from_config(config: ConfigCommandGroup, parent_cwd: &Path) -> Self {
        let cwd = config.cwd.unwrap_or_else(|| parent_cwd.to_path_buf());

        CommandGroup {
            id: config.id.unwrap_or_else(|| Uuid::new_v4().to_string()),
            name: config.name,
            auto: config
                .auto
                .map(|a| Auto::from_config(&a, &cwd))
                .unwrap_or_default(),
            cwd: cwd.clone(),
            commands: config
                .commands
                .unwrap_or_default()
                .into_iter()
                .map(|cmd| Command::from_config(cmd, &cwd))
                .collect(),
            children: config
                .children
                .unwrap_or_default()
                .into_iter()
                .map(|child| CommandGroup::from_config(child, &cwd))
                .collect(),
        }
    }

    /// Propagate settings recursively to all children
    fn propagate_settings_internal(&self, parent_settings: &InheritableSettings) -> Self {
        // Create current level settings
        let current_settings = InheritableSettings {
            cwd: self.cwd.clone(),
            auto: self.auto.clone(),
        }
        .merge_with_parent(parent_settings);

        // Process children with the new settings
        let children = self
            .children
            .iter()
            .map(|child| child.propagate_settings_internal(&current_settings))
            .collect();

        // Process commands
        let commands = self
            .commands
            .iter()
            .map(|command| {
                let command_settings = InheritableSettings {
                    cwd: command.cwd.clone(),
                    auto: command.auto.clone(),
                }
                .merge_with_parent(&current_settings);

                Command {
                    id: command.id.clone(),
                    name: command.name.clone(),
                    cmd: command.cmd.clone(),
                    cwd: command_settings.cwd,
                    interactive: command.interactive,
                    auto: command_settings.auto,
                }
            })
            .collect();

        CommandGroup {
            id: self.id.clone(),
            name: self.name.clone(),
            cwd: current_settings.cwd,
            auto: current_settings.auto,
            commands,
            children,
        }
    }

    /// Propagates inherited settings from this group to all its children
    ///
    /// Recursively processes the command group hierarchy, ensuring that settings
    /// like working directory and automation rules are properly inherited from
    /// parent to child elements.
    ///
    /// # Returns
    ///
    /// A new CommandGroup with inherited settings applied
    pub fn propagate_settings(&self) -> Self {
        let settings = InheritableSettings {
            cwd: self.cwd.clone(),
            auto: self.auto.clone(),
        };

        self.propagate_settings_internal(&settings)
    }
}
