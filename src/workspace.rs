//! Workspace discovery for mono-repo setups.
//!
//! Discovers nested `.fnug.yaml` files and merges them as child `CommandGroup`s.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use log::{debug, warn};

use crate::config_file::{
    Config, ConfigCommandGroup, ConfigError, WorkspaceConfig, find_config_in_dir,
};
use crate::trust::TrustPolicy;

/// How the directory walk treats a root that is not inside a git repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutsideGit {
    /// Walk without gitignore filtering (the loaded config's own workspace).
    Walk,
    /// Fail, so a non-git parent directory never claims configs below it.
    Fail,
}

/// Find the package configs `ws` selects below `root_dir`, as paths with canonical directories.
///
/// # Errors
///
/// Returns `ConfigError::Workspace` if `root_dir` can't be resolved, a glob pattern is invalid,
/// or a directory walk fails (including when `root_dir` is not in a git repository and
/// `outside_git` is [`OutsideGit::Fail`]).
pub fn discover(
    ws: &WorkspaceConfig,
    root_dir: &Path,
    outside_git: OutsideGit,
) -> Result<Vec<PathBuf>, ConfigError> {
    const DEFAULT_MAX_DEPTH: usize = 5;

    let root_dir = root_dir.canonicalize().map_err(|e| {
        ConfigError::Workspace(format!("Failed to resolve {}: {e}", root_dir.display()))
    })?;
    let paths = match ws {
        WorkspaceConfig::Enabled(false) => return Ok(vec![]),
        WorkspaceConfig::Enabled(true) => discover_walk(&root_dir, DEFAULT_MAX_DEPTH, outside_git)?,
        WorkspaceConfig::Options(opts) => {
            let max_depth = opts.max_depth.unwrap_or(DEFAULT_MAX_DEPTH);
            if let Some(patterns) = &opts.paths {
                discover_glob(&root_dir, patterns)?
            } else {
                discover_walk(&root_dir, max_depth, outside_git)?
            }
        }
    };
    debug!("Discovered {} workspace config(s)", paths.len());
    Ok(paths)
}

/// Load each package config that `trust` accepts and append it to `root`'s children. Refused
/// packages are skipped with a warning. Returns the loaded paths.
///
/// # Errors
///
/// Returns `ConfigError` if a package config can't be read or parsed.
pub fn merge(
    root: &mut ConfigCommandGroup,
    packages: &[PathBuf],
    trust: &TrustPolicy,
) -> Result<Vec<PathBuf>, ConfigError> {
    let children = root.children.get_or_insert_with(Vec::new);
    let mut loaded = Vec::new();
    for path in packages {
        if let Err(untrusted) = trust.check(path) {
            warn!(
                "Skipping workspace package: {}",
                ConfigError::from(untrusted)
            );
            continue;
        }
        children.push(load_sub_config(path)?);
        loaded.push(path.clone());
    }
    Ok(loaded)
}

/// Discover config files by walking the filesystem, skipping `.gitignore`'d paths.
fn discover_walk(
    root_dir: &Path,
    max_depth: usize,
    outside_git: OutsideGit,
) -> Result<Vec<PathBuf>, ConfigError> {
    let repo = match git2::Repository::discover(root_dir) {
        Ok(repo) => Some(repo),
        Err(e) if outside_git == OutsideGit::Walk => {
            debug!(
                "No git repository at {} ({e}); walking without gitignore",
                root_dir.display()
            );
            None
        }
        Err(e) => {
            return Err(ConfigError::Workspace(format!(
                "Failed to discover git repository: {e}"
            )));
        }
    };

    let mut seen_dirs = HashSet::new();
    let mut results = Vec::new();
    walk_dir(
        root_dir,
        root_dir,
        repo.as_ref(),
        max_depth,
        0,
        &mut seen_dirs,
        &mut results,
    )?;
    Ok(results)
}

/// Recursively walk `dir`, collecting config files while skipping ignored/hidden dirs.
fn walk_dir(
    dir: &Path,
    root_dir: &Path,
    repo: Option<&git2::Repository>,
    max_depth: usize,
    current_depth: usize,
    seen_dirs: &mut HashSet<PathBuf>,
    results: &mut Vec<PathBuf>,
) -> Result<(), ConfigError> {
    if current_depth >= max_depth {
        return Ok(());
    }
    let entries = std::fs::read_dir(dir).map_err(|e| {
        ConfigError::Workspace(format!("Failed to read directory {}: {e}", dir.display()))
    })?;

    for entry in entries {
        let entry = entry.map_err(|e| {
            ConfigError::Workspace(format!(
                "Failed to read dir entry in {}: {e}",
                dir.display()
            ))
        })?;

        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        // Skip hidden directories
        if name_str.starts_with('.') {
            continue;
        }

        // Skip gitignored directories
        if repo.is_some_and(|repo| repo.is_path_ignored(&path).unwrap_or(false)) {
            continue;
        }

        // Skip the root directory itself
        if path == root_dir {
            continue;
        }

        if let Some(config_path) = find_config_in_dir(&path) {
            let config_path = canonical_config(&config_path)?;
            if seen_dirs.insert(config_path.clone()) {
                debug!("Found workspace config: {}", config_path.display());
                results.push(config_path);
            }
        } else {
            walk_dir(
                &path,
                root_dir,
                repo,
                max_depth,
                current_depth + 1,
                seen_dirs,
                results,
            )?;
        }
    }

    Ok(())
}

