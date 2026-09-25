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
pub mod logger;
pub mod mcp;
pub mod pty;
pub mod selectors;
pub mod setup;
pub mod theme;
pub mod tui;
pub mod workspace;

/// Load configuration from a file (or auto-detect), returning the root `CommandGroup` and cwd.
///
/// A relative `config_file` is resolved against the process working directory.
///
/// # Errors
///
/// Returns `ConfigError::ConfigFileMissing` if `config_file` doesn't exist, and another
/// `ConfigError` if no config file is found, it cannot be parsed, contains invalid values,
/// or references non-existent directories.
pub fn load_config(
    config_file: Option<&str>,
    no_workspace: bool,
) -> Result<(CommandGroup, PathBuf), ConfigError> {
    let config_path = match config_file {
        Some(file) => {
            // A bare filename has an empty parent, which breaks cwd and workspace resolution.
            let config_path = std::path::absolute(file)
                .map_err(|e| ConfigError::UnknownWorkingDirectory(e.to_string()))?;
            if !config_path.exists() {
                return Err(ConfigError::ConfigFileMissing(config_path));
            }
            // Canonicalize the directory (not the file, which may be a symlink) so `..`
            // components don't break workspace discovery's path prefix checks.
            let dir = config_path
                .parent()
                .ok_or_else(|| ConfigError::ConfigNotFound(config_path.clone()))?
                .canonicalize()
                .map_err(|e| ConfigError::UnknownWorkingDirectory(e.to_string()))?;
            match config_path.file_name() {
                Some(name) => dir.join(name),
                None => return Err(ConfigError::ConfigFileMissing(config_path)),
            }
        }
        None => Config::find_config()?,
    };

    // If workspace resolution is enabled, check for a parent workspace root
    let config_path = if no_workspace {
        config_path
    } else {
        find_workspace_root(&config_path)?.unwrap_or(config_path)
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
    check_version(parsed.fnug_version.as_deref());
    let (mut root, workspace) = parsed.into_root();

    // Discover and merge workspace sub-configs before converting
    if let Some(ref ws) = workspace {
        workspace::discover_and_merge(ws, &cwd, &mut root)?;
    }

    let mut config: CommandGroup = root.try_into()?;
    validate_tree(&config)?;
    validate_dependencies(&config)?;
    config.inherit(&Inheritance::from(cwd.clone()))?;
    Ok((config, cwd))
}

/// Search upward from a config file's directory for a parent config with `workspace` enabled.
/// Returns `Some(parent_config_path)` if found, `None` otherwise.
fn find_workspace_root(config_path: &std::path::Path) -> Result<Option<PathBuf>, ConfigError> {
    let config_dir = config_path
        .parent()
        .and_then(|p| p.canonicalize().ok())
        .ok_or_else(|| ConfigError::ConfigNotFound(config_path.to_path_buf()))?;

    let mut search_dir = config_dir.clone();
    while search_dir.pop() {
        if let Some(candidate) = config_file::find_config_in_dir(&search_dir) {
            let parsed = Config::from_file(&candidate)?;
            if matches!(
                parsed.workspace,
                Some(
                    config_file::WorkspaceConfig::Enabled(true)
                        | config_file::WorkspaceConfig::Options(_)
                )
            ) {
                debug!(
                    "Found workspace root: {} (from {})",
                    candidate.display(),
                    config_path.display()
                );
                return Ok(Some(candidate));
            }
        }
    }
    Ok(None)
}

/// Warn if the config's `fnug_version` doesn't fit this binary (see [`version_warning`]).
fn check_version(config_version: Option<&str>) {
    if let Some(message) =
        config_version.and_then(|v| version_warning(v, env!("CARGO_PKG_VERSION")))
    {
        warn!("{message}");
    }
}

/// Why a config written for fnug `config` may not work with fnug `binary`, if it may not.
///
/// Only the numeric `major.minor.patch` cores are compared. It warns when the config needs a
/// newer binary, or was written for an older release series (a different minor on 0.x, a
/// different major from 1.0), and when `config` can't be parsed.
fn version_warning(config: &str, binary: &str) -> Option<String> {
    let Some(wanted) = version_core(config) else {
        return Some(format!(
            "Config fnug_version '{config}': cannot parse it, expected a version like 0.1.0"
        ));
    };
    let current = version_core(binary)?;
    let series =
        |(major, minor, _): (u64, u64, u64)| if major == 0 { (0, minor) } else { (major, 0) };
    if wanted > current {
        Some(format!(
            "Config requires fnug >= {config}, but this is fnug {binary}"
        ))
    } else if series(wanted) != series(current) {
        Some(format!(
            "Config was written for fnug {config}; fnug {binary} may have breaking changes since"
        ))
    } else {
        None
    }
}

/// The leading `major.minor.patch` numbers of `version`, ignoring any pre-release or build
/// suffix (`0.1.0-alpha.13` and Python's `0.1.0a13` are both `(0, 1, 0)`). Missing minor and
/// patch numbers count as 0.
fn version_core(version: &str) -> Option<(u64, u64, u64)> {
    let version = version.trim();
    let version = version.strip_prefix('v').unwrap_or(version);
    let end = version
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(version.len());
    let mut parts = version[..end].split('.').map(|p| p.parse::<u64>().ok());
    let major = parts.next()??;
    let minor = parts.next().unwrap_or(Some(0))?;
    let patch = parts.next().unwrap_or(Some(0))?;
    Some((major, minor, patch))
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
    group.commands.iter().try_for_each(&mut *visit_cmd)?;
    group
        .children
        .iter()
        .try_for_each(|child| walk_tree(child, visit_group, visit_cmd))
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

    #[test]
    fn version_core_compare() {
        let cases = [
            ("0.1.0", "0.1.0-alpha.13", None),
            ("0.1.0a13", "0.1.0-alpha.13", None),
            ("0.1.0", "0.1.3", None),
            ("1.0.0", "1.4.2", None),
            ("v0.1", "0.1.0", None),
            ("0.2.0", "0.1.0-alpha.13", Some("requires fnug >= 0.2.0")),
            ("0.1.5", "0.1.0", Some("requires fnug >= 0.1.5")),
            ("0.0.9", "0.1.0-alpha.13", Some("written for fnug 0.0.9")),
            ("1.2.0", "2.0.0", Some("written for fnug 1.2.0")),
            ("garbage", "0.1.0", Some("cannot parse")),
            ("", "0.1.0", Some("cannot parse")),
        ];
        for (config, binary, expected) in cases {
            let warning = version_warning(config, binary);
            match expected {
                None => assert_eq!(warning, None, "{config} vs {binary}"),
                Some(part) => {
                    let warning = warning.unwrap_or_else(|| panic!("{config} vs {binary}"));
                    assert!(warning.contains(part), "{config} vs {binary}: {warning}");
                }
            }
        }
    }
}
