//! Configuration file handling for Fnug
//!
//! This module handles loading and parsing of Fnug configuration files in YAML/JSON format.
//! It supports a hierarchical configuration structure that defines commands, groups,
//! and their automation rules.

use crate::commands::auto::Auto;
use crate::commands::command::Command;
use crate::commands::group::CommandGroup;
use log::{debug, info};
use pyo3::PyErr;
use regex_cache::LazyRegex;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use thiserror::Error;
use uuid::Uuid;

/// Errors that can occur while loading configuration
#[derive(Error, Debug)]
pub enum ConfigError {
    #[error("No config file found in current directory or its parents: {0}")]
    ConfigNotFound(PathBuf),
    #[error("Unable to find directory: {path:?} (entry: {entry:?})")]
    DirectoryNotFound { entry: String, path: PathBuf },
    #[error("Unknown working directory: {0}")]
    UnknownWorkingDirectory(String),
    #[error("Unable to parse config file")]
    Serde(#[from] serde_yaml::Error),
    #[error("Invalid regex pattern: {0}")]
    Regex(#[from] regex::Error),
}

impl From<ConfigError> for PyErr {
    fn from(error: ConfigError) -> Self {
        match error {
            ConfigError::Serde(err) => pyo3::exceptions::PyValueError::new_err(format!(
                "Error parsing config file: {:?}",
                err
            )),
            ConfigError::UnknownWorkingDirectory(path) => pyo3::exceptions::PyValueError::new_err(
                format!("Unknown working directory: {:?}", path),
            ),
            ConfigError::Regex(err) => pyo3::exceptions::PyValueError::new_err(format!(
                "Error parsing regex in config file: {:?}",
                err
            )),
            ConfigError::ConfigNotFound(path) => {
                pyo3::exceptions::PyFileNotFoundError::new_err(format!(
                    "No config file found in current directory or its parents: {:?}",
                    path
                ))
            }
            ConfigError::DirectoryNotFound { path, entry } => {
                pyo3::exceptions::PyFileNotFoundError::new_err(format!(
                    "Unable to find directory: {:?} (entry: {:?})",
                    path, entry
                ))
            }
        }
    }
}

pub fn parse_regexes(regex: Vec<String>) -> Result<Vec<LazyRegex>, ConfigError> {
    regex
        .into_iter()
        .map(|r| LazyRegex::new(&r).map_err(ConfigError::Regex))
        .collect()
}

/// Configuration for automatic command execution
///
/// # Example
///
/// ```yaml
/// name: fnug
/// commands:
///  - name: test
///    cmd: cargo test
///    auto:
///     watch: true
///     git: true
///     path:
///      - src/
///      - tests/
///     regex:
///      - '.*\\.rs$'
///      - '.*\\.toml$'
/// ```

#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct ConfigAuto {
    pub watch: Option<bool>,
    pub git: Option<bool>,
    pub path: Option<Vec<PathBuf>>,
    pub regex: Option<Vec<String>>,
    pub always: Option<bool>,
}

impl From<Auto> for ConfigAuto {
    fn from(auto: Auto) -> Self {
        ConfigAuto {
            watch: auto.watch,
            git: auto.git,
            path: Some(auto.path),
            regex: Some(auto.regex.iter().map(|r| r.to_string()).collect()),
            always: auto.always,
        }
    }
}

impl TryInto<Auto> for ConfigAuto {
    type Error = ConfigError;

    fn try_into(self) -> Result<Auto, Self::Error> {
        let regex = self.regex.map(parse_regexes).unwrap_or(Ok(Vec::new()))?;
        Ok(Auto {
            regex,
            watch: self.watch,
            git: self.git,
            path: self.path.unwrap_or_default(),
            always: self.always,
        })
    }
}

/// Configuration for a single command
///
/// # Example
///
/// ```yaml
/// name: fnug
/// commands:
/// - name: build
///   cmd: cargo build
/// ```
#[derive(Debug, Deserialize, Serialize)]
pub struct ConfigCommand {
    pub id: Option<String>,
    pub name: String,
    pub cwd: Option<PathBuf>,
    pub cmd: String,
    pub interactive: Option<bool>,
    pub auto: Option<ConfigAuto>,
}

impl From<Command> for ConfigCommand {
    fn from(command: Command) -> Self {
        ConfigCommand {
            id: Some(command.id),
            name: command.name,
            cwd: Some(command.cwd),
            cmd: command.cmd,
            interactive: Some(command.interactive),
            auto: Some(command.auto.into()),
        }
    }
}

impl TryInto<Command> for ConfigCommand {
    type Error = ConfigError;