/// Discover config files by expanding glob patterns.
fn discover_glob(root_dir: &Path, patterns: &[String]) -> Result<Vec<PathBuf>, ConfigError> {
    let mut seen_dirs = HashSet::new();
    let mut results = Vec::new();

    for pattern in patterns {
        let resolved = root_dir.join(pattern);
        let pattern_str = resolved.to_string_lossy();

        let entries = glob::glob(&pattern_str).map_err(|e| {
            ConfigError::Workspace(format!("Invalid glob pattern '{pattern}': {e}"))
        })?;

        for entry in entries {
            let dir = entry.map_err(|e| ConfigError::Workspace(format!("Glob error: {e}")))?;

            if !dir.is_dir() {
                continue;
            }
            let dir = dir.canonicalize().map_err(|e| {
                ConfigError::Workspace(format!("Failed to resolve {}: {e}", dir.display()))
            })?;

            // Skip the root directory itself
            if dir == root_dir {
                continue;
            }

            if let Some(config_path) = find_config_in_dir(&dir)
                && seen_dirs.insert(dir)
            {
                debug!("Found workspace config: {}", config_path.display());
                results.push(config_path);
            }
        }
    }

    Ok(results)
}

/// `config_path` with its directory canonicalized, so it compares equal to other spellings.
fn canonical_config(config_path: &Path) -> Result<PathBuf, ConfigError> {
    let resolve = || {
        Some(
            config_path
                .parent()?
                .canonicalize()
                .ok()?
                .join(config_path.file_name()?),
        )
    };
    resolve().ok_or_else(|| {
        ConfigError::Workspace(format!("Failed to resolve {}", config_path.display()))
    })
}

/// Load a sub-config file and prepare it as a `ConfigCommandGroup`.
fn load_sub_config(config_path: &Path) -> Result<ConfigCommandGroup, ConfigError> {
    let config = Config::from_file(config_path)?;
    crate::check_version(config.fnug_version.as_deref(), config_path);
    let (mut group, workspace) = config.into_root();

    if workspace.is_some() {
        warn!(
            "Workspace config '{}' has a 'workspace' field which will be ignored (no recursive discovery)",
            config_path.display()
        );
    }

    let sub_dir = config_path
        .parent()
        .ok_or_else(|| ConfigError::Workspace("Config path has no parent directory".into()))?
        .canonicalize()
        .map_err(|source| ConfigError::Io {
            path: config_path.to_path_buf(),
            source,
        })?;

    // An absolute cwd, so the package resolves paths exactly as it does standalone.
    group.cwd = Some(match group.cwd {
        Some(cwd) => sub_dir.join(cwd),
        None => sub_dir,
    });
    group.source = Some(config_path.to_path_buf());

    Ok(group)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_workspace_config_deserialize_bool() {
        let yaml = "true";
        let config: WorkspaceConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(matches!(config, WorkspaceConfig::Enabled(true)));
    }

    #[test]
    fn test_workspace_config_deserialize_false() {
        let yaml = "false";
        let config: WorkspaceConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(matches!(config, WorkspaceConfig::Enabled(false)));
    }

    #[test]
    fn test_workspace_config_deserialize_paths() {
        let yaml = "paths:\n  - ./packages/*/\n  - ./apps/*/";
        let config: WorkspaceConfig = serde_yaml::from_str(yaml).unwrap();
        match config {
            WorkspaceConfig::Options(opts) => {
                let paths = opts.paths.unwrap();
                assert_eq!(paths.len(), 2);
                assert_eq!(paths[0], "./packages/*/");
                assert_eq!(paths[1], "./apps/*/");
                assert!(opts.max_depth.is_none());
            }
            WorkspaceConfig::Enabled(enabled) => {
                panic!("Expected Options variant, got: Enabled({enabled})")
            }
        }
    }

    #[test]
    fn test_workspace_config_deserialize_max_depth() {
        let yaml = "max_depth: 2";
        let config: WorkspaceConfig = serde_yaml::from_str(yaml).unwrap();
        match config {
            WorkspaceConfig::Options(opts) => {
                assert!(opts.paths.is_none());
                assert_eq!(opts.max_depth, Some(2));
            }
            WorkspaceConfig::Enabled(enabled) => {
                panic!("Expected Options variant, got: Enabled({enabled})")
            }
        }
    }

    #[test]
    fn test_workspace_config_deserialize_paths_with_max_depth() {
        let yaml = "paths:\n  - ./packages/*/\nmax_depth: 10";
        let config: WorkspaceConfig = serde_yaml::from_str(yaml).unwrap();
        match config {
            WorkspaceConfig::Options(opts) => {
                assert_eq!(opts.paths.unwrap().len(), 1);
                assert_eq!(opts.max_depth, Some(10));
            }
            WorkspaceConfig::Enabled(enabled) => {
                panic!("Expected Options variant, got: Enabled({enabled})")
            }
        }
    }
}
