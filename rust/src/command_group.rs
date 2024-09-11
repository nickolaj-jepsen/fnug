use crate::config_file::{ConfigAuto, ConfigCommand, ConfigCommandGroup};
use pyo3::pyclass;
use pyo3_stub_gen::derive::gen_stub_pyclass;
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[gen_stub_pyclass]
#[derive(Debug, Clone, Default)]
#[pyclass]
#[pyo3(get_all)]
pub struct Auto {
    watch: bool,
    git: bool,
    path: Vec<PathBuf>,
    regex: Vec<String>,
    always: bool,
}

#[gen_stub_pyclass]
#[derive(Debug, Clone)]
#[pyclass]
#[pyo3(get_all)]
struct Command {
    id: String,
    name: String,
    cmd: String,
    cwd: PathBuf,
    interactive: bool,
    auto: Auto,
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

fn build_auto(config: Option<ConfigAuto>, parent_auto: &Auto) -> Auto {
    if let Some(config) = config {
        Auto {
            watch: config.watch.unwrap_or(parent_auto.watch),
            git: config.git.unwrap_or(parent_auto.git),
            path: config.path.unwrap_or_else(|| parent_auto.path.clone()),
            regex: config.regex.unwrap_or_else(|| parent_auto.regex.clone()),
            always: config.always.unwrap_or(parent_auto.always),
        }
    } else {
        parent_auto.clone()
    }
}

fn build_command(config: ConfigCommand, parent_cwd: &Path, parent_auto: &Auto) -> Command {
    let auto = build_auto(config.auto, parent_auto);
    Command {
        id: config.id.unwrap_or_else(|| Uuid::new_v4().to_string()),
        name: config.name,
        cwd: config.cwd.unwrap_or_else(|| parent_cwd.to_path_buf()),
        cmd: config.cmd,
        interactive: config.interactive.unwrap_or(false),
        auto,
    }
}

pub fn build_command_group(
    config: ConfigCommandGroup,
    parent_cwd: &Path,
    parent_auto: Option<&Auto>,
) -> CommandGroup {
    let cwd = config.cwd.unwrap_or_else(|| parent_cwd.to_path_buf());
    let auto = build_auto(config.auto, parent_auto.unwrap_or(&Auto::default()));
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