    fn try_into(self) -> Result<Command, Self::Error> {
        Ok(Command {
            cwd: self.cwd.unwrap_or_default(),
            auto: self.auto.unwrap_or_default().try_into()?,
            cmd: self.cmd,
            id: self.id.unwrap_or_else(|| Uuid::new_v4().to_string()),
            name: self.name,
            interactive: self.interactive.unwrap_or(false),
        })
    }
}

/// Configuration for a group of commands
#[derive(Debug, Deserialize, Serialize)]
pub struct ConfigCommandGroup {
    pub id: Option<String>,
    pub name: String,
    pub auto: Option<ConfigAuto>,
    pub cwd: Option<PathBuf>,
    pub commands: Option<Vec<ConfigCommand>>,
    pub children: Option<Vec<ConfigCommandGroup>>,
}

impl From<CommandGroup> for ConfigCommandGroup {
    fn from(group: CommandGroup) -> Self {
        ConfigCommandGroup {
            id: Some(group.id),
            name: group.name,
            auto: Some(group.auto.into()),
            cwd: Some(group.cwd),
            commands: Some(group.commands.into_iter().map(Into::into).collect()),
            children: Some(group.children.into_iter().map(Into::into).collect()),
        }
    }
}

impl TryInto<CommandGroup> for ConfigCommandGroup {
    type Error = ConfigError;

    fn try_into(self) -> Result<CommandGroup, Self::Error> {
        let children = self
            .children
            .unwrap_or_default()
            .into_iter()
            .map(|c| c.try_into())
            .collect::<Result<Vec<CommandGroup>, ConfigError>>()?;
        let commands = self
            .commands
            .unwrap_or_default()
            .into_iter()
            .map(|c| c.try_into())
            .collect::<Result<Vec<Command>, ConfigError>>()?;
        Ok(CommandGroup {
            id: self.id.unwrap_or_else(|| Uuid::new_v4().to_string()),
            name: self.name,
            auto: self.auto.unwrap_or_default().try_into()?,
            cwd: self.cwd.unwrap_or_default(),
            commands,
            children,
        })
    }
}

impl ConfigCommandGroup {
    pub fn as_yaml(&self) -> Result<String, ConfigError> {
        serde_yaml::to_string(self).map_err(ConfigError::Serde)
    }
}

/// Root configuration structure for Fnug
///
/// This struct basically "inherits" from `ConfigCommandGroup`, but making serde
/// flatten the fields into the root config struct.
///
///
/// # Example Configuration
///
/// ```yaml
/// fnug_version: "1.0"
/// name: project
/// commands:
///   - name: build
///     cmd: make all
/// children:
///   - name: backend
///     commands:
///       - name: test
///         cmd: cargo test
/// ```
#[derive(Debug, Deserialize, Serialize)]
pub struct Config {
    fnug_version: String,
    #[serde(flatten)]
    pub root: ConfigCommandGroup,
}

/// List of supported configuration file names
const FILENAMES: [&str; 3] = [".fnug.json", ".fnug.yaml", ".fnug.yml"];

impl Config {
    /// Loads and parses a configuration file
    ///
    /// # Arguments
    ///
    /// * `file` - Path to the configuration file
    ///
    /// # Errors
    ///
    /// * `ConfigError::Io` if the file cannot be read
    /// * `ConfigError::Serde` if the file contains invalid YAML/JSON
    ///
    /// ```
    pub fn from_file(file: &PathBuf) -> Result<Config, ConfigError> {
        let file =
            std::fs::read_to_string(file).map_err(|_| ConfigError::ConfigNotFound(file.clone()))?;
        let config: Config = serde_yaml::from_str(&file)?;
        Ok(config)
    }

    /// Searches for a configuration file in the current directory and its parents
    ///
    /// Looks for files named `.fnug.json`, `.fnug.yaml`, or `.fnug.yml` starting
    /// in the current directory and moving up through parent directories until
    /// one is found.
    ///
    /// # Errors
    ///
    /// * `ConfigError::Io` if no configuration file is found
    ///
    /// ```
    pub fn find_config() -> Result<PathBuf, ConfigError> {
        let config_path = std::env::current_dir()
            .map_err(|e| ConfigError::UnknownWorkingDirectory(e.to_string()))?;
        let mut path = config_path.clone();
        debug!("Searching for config file in {:?}", config_path);
        loop {
            for file in &FILENAMES {
                let config_path = path.join(file);
                if config_path.exists() {
                    info!("Found config file: {:?}", config_path);
                    return Ok(config_path);
                }
            }
            if !path.pop() {
                return Err(ConfigError::ConfigNotFound(config_path));
            }
        }
    }
}
