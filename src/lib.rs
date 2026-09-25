//! Core implementation of the Fnug command scheduler
//!
//! Fnug is a command scheduler that detects and executes commands based on file system
//! and git changes. It allows users to define commands and command groups in a configuration
//! file, with flexible automation rules for when commands should be executed.

use std::path::{Path, PathBuf};

use log::{debug, warn};

use crate::commands::command::Command;
use crate::commands::group::CommandGroup;
use crate::commands::inherit::{Inheritable, Inheritance};
use crate::config_file::{Config, ConfigError};
use crate::trust::TrustPolicy;

pub mod check;
pub mod commands;
pub mod config_file;
pub mod logger;
pub mod mcp;
pub mod process;
pub mod pty;
pub mod schema;
pub mod selectors;
pub mod setup;
pub mod theme;
pub mod trust;
pub mod tui;
pub mod workspace;

/// How to find and load a config.
#[derive(Debug, Clone, Default)]
pub struct LoadOptions {
    /// Explicit config file, always loaded as the root. Without it, the nearest config file at
    /// or above `start_dir` is used, or the parent workspace root whose discovery includes it.
    pub config: Option<PathBuf>,
    /// Don't resolve upward to a parent workspace root.
    pub no_workspace: bool,
    /// Directory to act from: the config search starts here and a relative `config` or
    /// `root_dir` resolves against it. Defaults to the process working directory.
    pub start_dir: Option<PathBuf>,
    /// Base directory for the config's relative paths and workspace discovery, instead of the
    /// config file's directory. Without `config`, the search starts here. Setting it disables
    /// the parent workspace lookup.
    pub root_dir: Option<PathBuf>,
    /// Which configs may be loaded without being passed as `config`: the one found by
    /// searching, a parent workspace root, and workspace packages. The default ignores
    /// `FNUG_SAFE_DIRECTORIES`; pass [`TrustPolicy::from_env`] to honour it like the CLI does.
    pub trust: TrustPolicy,
}

/// A loaded, validated config with inheritance applied.
#[derive(Debug, Clone)]
pub struct LoadedConfig {
    /// The root command group.
    pub root: CommandGroup,
    /// The directory relative paths in the root config resolve against: `root_dir`, or the root
    /// config's directory.
    pub cwd: PathBuf,
    /// The root config file (the workspace root, if one was resolved).
    pub config_path: PathBuf,
    /// Every config file that was read: `config_path`, then any workspace packages.
    pub sources: Vec<PathBuf>,
}

/// Find, parse, validate and resolve a config.
///
/// # Errors
///
/// Returns `ConfigError::EmptyConfigPath` if `opts.config` is empty,
/// `ConfigError::ConfigFileMissing` if `opts.config` doesn't exist,
/// `ConfigError::RootDirMissing` if `opts.root_dir` doesn't exist,
/// `ConfigError::ConfigNotFound` if no config file is found, `ConfigError::UntrustedConfig` if
/// the config found by searching is refused by `opts.trust`, and another `ConfigError` if a
/// config cannot be parsed, contains invalid values, or references non-existent directories.
/// Untrusted parent workspace roots and packages are skipped with a warning.
pub fn load(opts: &LoadOptions) -> Result<LoadedConfig, ConfigError> {
    let start_dir = match &opts.start_dir {
        Some(dir) => dir.canonicalize(),
        None => std::env::current_dir(),
    }
    .map_err(|e| ConfigError::UnknownWorkingDirectory(e.to_string()))?;

    let root_dir = opts
        .root_dir
        .as_ref()
        .map(|dir| {
            let dir = start_dir.join(dir);
            match dir.canonicalize() {
                Ok(canonical) if !canonical.is_dir() => Err(ConfigError::RootDirMissing {
                    path: dir,
                    source: std::io::ErrorKind::NotADirectory.into(),
                }),
                result => {
                    result.map_err(|source| ConfigError::RootDirMissing { path: dir, source })
                }
            }
        })
        .transpose()?;

    let found = match &opts.config {
        Some(file) if file.as_os_str().is_empty() => return Err(ConfigError::EmptyConfigPath),
        Some(file) if opts.start_dir.is_some() => resolve_config_arg(&start_dir.join(file))?,
        Some(file) => resolve_config_arg(file)?,
        None => {
            config_file::find_config_from(root_dir.as_ref().unwrap_or(&start_dir), &opts.trust)?
        }
    };

    // An explicit config or base directory pins the root; a found config may belong to a
    // parent workspace.
    let promoted = if opts.no_workspace || opts.config.is_some() || root_dir.is_some() {
        None
    } else {
        find_workspace_root(&found, &opts.trust)
    };
    let (config_path, parsed, packages) = if let Some((path, parsed, packages)) = promoted {
        (path, parsed, Some(packages))
    } else {
        let parsed = Config::from_file(&found)?;
        (found, parsed, None)
    };

    let cwd = match root_dir {
        Some(dir) => dir,
        None => config_path
            .parent()
            .ok_or_else(|| ConfigError::ConfigNotFound(config_path.clone()))?
            .to_path_buf(),
    };
    debug!(
        "Creating core from config file: {} (cwd: {})",
        config_path.display(),
        cwd.display()
    );
    check_version(parsed.fnug_version.as_deref(), &config_path);
    let (mut root, workspace) = parsed.into_root();
    root.source = Some(config_path.clone());

    let packages = match (packages, &workspace) {
        (Some(packages), _) => packages,
        (None, Some(ws)) => workspace::discover(ws, &cwd, workspace::OutsideGit::Walk)?,
        (None, None) => Vec::new(),
    };
    let mut sources = vec![config_path.clone()];
    sources.extend(workspace::merge(&mut root, &packages, &opts.trust)?);

    commands::ids::assign_ids(&mut root)?;
    let mut config: CommandGroup = root.try_into()?;
    validate_tree(&config)?;
    config.inherit(&Inheritance::from(cwd.clone()))?;
    Ok(LoadedConfig {
        root: config,
        cwd,
        config_path,
        sources,
    })
}

