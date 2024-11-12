use crate::config_file::{ConfigAuto, ConfigCommand, ConfigCommandGroup};
use pyo3::{pyclass, pymethods};
use pyo3_stub_gen::derive::{gen_stub_pyclass, gen_stub_pymethods};
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[gen_stub_pyclass]
#[derive(Debug, Clone)]
#[pyclass]
#[pyo3(get_all)]
#[derive(Default)]
pub struct Auto {
    pub watch: bool,
    pub git: bool,
    pub path: Vec<PathBuf>,
    pub regex: Vec<String>,
    pub always: bool,
}

#[gen_stub_pyclass]
#[derive(Debug, Clone)]
#[pyclass]
#[pyo3(get_all)]
pub struct Command {
    id: String,
    name: String,
    cmd: String,
    cwd: PathBuf,
    interactive: bool,
    pub auto: Auto,
}

#[gen_stub_pyclass]
#[derive(Debug, Clone)]
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

#[gen_stub_pymethods]
#[pymethods]
impl CommandGroup {
    #[new]
    #[pyo3(signature = (name, id = Uuid::new_v4().to_string(), auto = Auto::default(), cwd = PathBuf::from("."), commands = Vec::new(), children = Vec::new())
    )]
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
    pub fn all_commands(&self) -> Vec<&Command> {
        let mut commands = self.commands.iter().collect::<Vec<_>>();
        for child in &self.children {
            commands.extend(child.all_commands());
        }
        commands
    }
}

fn build_auto(config: Option<ConfigAuto>, command_cwd: &Path, parent_auto: &Auto) -> Auto {
    if let Some(config) = config {
        // Combine the paths with the command cwd
        let path = config
            .path
            .unwrap_or_else(|| parent_auto.path.clone())
            .into_iter()
            .map(|path| command_cwd.join(path).canonicalize().unwrap())
            .collect();

        Auto {
            watch: config.watch.unwrap_or(parent_auto.watch),
            git: config.git.unwrap_or(parent_auto.git),
            regex: config.regex.unwrap_or_else(|| parent_auto.regex.clone()),
            always: config.always.unwrap_or(parent_auto.always),
            path,
        }
    } else {
        parent_auto.clone()
    }
}

fn merge_auto(auto: Option<Auto>, parent_auto: &Auto) -> Auto {
    if let Some(auto) = auto {
        let regex = if auto.regex.is_empty() {
            parent_auto.regex.clone()
        } else {
            auto.regex
        };
        let path = if auto.path.is_empty() {
            parent_auto.path.clone()
        } else {
            auto.path
        };
        Auto {
            watch: auto.watch || parent_auto.watch,
            git: auto.git || parent_auto.git,
            regex,
            always: auto.always || parent_auto.always,
            path,
        }
    } else {
        parent_auto.clone()
    }
}

fn build_command(config: ConfigCommand, parent_cwd: &Path, parent_auto: &Auto) -> Command {
    let cwd = config.cwd.unwrap_or_else(|| parent_cwd.to_path_buf());
    let auto = build_auto(config.auto, &cwd, parent_auto);
    Command {
        id: config.id.unwrap_or_else(|| Uuid::new_v4().to_string()),
        name: config.name,
        cmd: config.cmd,
        interactive: config.interactive.unwrap_or(false),
        cwd,
        auto,
    }
}

pub fn build_command_group(
    config: ConfigCommandGroup,
    parent_cwd: &Path,
    parent_auto: Option<&Auto>,
) -> CommandGroup {
    let cwd = config.cwd.unwrap_or_else(|| parent_cwd.to_path_buf());
    let auto = build_auto(config.auto, &cwd, parent_auto.unwrap_or(&Auto::default()));
    let children = config
        .children
        .unwrap_or_default()
        .into_iter()
        .map(|child| build_command_group(child, &cwd, Some(&auto)))
        .collect();
    let commands = config
        .commands
        .unwrap_or_default()
        .into_iter()
        .map(|command| build_command(command, &cwd, &auto))
        .collect();

    CommandGroup {
        id: config.id.unwrap_or_else(|| Uuid::new_v4().to_string()),
        name: config.name,
        cwd,
        auto,
        commands,
        children,
    }
}

pub fn fix_command_group(
    command_group: CommandGroup,
    parent_cwd: &Path,
    parent_auto: Option<&Auto>,
) -> CommandGroup {
    let cwd = if command_group.cwd == PathBuf::from(".") {
        parent_cwd.to_path_buf()
    } else {
        command_group.cwd
    };

    let auto = merge_auto(
        Some(command_group.auto.clone()),
        parent_auto.unwrap_or(&Auto::default()),
    );
    let children = command_group
        .children
        .into_iter()
        .map(|child| fix_command_group(child, &cwd, Some(&auto)))
        .collect();
    let commands = command_group
        .commands
        .into_iter()
        .map(|command| {
            let auto = merge_auto(Some(command.auto.clone()), &auto);
            Command {
                id: command.id,
                name: command.name,
                cmd: command.cmd,
                cwd: command.cwd,
                interactive: command.interactive,
                auto,
            }
        })
        .collect();

    CommandGroup {
        id: command_group.id,
        name: command_group.name,
        cwd,
        auto,
        commands,
        children,
    }
}
