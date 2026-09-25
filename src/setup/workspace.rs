use std::path::{Path, PathBuf};

use crate::commands::group::CommandGroup;
use crate::setup::hooks;

/// A workspace package with its own git repository.
pub struct SubRepo {
    pub name: String,
    /// The package's config directory.
    pub path: PathBuf,
}

/// Workspace packages whose pre-commit hook differs from the one for `cwd`, such as packages in
/// git submodules, so they need a hook of their own. A hook runs one package's checks, so of
/// packages that share a hook only the first is returned, with a warning.
#[must_use]
pub fn find_sub_repos(cwd: &Path, config: &CommandGroup) -> Vec<SubRepo> {
    let root_hook = hooks::resolve(cwd).ok().map(|t| t.hook_path);
    let mut hooks_seen: Vec<PathBuf> = Vec::new();
    let mut sub_repos: Vec<SubRepo> = Vec::new();
    for child in &config.children {
        // Only packages have a config of their own for the hook to run
        let Some(dir) = child.source.as_deref().and_then(Path::parent) else {
            continue;
        };
        let Ok(target) = hooks::resolve(dir) else {
            continue;
        };
        if root_hook.as_ref() == Some(&target.hook_path) {
            continue;
        }
        if let Some(i) = hooks_seen.iter().position(|h| *h == target.hook_path) {
            log::warn!(
                "packages {} and {} share the pre-commit hook {}, so it only runs {}'s checks",
                sub_repos[i].name,
                child.name,
                target.hook_path.display(),
                sub_repos[i].name
            );
            continue;
        }
        hooks_seen.push(target.hook_path);
        sub_repos.push(SubRepo {
            name: child.name.clone(),
            path: dir.to_path_buf(),
        });
    }
    sub_repos
}
