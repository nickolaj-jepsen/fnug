//! Configuration file handling for Fnug

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use log::{debug, info};
use regex_cache::LazyRegex;
use schemars::JsonSchema;
use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
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
    #[error("Config file not found: {0}")]
    ConfigFileMissing(PathBuf),
    #[error("Unable to read config file {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("Unable to find directory: {path:?} (entry: {entry:?})")]
    DirectoryNotFound {
        entry: String,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("Unknown working directory: {0}")]
    UnknownWorkingDirectory(String),
    #[error("Unable to parse YAML config file {path}: {source}{}", fmt_hint(hint.as_deref()))]
    Yaml {
        source: serde_yaml::Error,
        path: PathBuf,
        hint: Option<String>,
    },
    #[error("Unable to parse JSON config file {path}: {source}{}", fmt_hint(hint.as_deref()))]
    Json {
        source: serde_json::Error,
        path: PathBuf,
        hint: Option<String>,
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
    #[error("Workspace discovery error: {0}")]
    Workspace(String),
}

fn fmt_hint(hint: Option<&str>) -> String {
    hint.map(|h| format!("\n  hint: {h}")).unwrap_or_default()
}

/// Suggest a fix for serde's `unknown field `X`, expected …` message, if it is one.
fn unknown_field_hint(msg: &str) -> Option<String> {
    let rest = msg.split_once("unknown field `")?.1;
    let (field, rest) = rest.split_once('`')?;
    if field == "<<" {
        return Some(
            "YAML merge keys (`<<`) are not supported; put the anchor on the whole value \
             instead (e.g. `auto: *defaults`)"
                .to_string(),
        );
    }
    let expected = rest.split_once("expected ")?.1;
    let wanted = normalize_key(field);
    expected
        .split('`')
        .skip(1)
        .step_by(2)
        .map(|candidate| {
            let distance = strsim::damerau_levenshtein(&wanted, &normalize_key(candidate));
            (distance, candidate)
        })
        .filter(|&(distance, _)| distance <= 2 && distance < field.len())
        .min_by_key(|&(distance, _)| distance)
        .map(|(_, candidate)| format!("did you mean `{candidate}`?"))
}

/// Lowercase `snake_case` form of a key, so `dependsOn` and `depends-on` compare equal to `depends_on`.
fn normalize_key(key: &str) -> String {
    let mut out = String::with_capacity(key.len() + 4);
    for (i, c) in key.chars().enumerate() {
        if c == '-' {
            out.push('_');
        } else if c.is_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.extend(c.to_lowercase());
        } else {
            out.push(c);
        }
    }
    out
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

/// Rules for when a command is selected automatically. Each field is inherited from the
/// parent group unless set here.
#[derive(Debug, Deserialize, Serialize, JsonSchema, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct ConfigAuto {
    /// Select when a watched file under `path` matching `regex` changes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub watch: Option<bool>,
    /// Select when a file under `path` matching `regex` has uncommitted git changes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git: Option<bool>,
    /// Path prefixes, relative to the working directory, that changed files must be under.
    /// Defaults to the working directory.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<Vec<PathBuf>>,
    /// Regular expressions matched against changed file paths; a file must match at least one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub regex: Option<Vec<String>>,
    /// Always select, regardless of changes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub always: Option<bool>,
    /// Set to `false` to skip in `fnug check`, git hooks and MCP runs (default `true`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub check: Option<bool>,
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
            check: config.check,
        })
    }
}

/// A command to run.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConfigCommand {
    /// Identifier used by `depends_on` and the MCP tools.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Display name.
    pub name: String,
    /// Working directory, relative to the parent group's.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    /// Shell command, run with `sh -c`.
    pub cmd: String,
    /// Auto-selection rules.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto: Option<ConfigAuto>,
    /// Extra environment variables, added to the inherited ones.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<HashMap<String, String>>,
    /// Ids of commands that must finish successfully before this one runs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub depends_on: Option<Vec<String>>,
    /// Number of scrollback lines kept for the command's terminal.
    #[serde(skip_serializing_if = "Option::is_none")]
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

/// Where to look for workspace package configs.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceOptions {
    /// Glob patterns for package directories, relative to this config. Without it, fnug walks
    /// the directory tree, skipping hidden and gitignored directories.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paths: Option<Vec<String>>,
    /// How many directory levels the walk descends (default 5).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_depth: Option<usize>,
}

