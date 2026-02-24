//! Core implementation of the Fnug command scheduler
//!
//! Fnug is a command scheduler that detects and executes commands based on file system
//! and git changes. It allows users to define commands and command groups in a configuration
//! file, with flexible automation rules for when commands should be executed.

use std::collections::{HashMap, HashSet};
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
pub mod mcp;
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

/// Walk a command tree, calling `visit_group` on each group and `visit_cmd` on each command.
/// Short-circuits on the first error.
fn walk_tree(
    group: &CommandGroup,
    visit_group: &mut impl FnMut(&CommandGroup) -> Result<(), ConfigError>,
    visit_cmd: &mut impl FnMut(&Command) -> Result<(), ConfigError>,
) -> Result<(), ConfigError> {
    visit_group(group)?;
    for cmd in &group.commands {
        visit_cmd(cmd)?;
    }
    for child in &group.children {
        walk_tree(child, visit_group, visit_cmd)?;
    }
    Ok(())
}

fn check_duplicates(group: &CommandGroup, seen: &mut HashSet<String>) -> Result<(), ConfigError> {
    fn check_id(id: &str, seen: &mut HashSet<String>) -> Result<(), ConfigError> {
        if !seen.insert(id.to_string()) {
            return Err(ConfigError::DuplicateId(id.to_string()));
        }
        Ok(())
    }

    check_id(&group.id, seen)?;
    for cmd in &group.commands {
        check_id(&cmd.id, seen)?;
    }
    for child in &group.children {
        check_duplicates(child, seen)?;
    }
    Ok(())
}

fn check_empty_names(group: &CommandGroup) -> Result<(), ConfigError> {
    walk_tree(
        group,
        &mut |g| {
            if g.name.trim().is_empty() {
                return Err(ConfigError::Validation(format!(
                    "Group with id '{}' has an empty name",
                    g.id
                )));
            }
            Ok(())
        },
        &mut |cmd| {
            if cmd.name.trim().is_empty() {
                return Err(ConfigError::Validation(format!(
                    "Command with id '{}' has an empty name",
                    cmd.id
                )));
            }
            Ok(())
        },
    )
}

fn check_empty_commands(group: &CommandGroup) -> Result<(), ConfigError> {
    walk_tree(group, &mut |_| Ok(()), &mut |cmd| {
        if cmd.cmd.trim().is_empty() {
            return Err(ConfigError::Validation(format!(
                "Command '{}' has an empty cmd string",
                cmd.name
            )));
        }
        Ok(())
    })
}

/// Validate that all `depends_on` references resolve and there are no cycles
fn validate_dependencies(root: &CommandGroup) -> Result<(), ConfigError> {
    let commands = root.all_commands();
    let cmd_by_id: HashMap<&str, &Command> = commands.iter().map(|c| (c.id.as_str(), *c)).collect();

    // Validate references
    for cmd in &commands {
        for dep in &cmd.depends_on {
            if !cmd_by_id.contains_key(dep.as_str()) {
                return Err(ConfigError::Validation(format!(
                    "Command '{}' depends on '{}' which does not exist",
                    cmd.name, dep
                )));
            }
        }
    }

    // Cycle detection via DFS with O(1) lookup
    let mut visited = HashSet::new();
    let mut stack = HashSet::new();
    for cmd in &commands {
        if !visited.contains(cmd.id.as_str()) {
            detect_cycle(cmd.id.as_str(), &cmd_by_id, &mut visited, &mut stack)?;
        }
    }

    Ok(())
}

fn detect_cycle<'a>(
    id: &'a str,
    cmd_by_id: &HashMap<&str, &'a Command>,
    visited: &mut HashSet<&'a str>,
    stack: &mut HashSet<&'a str>,
) -> Result<(), ConfigError> {
    visited.insert(id);
    stack.insert(id);

    if let Some(cmd) = cmd_by_id.get(id) {
        for dep in &cmd.depends_on {
            let dep_str: &str = dep.as_str();
            if !visited.contains(dep_str) {
                detect_cycle(dep_str, cmd_by_id, visited, stack)?;
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
    // walk_tree requires Result return; use Infallible since this never fails.
    let _ = walk_tree(
        group,
        &mut |g| {
            if g.commands.is_empty() && g.children.is_empty() {
                warn!("Group '{}' has no commands and no children", g.name);
            }
            Ok(())
        },
        &mut |_| Ok(()),
    );
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