/// Load configuration from a file (or auto-detect), returning the root `CommandGroup` and cwd.
///
/// A relative `config_file` is resolved against the process working directory.
///
/// # Errors
///
/// See [`load`].
pub fn load_config(
    config_file: Option<&str>,
    no_workspace: bool,
) -> Result<(CommandGroup, PathBuf), ConfigError> {
    let loaded = load(&LoadOptions {
        config: config_file.map(PathBuf::from),
        no_workspace,
        trust: TrustPolicy::from_env(),
        ..LoadOptions::default()
    })?;
    Ok((loaded.root, loaded.cwd))
}

/// Make an explicitly given config path absolute, with its directory canonicalized.
fn resolve_config_arg(file: &Path) -> Result<PathBuf, ConfigError> {
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
        Some(name) => Ok(dir.join(name)),
        None => Err(ConfigError::ConfigFileMissing(config_path)),
    }
}

/// The nearest ancestor config whose workspace discovery includes `start` (a config path with a
/// canonical directory), with its parsed config and discovered packages.
///
/// Ancestors that `trust` refuses, that fail to parse, or whose discovery fails are skipped
/// with a warning.
fn find_workspace_root(
    start: &Path,
    trust: &TrustPolicy,
) -> Option<(PathBuf, Config, Vec<PathBuf>)> {
    for dir in start.parent()?.ancestors().skip(1) {
        let Some(candidate) = config_file::find_config_in_dir(dir) else {
            continue;
        };
        if let Err(untrusted) = trust.check(&candidate) {
            warn!("Ignoring parent config: {}", ConfigError::from(untrusted));
            continue;
        }
        let parsed = match Config::from_file(&candidate) {
            Ok(parsed) => parsed,
            Err(e) => {
                warn!("Ignoring parent config: {e}");
                continue;
            }
        };
        let Some(ws) = &parsed.workspace else {
            continue;
        };
        let packages = match workspace::discover(ws, dir, workspace::OutsideGit::Fail) {
            Ok(packages) => packages,
            Err(e) => {
                warn!("Ignoring parent workspace {}: {e}", candidate.display());
                continue;
            }
        };
        if packages.iter().any(|p| p == start) {
            debug!(
                "Found workspace root: {} (from {})",
                candidate.display(),
                start.display()
            );
            return Some((candidate, parsed, packages));
        }
        debug!(
            "Workspace {} does not include {}",
            candidate.display(),
            start.display()
        );
    }
    None
}

/// Warn if the `fnug_version` of the config at `path` doesn't fit this binary (see
/// [`version_warning`]).
pub(crate) fn check_version(config_version: Option<&str>, path: &Path) {
    if let Some(message) =
        config_version.and_then(|v| version_warning(v, env!("CARGO_PKG_VERSION")))
    {
        warn!("{}: {message}", path.display());
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

/// Reject empty `cmd`s and warn about empty groups. Names and ids are checked by
/// [`commands::ids::assign_ids`].
fn validate_tree(root: &CommandGroup) -> Result<(), ConfigError> {
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
