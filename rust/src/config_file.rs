use log::trace;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum ConfigError {
    #[error("Unable to read config file")]
    Io(#[from] std::io::Error),
    #[error("Unable to parse config file")]
    Serde(#[from] serde_yaml::Error),
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ConfigCommandGroup {
    pub id: Option<String>,
    pub name: String,
    pub auto: Option<ConfigAuto>,
    pub cwd: Option<PathBuf>,
    pub commands: Option<Vec<ConfigCommand>>,
    pub children: Option<Vec<ConfigCommandGroup>>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ConfigAuto {
    pub watch: Option<bool>,
    pub git: Option<bool>,
    pub path: Option<Vec<PathBuf>>,
    pub regex: Option<Vec<String>>,
    pub always: Option<bool>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ConfigCommand {
    pub id: Option<String>,
    pub name: String,
    pub cwd: Option<PathBuf>,
    pub cmd: String,
    pub interactive: Option<bool>,
    pub auto: Option<ConfigAuto>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Config {
    fnug_version: String,
    #[serde(flatten)]
    pub root: ConfigCommandGroup,
}

const FILENAMES: [&str; 3] = [".fnug.json", ".fnug.yaml", ".fnug.yml"];

impl Config {
    pub fn from_file(file: &PathBuf) -> Result<Config, ConfigError> {
        let file = std::fs::read_to_string(file).map_err(ConfigError::Io)?;
        let config: Config = serde_yaml::from_str(&file).map_err(ConfigError::Serde)?;
        Ok(config)
    }

    pub fn find_config() -> Result<PathBuf, ConfigError> {
        let mut path = std::env::current_dir().map_err(ConfigError::Io)?;
        loop {
            trace!("{:?}", path);
            for filename in FILENAMES.iter() {
                let file = path
                    .join(filename)
                    .canonicalize()
                    .map_err(ConfigError::Io)?;
                if file.exists() {
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
