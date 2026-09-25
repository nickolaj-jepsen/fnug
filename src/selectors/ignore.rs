//! Git's ignore rules, so the watcher skips the files git selection never sees.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use git2::Repository;
use log::debug;

use crate::selectors::git::open_work_tree;

/// Tells whether git ignores a path, by the rules of the innermost known repo containing it:
/// `.gitignore` files, `.git/info/exclude` and `core.excludesFile`.
pub(crate) struct IgnoreFilter {
    /// Work tree roots and their repos, deepest first.
    repos: Vec<(PathBuf, Repository)>,
    /// Whether each directory looked up so far is ignored, by absolute path.
    dir_cache: HashMap<PathBuf, bool>,
}

impl IgnoreFilter {
    /// A filter with the repos containing `paths`. Paths in no repo are never ignored.
    pub(crate) fn new<'a>(paths: impl IntoIterator<Item = &'a Path>) -> Self {
        let mut repos: Vec<(PathBuf, Repository)> = Vec::new();
        for path in paths {
            match open_work_tree(path) {
                Ok((workdir, repo)) => {
                    if !repos.iter().any(|(known, _)| *known == workdir) {
                        repos.push((workdir, repo));
                    }
                }
                Err(e) => debug!("No ignore rules for {}: {e}", path.display()),
            }
        }
        repos.sort_by_key(|(workdir, _)| std::cmp::Reverse(workdir.components().count()));
        IgnoreFilter {
            repos,
            dir_cache: HashMap::new(),
        }
    }

    /// Whether git ignores the absolute `path`, which need not exist: a directory if `is_dir`,
    /// otherwise a file. Anything inside an ignored directory is ignored too.
    pub(crate) fn is_ignored(&mut self, path: &Path, is_dir: bool) -> bool {
        let IgnoreFilter { repos, dir_cache } = self;
        let Some((workdir, repo)) = repos.iter().find(|(workdir, _)| path.starts_with(workdir))
        else {
            return false;
        };
        let mut current = workdir.clone();
        let mut parts = path.strip_prefix(workdir).unwrap_or(path).components();
        while let Some(part) = parts.next() {
            current.push(part);
            let last = parts.as_path().as_os_str().is_empty();
            if last && !is_dir {
                return ask(repo, workdir, &current, false);
            }
            let ignored = *dir_cache
                .entry(current.clone())
                .or_insert_with(|| ask(repo, workdir, &current, true));
            if ignored {
                return true;
            }
        }
        false
    }

    /// Forget cached answers, as needed after an ignore file changed.
    pub(crate) fn clear_cache(&mut self) {
        self.dir_cache.clear();
    }
}

/// Ask `repo`, whose work tree is `workdir`, whether `path` is ignored, taking the parent
/// directories into account as well.
fn ask(repo: &Repository, workdir: &Path, path: &Path, is_dir: bool) -> bool {
    let Ok(rel) = path.strip_prefix(workdir) else {
        return false;
    };
    let mut spec = rel.as_os_str().to_owned();
    // The trailing slash tells libgit2 it's a directory, even if it has been removed.
    if is_dir {
        spec.push("/");
    }
    repo.is_path_ignored(Path::new(&spec)).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_with_ignores() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        Repository::init(&root).unwrap();
        std::fs::write(root.join(".gitignore"), "target/\n*.log\n").unwrap();
        std::fs::create_dir_all(root.join("src/gen")).unwrap();
        std::fs::write(root.join("src/.gitignore"), "gen/\n").unwrap();
        (tmp, root)
    }

    #[test]
    fn follows_nested_ignore_files() {
        let (_tmp, root) = repo_with_ignores();
        let mut filter = IgnoreFilter::new([root.as_path()]);

        assert!(!filter.is_ignored(&root.join("src/main.rs"), false));
        assert!(filter.is_ignored(&root.join("src/gen"), true));
        assert!(filter.is_ignored(&root.join("src/gen/out.rs"), false));
        assert!(filter.is_ignored(&root.join("debug.log"), false));
        assert!(!filter.is_ignored(&root.join("gen/x.rs"), false));
    }

    #[test]
    fn ignores_contents_of_missing_ignored_dirs() {
        let (_tmp, root) = repo_with_ignores();
        let mut filter = IgnoreFilter::new([root.as_path()]);

        assert!(filter.is_ignored(&root.join("target"), true));
        assert!(filter.is_ignored(&root.join("target/debug/build/x.rs"), false));
        assert!(!filter.is_ignored(&root.join("target"), false));
    }

    #[test]
    fn nested_repo_uses_its_own_rules() {
        let (_tmp, root) = repo_with_ignores();
        let sub = root.join("sub");
        std::fs::create_dir(&sub).unwrap();
        Repository::init(&sub).unwrap();
        std::fs::write(sub.join(".gitignore"), "dist/\n").unwrap();
        let mut filter = IgnoreFilter::new([root.as_path(), sub.as_path()]);

        assert!(filter.is_ignored(&sub.join("dist/a.js"), false));
        assert!(!filter.is_ignored(&root.join("dist/a.js"), false));
    }

    #[test]
    fn outside_any_repo_nothing_is_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        if open_work_tree(&root).is_ok() {
            eprintln!("skipping: the temp dir is inside a git repo");
            return;
        }
        std::fs::write(root.join(".gitignore"), "*\n").unwrap();
        let mut filter = IgnoreFilter::new([root.as_path()]);

        assert!(!filter.is_ignored(&root.join("a.txt"), false));
    }

    #[test]
    fn clear_cache_picks_up_changed_rules() {
        let (_tmp, root) = repo_with_ignores();
        let mut filter = IgnoreFilter::new([root.as_path()]);
        assert!(!filter.is_ignored(&root.join("dist/a.js"), false));

        std::fs::write(root.join(".gitignore"), "dist/\n").unwrap();
        filter.clear_cache();
        assert!(filter.is_ignored(&root.join("dist/a.js"), false));
    }
}
