use std::path::{Path, PathBuf};

use crate::commands::group::CommandGroup;

/// A sub-repository discovered within a workspace.
pub struct SubRepo {
    pub name: String,
    pub path: PathBuf,
}

/// Find sub-repos in the workspace that have their own git repository.
///
/// Only returns directories that have a different `.git` dir from the workspace root,
/// meaning they are actual sub-repos (e.g. git submodules) rather than subdirectories
/// of the same repo.
#[must_use]
pub fn find_sub_repos(cwd: &Path, config: &CommandGroup) -> Vec<SubRepo> {
    let root_git_dir = git2::Repository::discover(cwd)
        .ok()
        .map(|r| r.path().to_path_buf());

    let mut sub_repos = Vec::new();

    for child in &config.children {
        let child_dir = if child.cwd.as_os_str().is_empty() {
            cwd.to_path_buf()
        } else if child.cwd.is_absolute() {
            child.cwd.clone()
        } else {
            cwd.join(&child.cwd)
        };

        if !child_dir.exists() {
            continue;
        }

        let child_git_dir = git2::Repository::discover(&child_dir)
            .ok()
            .map(|r| r.path().to_path_buf());

        // Only include if it has a different git dir (i.e., a separate repo)
        let is_different_repo = match (&root_git_dir, &child_git_dir) {
            (Some(root), Some(child)) => root != child,
            _ => false,
        };

        if is_different_repo {
            sub_repos.push(SubRepo {
                name: child.name.clone(),
                path: child_dir,
            });
        }
    }

    sub_repos
}
