//! Workspace discovery for mono-repo setups.
//!
//! Discovers nested `.fnug.yaml` files and merges them as child `CommandGroup`s.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use log::{debug, warn};

use crate::config_file::{
    Config, ConfigCommandGroup, ConfigError, WorkspaceConfig, find_config_in_dir,
};

/// Discover workspace configs and append them as children of the root config group.
///
/// # Errors
///
/// Returns `ConfigError` if discovery fails or a sub-config cannot be parsed.
pub fn discover_and_merge(
    ws: &WorkspaceConfig,
    root_dir: &Path,
    root: &mut ConfigCommandGroup,
) -> Result<Vec<PathBuf>, ConfigError> {
    const DEFAULT_MAX_DEPTH: usize = 5;

    let paths = match ws {
        WorkspaceConfig::Enabled(false) => return Ok(vec![]),
        WorkspaceConfig::Enabled(true) => discover_git(root_dir, DEFAULT_MAX_DEPTH)?,
        WorkspaceConfig::Options(opts) => {
            let max_depth = opts.max_depth.unwrap_or(DEFAULT_MAX_DEPTH);
            if let Some(patterns) = &opts.paths {
                discover_glob(root_dir, patterns)?
            } else {
                discover_git(root_dir, max_depth)?
            }
        }
    };

    debug!("Discovered {} workspace config(s)", paths.len());

    let children = root.children.get_or_insert_with(Vec::new);
    for path in &paths {
        let sub = load_sub_config(path, root_dir)?;
        children.push(sub);
    }

    Ok(paths)
}

/// Discover config files by walking the filesystem, skipping `.gitignore`'d paths.
fn discover_git(root_dir: &Path, max_depth: usize) -> Result<Vec<PathBuf>, ConfigError> {
    let repo = git2::Repository::discover(root_dir)
        .map_err(|e| ConfigError::Workspace(format!("Failed to discover git repository: {e}")))?;

    let mut seen_dirs = HashSet::new();
    let mut results = Vec::new();
    walk_dir(
        root_dir,
        root_dir,
        &repo,
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
    repo: &git2::Repository,
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
        if repo.is_path_ignored(&path).unwrap_or(false) {
            continue;
        }

        // Skip the root directory itself
        if path == root_dir {
            continue;
        }

        if let Some(config_path) = find_config_in_dir(&path) {
            if seen_dirs.insert(path.clone()) {
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

/// Load a sub-config file and prepare it as a `ConfigCommandGroup`.
fn load_sub_config(config_path: &Path, root_dir: &Path) -> Result<ConfigCommandGroup, ConfigError> {
    let config = Config::from_file(config_path)?;

    if config.workspace.is_some() {
        warn!(
            "Workspace config '{}' has a 'workspace' field which will be ignored (no recursive discovery)",
            config_path.display()
        );
    }

    let sub_dir = config_path
        .parent()
        .ok_or_else(|| ConfigError::Workspace("Config path has no parent directory".into()))?;

    let mut group = config.root;

    // Set cwd to the sub-config's directory relative to root, if not already set
    if group.cwd.is_none() {
        let relative = sub_dir.strip_prefix(root_dir).unwrap_or(sub_dir);
        group.cwd = Some(relative.to_path_buf());
    }

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
            other => panic!("Expected Options variant, got: {other:?}"),
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
            other => panic!("Expected Options variant, got: {other:?}"),
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
            other => panic!("Expected Options variant, got: {other:?}"),
        }
    }
}
