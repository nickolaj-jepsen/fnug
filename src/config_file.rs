//! Configuration file handling for Fnug

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use log::{debug, info};
use regex_cache::LazyRegex;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::commands::auto::Auto;
use crate::commands::command::Command;
use crate::commands::group::CommandGroup;

/// Errors that can occur while loading configuration
#[derive(Error, Debug)]
pub enum ConfigError {
    #[error("No config file found in current directory or its parents: {0}")]
    ConfigNotFound(PathBuf),
    #[error("Unable to find directory: {path:?} (entry: {entry:?})")]
    DirectoryNotFound {
        entry: String,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("Unknown working directory: {0}")]
    UnknownWorkingDirectory(String),
    #[error("Unable to parse YAML config file {path}: {source}")]
    Yaml {
        source: serde_yaml::Error,
        path: PathBuf,
    },
    #[error("Unable to parse JSON config file {path}: {source}")]
    Json {
        source: serde_json::Error,
        path: PathBuf,
    },
    #[error("Invalid regex pattern `{pattern}`: {source}")]
    Regex {
        source: regex::Error,
        pattern: String,
    },
    #[error("Duplicate ID in config: {0}")]
    DuplicateId(String),
    #[error("Invalid config: {0}")]
    Validation(String),
}

/// Parse a list of regex pattern strings into compiled regexes.
///
/// # Errors
///
/// Returns `ConfigError::Regex` if any pattern fails to compile.
pub fn parse_regexes(regex: Vec<String>) -> Result<Vec<LazyRegex>, ConfigError> {
    regex
        .into_iter()
        .map(|r| {
            LazyRegex::new(&r).map_err(|e| ConfigError::Regex {
                source: e,
                pattern: r,
            })
        })
        .collect()
}

/// Configuration for automatic command execution
#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct ConfigAuto {
    pub watch: Option<bool>,
    pub git: Option<bool>,
    pub path: Option<Vec<PathBuf>>,
    pub regex: Option<Vec<String>>,
    pub always: Option<bool>,
}

impl TryFrom<ConfigAuto> for Auto {
    type Error = ConfigError;

    fn try_from(config: ConfigAuto) -> Result<Self, Self::Error> {
        let regex = config.regex.map_or(Ok(Vec::new()), parse_regexes)?;
        Ok(Auto {
            regex,
            watch: config.watch,
            git: config.git,
            path: config.path.unwrap_or_default(),
            always: config.always,
        })
    }
}

/// Configuration for a single command
#[derive(Debug, Deserialize, Serialize)]
pub struct ConfigCommand {
    pub id: Option<String>,
    pub name: String,
    pub cwd: Option<PathBuf>,
    pub cmd: String,
    pub auto: Option<ConfigAuto>,
    pub env: Option<HashMap<String, String>>,
    pub depends_on: Option<Vec<String>>,
    pub scrollback: Option<usize>,
}

impl TryFrom<ConfigCommand> for Command {
    type Error = ConfigError;

    fn try_from(config: ConfigCommand) -> Result<Self, Self::Error> {
        Ok(Command {
            cwd: config.cwd.unwrap_or_default(),
            auto: config.auto.unwrap_or_default().try_into()?,
            cmd: config.cmd,
            id: config.id.unwrap_or_else(|| Uuid::new_v4().to_string()),
            name: config.name,
            env: config.env.unwrap_or_default(),
            depends_on: config.depends_on.unwrap_or_default(),
            scrollback: config.scrollback,
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
    pub env: Option<HashMap<String, String>>,
}

impl TryFrom<ConfigCommandGroup> for CommandGroup {
    type Error = ConfigError;

    fn try_from(config: ConfigCommandGroup) -> Result<Self, Self::Error> {
        let children = config
            .children
            .unwrap_or_default()
            .into_iter()
            .map(CommandGroup::try_from)
            .collect::<Result<Vec<CommandGroup>, ConfigError>>()?;
        let commands = config
            .commands
            .unwrap_or_default()
            .into_iter()
            .map(Command::try_from)
            .collect::<Result<Vec<Command>, ConfigError>>()?;
        Ok(CommandGroup {
            id: config.id.unwrap_or_else(|| Uuid::new_v4().to_string()),
            name: config.name,
            auto: config.auto.unwrap_or_default().try_into()?,
            cwd: config.cwd.unwrap_or_default(),
            commands,
            children,
            env: config.env.unwrap_or_default(),
        })
    }
}

/// Root configuration structure for Fnug
#[derive(Debug, Deserialize, Serialize)]
pub struct Config {
    pub fnug_version: String,
    #[serde(flatten)]
    pub root: ConfigCommandGroup,
}

/// List of supported configuration file names
const FILENAMES: [&str; 3] = [".fnug.json", ".fnug.yaml", ".fnug.yml"];

impl Config {
    /// Loads and parses a configuration file.
    ///
    /// # Errors
    ///
    /// Returns `ConfigError::ConfigNotFound` if the file cannot be read, or
    /// `ConfigError::Yaml`/`ConfigError::Json` if parsing fails.
    pub fn from_file(file: &Path) -> Result<Config, ConfigError> {
        let contents = std::fs::read_to_string(file)
            .map_err(|_| ConfigError::ConfigNotFound(file.to_path_buf()))?;
        let config: Config = if file.extension().is_some_and(|ext| ext == "json") {
            serde_json::from_str(&contents).map_err(|e| ConfigError::Json {
                source: e,
                path: file.to_path_buf(),
            })?
        } else {
            serde_yaml::from_str(&contents).map_err(|e| ConfigError::Yaml {
                source: e,
                path: file.to_path_buf(),
            })?
        };
        Ok(config)
    }

    /// Searches for a configuration file in the current directory and its parents.
    ///
    /// # Errors
    ///
    /// Returns `ConfigError::UnknownWorkingDirectory` if the cwd cannot be determined,
    /// or `ConfigError::ConfigNotFound` if no config file is found.
    pub fn find_config() -> Result<PathBuf, ConfigError> {
        let config_path = std::env::current_dir()
            .map_err(|e| ConfigError::UnknownWorkingDirectory(e.to_string()))?;
        let mut path = config_path.clone();
        debug!("Searching for config file in {}", config_path.display());
        loop {
            for file in &FILENAMES {
                let config_path = path.join(file);
                if config_path.exists() {
                    info!("Found config file: {}", config_path.display());
                    return Ok(config_path);
                }
            }
            if !path.pop() {
                return Err(ConfigError::ConfigNotFound(config_path));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_from_file_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".fnug.json");
        std::fs::write(
            &path,
            r#"{
                "fnug_version": "0.0.27",
                "name": "root",
                "id": "root",
                "commands": [{"name": "test", "cmd": "echo hello"}]
            }"#,
        )
        .unwrap();
        let config = Config::from_file(&path).unwrap();
        assert_eq!(config.root.name, "root");
    }

    #[test]
    fn test_from_file_yaml() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".fnug.yaml");
        std::fs::write(
            &path,
            "fnug_version: '0.0.27'\nname: root\nid: root\ncommands:\n  - name: test\n    cmd: echo hello\n",
        )
        .unwrap();
        let config = Config::from_file(&path).unwrap();
        assert_eq!(config.root.name, "root");
    }

    #[test]
    fn test_regex_error_preserves_pattern() {
        let result = parse_regexes(vec!["[invalid".to_string()]);
        match result {
            Err(ConfigError::Regex { pattern, .. }) => {
                assert_eq!(pattern, "[invalid");
            }
            other => panic!("Expected ConfigError::Regex, got: {other:?}"),
        }
    }
}