/// Workspace mode: `true` to discover package configs in subdirectories, or options.
// Deserialize is hand-written: `untagged` would replace field errors with "did not match any variant".
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(untagged)]
pub enum WorkspaceConfig {
    Enabled(bool),
    Options(WorkspaceOptions),
}

impl<'de> Deserialize<'de> for WorkspaceConfig {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct WorkspaceVisitor;

        impl<'de> Visitor<'de> for WorkspaceVisitor {
            type Value = WorkspaceConfig;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a boolean or a map with `paths` and/or `max_depth`")
            }

            fn visit_bool<E: de::Error>(self, v: bool) -> Result<Self::Value, E> {
                Ok(WorkspaceConfig::Enabled(v))
            }

            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
                WorkspaceOptions::deserialize(de::value::MapAccessDeserializer::new(map))
                    .map(WorkspaceConfig::Options)
            }
        }

        deserializer.deserialize_any(WorkspaceVisitor)
    }
}

/// A group of commands and nested groups that share settings.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConfigCommandGroup {
    /// Identifier for the group.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Display name.
    pub name: String,
    /// Default auto-selection rules for everything in the group.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto: Option<ConfigAuto>,
    /// Working directory, relative to the parent group's.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    /// Commands in the group.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commands: Option<Vec<ConfigCommand>>,
    /// Nested groups.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub children: Option<Vec<ConfigCommandGroup>>,
    /// Environment variables for everything in the group, added to the inherited ones.
    #[serde(skip_serializing_if = "Option::is_none")]
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

/// A fnug config file (`.fnug.yaml`, `.fnug.yml` or `.fnug.json`). The file is the root group,
/// plus file-level settings.
// Not `#[serde(flatten)]` over `ConfigCommandGroup`: flatten hides unknown keys and error locations.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(title = "fnug config")]
pub struct Config {
    /// JSON Schema reference for editors; ignored by fnug.
    #[serde(rename = "$schema", skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    /// fnug version the config is written for. fnug warns if it needs a newer fnug or targets
    /// an older release series.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fnug_version: Option<String>,
    /// Discover and merge package configs from subdirectories.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<WorkspaceConfig>,
    /// Identifier for the root group.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Display name for the root group.
    pub name: String,
    /// Default auto-selection rules for every command.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto: Option<ConfigAuto>,
    /// Working directory, relative to this file's directory.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    /// Top-level commands.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commands: Option<Vec<ConfigCommand>>,
    /// Command groups.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub children: Option<Vec<ConfigCommandGroup>>,
    /// Environment variables for every command.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<HashMap<String, String>>,
}

/// List of supported configuration file names
pub(crate) const FILENAMES: [&str; 3] = [".fnug.json", ".fnug.yaml", ".fnug.yml"];

/// Find a config file in a specific directory, returning the first match.
pub(crate) fn find_config_in_dir(dir: &Path) -> Option<PathBuf> {
    FILENAMES.iter().map(|f| dir.join(f)).find(|p| p.exists())
}

impl Config {
    /// Split into the root command group and the workspace setting.
    #[must_use]
    pub fn into_root(self) -> (ConfigCommandGroup, Option<WorkspaceConfig>) {
        let root = ConfigCommandGroup {
            id: self.id,
            name: self.name,
            auto: self.auto,
            cwd: self.cwd,
            commands: self.commands,
            children: self.children,
            env: self.env,
        };
        (root, self.workspace)
    }

    /// Loads and parses a configuration file.
    ///
    /// # Errors
    ///
    /// Returns `ConfigError::Io` if the file cannot be read, or
    /// `ConfigError::Yaml`/`ConfigError::Json` if parsing fails.
    pub fn from_file(file: &Path) -> Result<Config, ConfigError> {
        let contents = std::fs::read_to_string(file).map_err(|e| ConfigError::Io {
            path: file.to_path_buf(),
            source: e,
        })?;
        let config: Config = if file.extension().is_some_and(|ext| ext == "json") {
            serde_json::from_str(&contents).map_err(|e| ConfigError::Json {
                hint: unknown_field_hint(&e.to_string()),
                source: e,
                path: file.to_path_buf(),
            })?
        } else {
            serde_yaml::from_str(&contents).map_err(|e| ConfigError::Yaml {
                hint: unknown_field_hint(&e.to_string()),
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
        assert_eq!(config.name, "root");
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
        assert_eq!(config.name, "root");
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
