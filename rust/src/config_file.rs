//! Configuration file handling for Fnug
//!
//! This module handles loading and parsing of Fnug configuration files in YAML/JSON format.
//! It supports a hierarchical configuration structure that defines commands, groups,
//! and their automation rules.

use log::trace;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use thiserror::Error;

/// Errors that can occur while loading configuration
#[derive(Error, Debug)]
pub enum ConfigError {
    /// File system errors when reading config files
    #[error("Unable to read config file")]
    Io(#[from] std::io::Error),
    /// Parse errors in YAML/JSON content
    #[error("Unable to parse config file")]
    Serde(#[from] serde_yaml::Error),
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

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ConfigAuto {
    pub watch: Option<bool>,
    pub git: Option<bool>,
    pub path: Option<Vec<PathBuf>>,
    pub regex: Option<Vec<String>>,
    pub always: Option<bool>,
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
    /// # Example
    ///
    /// ```rust
    /// use std::path::PathBuf;
    /// let config = Config::from_file(&PathBuf::from(".fnug.yaml"))?;
    /// ```
    pub fn from_file(file: &PathBuf) -> Result<Config, ConfigError> {
        let file = std::fs::read_to_string(file).map_err(ConfigError::Io)?;
        let config: Config = serde_yaml::from_str(&file).map_err(ConfigError::Serde)?;
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
    /// # Example
    ///
    /// ```rust
    /// let config_path = Config::find_config()?;
    /// let config = Config::from_file(&config_path)?;
    /// ```
    pub fn find_config() -> Result<PathBuf, ConfigError> {
        let mut path = std::env::current_dir().map_err(ConfigError::Io)?;
        loop {
            for filename in FILENAMES.iter() {
                let file = path.join(filename);
                if file.exists() {
                    trace!("Found config file: {:?}", file);
                    return Ok(file);
                }
            }

            if !path.pop() {
                break;
            }
        }
        Err(ConfigError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "fnug.yaml",
        )))
    }
}
