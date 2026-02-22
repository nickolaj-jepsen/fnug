//! Core implementation of the Fnug command scheduler
//!
//! Fnug is a command scheduler that detects and executes commands based on file system
//! and git changes. It allows users to define commands and command groups in a configuration
//! file, with flexible automation rules for when commands should be executed.

use std::collections::HashSet;
use std::path::PathBuf;

use log::{debug, warn};

use crate::commands::command::Command;
use crate::commands::group::CommandGroup;
use crate::commands::inherit::{Inheritable, Inheritance};
use crate::config_file::{Config, ConfigError};

pub mod check;
pub mod commands;
pub mod config_file;
pub mod init_hooks;
pub mod logger;
pub mod pty;
pub mod selectors;
pub mod theme;
pub mod tui;

/// Load configuration from a file (or auto-detect), returning the root `CommandGroup`, cwd, and config file path.
///
/// # Errors
///
/// Returns `ConfigError` if the config file is not found, cannot be parsed,
/// contains invalid values, or references non-existent directories.
pub fn load_config(
    config_file: Option<&str>,
) -> Result<(CommandGroup, PathBuf, PathBuf), ConfigError> {
    let config_path = match config_file {
        Some(file) => {
            let config_path = PathBuf::from(file);
            if !config_path.exists() {
                return Err(ConfigError::ConfigNotFound(config_path));
            }
            config_path
        }
        None => Config::find_config()?,
    };
    let cwd = config_path
        .parent()
        .ok_or_else(|| ConfigError::ConfigNotFound(config_path.clone()))?
        .to_path_buf();
    debug!(
        "Creating core from config file: {} (cwd: {})",
        config_path.display(),
        cwd.display()
    );
    let parsed = Config::from_file(&config_path)?;
    validate_version(&parsed.fnug_version);
    let mut config: CommandGroup = parsed.root.try_into()?;
    validate_tree(&config)?;
    validate_dependencies(&config)?;
    config.inherit(&Inheritance::from(cwd.clone()))?;
    Ok((config, cwd, config_path))
}

/// Warn if the config's `fnug_version` doesn't match the binary version
fn validate_version(config_version: &str) {
    let binary_version = env!("CARGO_PKG_VERSION");
    if config_version != binary_version {
        warn!(
            "Config fnug_version '{config_version}' differs from binary version '{binary_version}'"
        );
    }
}

/// Validate the config tree for duplicate IDs, empty groups, and invalid values
fn validate_tree(root: &CommandGroup) -> Result<(), ConfigError> {
    let mut seen_ids = HashSet::new();
    check_duplicates(root, &mut seen_ids)?;
    check_empty_names(root)?;
    check_empty_commands(root)?;
    check_empty_groups(root);
    Ok(())
}

fn check_duplicates(group: &CommandGroup, seen: &mut HashSet<String>) -> Result<(), ConfigError> {
    if !seen.insert(group.id.clone()) {
        return Err(ConfigError::DuplicateId(group.id.clone()));
    }
    for cmd in &group.commands {
        if !seen.insert(cmd.id.clone()) {
            return Err(ConfigError::DuplicateId(cmd.id.clone()));
        }
    }
    for child in &group.children {
        check_duplicates(child, seen)?;
    }
    Ok(())
}

fn check_empty_names(group: &CommandGroup) -> Result<(), ConfigError> {
    if group.name.trim().is_empty() {
        return Err(ConfigError::Validation(format!(
            "Group with id '{}' has an empty name",
            group.id
        )));
    }
    for cmd in &group.commands {
        if cmd.name.trim().is_empty() {
            return Err(ConfigError::Validation(format!(
                "Command with id '{}' has an empty name",
                cmd.id
            )));
        }
    }
    for child in &group.children {
        check_empty_names(child)?;
    }
    Ok(())
}

fn check_empty_commands(group: &CommandGroup) -> Result<(), ConfigError> {
    for cmd in &group.commands {
        if cmd.cmd.trim().is_empty() {
            return Err(ConfigError::Validation(format!(
                "Command '{}' has an empty cmd string",
                cmd.name
            )));
        }
    }
    for child in &group.children {
        check_empty_commands(child)?;
    }
    Ok(())
}

/// Validate that all `depends_on` references resolve and there are no cycles
fn validate_dependencies(root: &CommandGroup) -> Result<(), ConfigError> {
    // Collect all command IDs
    let mut all_ids = HashSet::new();
    collect_command_ids(root, &mut all_ids);

    // Validate references
    for cmd in root.all_commands() {
        for dep in &cmd.depends_on {
            if !all_ids.contains(dep.as_str()) {
                return Err(ConfigError::Validation(format!(
                    "Command '{}' depends on '{}' which does not exist",
                    cmd.name, dep
                )));
            }
        }
    }

    // Cycle detection via DFS
    let commands: Vec<&Command> = root.all_commands();
    let mut visited = HashSet::new();
    let mut stack = HashSet::new();
    for cmd in &commands {
        if !visited.contains(cmd.id.as_str()) {
            detect_cycle(cmd.id.as_str(), &commands, &mut visited, &mut stack)?;
        }
    }

    Ok(())
}

fn collect_command_ids<'a>(group: &'a CommandGroup, ids: &mut HashSet<&'a str>) {
    for cmd in &group.commands {
        ids.insert(&cmd.id);
    }
    for child in &group.children {
        collect_command_ids(child, ids);
    }
}

fn detect_cycle<'a>(
    id: &'a str,
    commands: &[&'a Command],
    visited: &mut HashSet<&'a str>,
    stack: &mut HashSet<&'a str>,
) -> Result<(), ConfigError> {
    visited.insert(id);
    stack.insert(id);

    if let Some(cmd) = commands.iter().find(|c| c.id == id) {
        for dep in &cmd.depends_on {
            let dep_str: &str = dep.as_str();
            if !visited.contains(dep_str) {
                detect_cycle(dep_str, commands, visited, stack)?;
            } else if stack.contains(dep_str) {
                return Err(ConfigError::Validation(format!(
                    "Circular dependency detected involving '{dep}'"
                )));
            }
        }
    }

    stack.remove(id);
    Ok(())
}

fn check_empty_groups(group: &CommandGroup) {
    for child in &group.children {
        if child.commands.is_empty() && child.children.is_empty() {
            warn!("Group '{}' has no commands and no children", child.name);
        }
        check_empty_groups(child);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::command::Command;

    fn make_cmd(id: &str) -> Command {
        Command {
            id: id.to_string(),
            name: id.to_string(),
            cmd: "echo test".to_string(),
            ..Default::default()
        }
    }

    fn make_group(id: &str, children: Vec<CommandGroup>, commands: Vec<Command>) -> CommandGroup {
        CommandGroup {
            id: id.to_string(),
            name: id.to_string(),
            children,
            commands,
            ..Default::default()
        }
    }

    #[test]
    fn test_duplicate_id_detection() {
        let config = make_group(
            "root",
            vec![make_group("dup", vec![], vec![make_cmd("dup")])],
            vec![],
        );
        let result = validate_tree(&config);
        assert!(result.is_err());
        match result.unwrap_err() {
            ConfigError::DuplicateId(id) => assert_eq!(id, "dup"),
            other => panic!("Expected DuplicateId, got: {other:?}"),
        }
    }

    #[test]
    fn test_unique_ids_pass() {
        let config = make_group(
            "root",
            vec![make_group("group1", vec![], vec![make_cmd("cmd1")])],
            vec![make_cmd("cmd2")],
        );
        assert!(validate_tree(&config).is_ok());
    }
}
