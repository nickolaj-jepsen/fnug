//! Tests for `fnug setup`'s git hook handling.

use std::path::{Path, PathBuf};
use std::sync::Once;

use fnug::setup::hooks::{self, HookError, HookLocation};
use git2::{IndexAddOption, Repository, RepositoryInitOptions, Signature};

/// Keep the developer's global and system git config (a global `core.hooksPath`, say) out of the
/// repositories these tests open.
fn isolate_git_config() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let empty = tempfile::tempdir().unwrap().keep();
        for level in [
            git2::ConfigLevel::System,
            git2::ConfigLevel::XDG,
            git2::ConfigLevel::Global,
            git2::ConfigLevel::ProgramData,
        ] {
            // SAFETY: every test calls this before opening a repository, and `Once` makes the
            // others wait until it is done.
            unsafe { git2::opts::set_search_path(level, &empty) }.unwrap();
        }
    });
}

/// A temporary directory, canonicalized so paths compare equal to the ones git reports.
fn tempdir() -> (tempfile::TempDir, PathBuf) {
    isolate_git_config();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    (tmp, root)
}

fn commit_all(repo: &Repository) {
    let mut index = repo.index().unwrap();
    index.add_all(["*"], IndexAddOption::DEFAULT, None).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let sig = Signature::now("fnug", "fnug@example.com").unwrap();
    let parent = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
    let parents: Vec<_> = parent.iter().collect();
    repo.commit(Some("HEAD"), &sig, &sig, "commit", &tree, &parents)
        .unwrap();
}

fn set_hooks_path(repo: &Repository, value: &str) {
    repo.config()
        .unwrap()
        .set_str("core.hooksPath", value)
        .unwrap();
}

#[test]
fn linked_worktree_installs_to_commondir() {
    let (_tmp, root) = tempdir();
    let main = root.join("main");
    std::fs::create_dir(&main).unwrap();
    let repo = Repository::init(&main).unwrap();
    std::fs::write(main.join("a.txt"), "one\n").unwrap();
    commit_all(&repo);
    let wt = root.join("wt");
    repo.worktree("wt", &wt, None).unwrap();

    hooks::install(&wt, false).unwrap();

    assert!(main.join(".git/hooks/pre-commit").exists());
    assert!(!main.join(".git/worktrees/wt/hooks/pre-commit").exists());
    assert!(hooks::is_installed(&wt));
    assert!(hooks::is_installed(&main), "worktrees share one hooks dir");
    assert_eq!(hooks::resolve(&wt).unwrap().workdir, wt);
}

#[test]
fn gitlink_submodule_is_installed_true() {
    let (_tmp, outer) = tempdir();
    Repository::init(&outer).unwrap();
    let sub = outer.join("sub");
    let gitdir = outer.join(".git/modules/sub");
    Repository::init_opts(
        &gitdir,
        RepositoryInitOptions::new()
            .no_dotgit_dir(true)
            .workdir_path(&sub),
    )
    .unwrap();

    assert!(!hooks::is_installed(&sub));
    hooks::install(&sub, true).unwrap();

    assert!(gitdir.join("hooks/pre-commit").exists());
    assert!(hooks::is_installed(&sub));
    assert!(
        !hooks::is_installed(&outer),
        "the outer repo has its own hooks"
    );
    hooks::remove(&sub).unwrap();
    assert!(!gitdir.join("hooks/pre-commit").exists());
}

#[test]
fn core_hooks_path_respected() {
    let (_tmp, root) = tempdir();
    let repo = Repository::init(&root).unwrap();
    set_hooks_path(&repo, ".githooks");

    let target = hooks::resolve(&root).unwrap();
    assert_eq!(target.location, HookLocation::Shared);
    assert_eq!(target.hook_path, root.join(".githooks/pre-commit"));

    hooks::install(&root, false).unwrap();
    assert!(root.join(".githooks/pre-commit").exists());
    assert!(!root.join(".git/hooks/pre-commit").exists());
    assert!(hooks::is_installed(&root));
}

#[test]
fn hooks_path_inside_git_dir_is_local() {
    let (_tmp, root) = tempdir();
    let repo = Repository::init(&root).unwrap();
    set_hooks_path(&repo, &root.join(".git/my-hooks").display().to_string());

    let target = hooks::resolve(&root).unwrap();
    assert_eq!(target.location, HookLocation::Local);
    assert_eq!(target.hook_path, root.join(".git/my-hooks/pre-commit"));
}

#[test]
fn husky_refused_with_snippet() {
    for hooks_path in [".husky/_", ".husky"] {
        let (_tmp, root) = tempdir();
        let repo = Repository::init(&root).unwrap();
        set_hooks_path(&repo, hooks_path);

        let err = hooks::install(&root, false).unwrap_err();
        let HookError::Husky { user_hook, snippet } = &err else {
            panic!("{hooks_path}: expected a husky refusal, got {err}");
        };
        assert_eq!(user_hook, &root.join(".husky/pre-commit"));
        assert!(
            snippet.contains("check --fail-fast --mute-success"),
            "{snippet}"
        );
        assert!(err.to_string().contains(".husky/pre-commit"), "{err}");
        assert!(!root.join(".husky").exists(), "nothing is written");
        assert!(!root.join(".git/hooks/pre-commit").exists());
    }
}

#[test]
fn global_hooks_path_refused() {
    let (_tmp, root) = tempdir();
    let repo_dir = root.join("repo");
    let shared = root.join("shared-hooks");
    std::fs::create_dir(&shared).unwrap();
    let repo = Repository::init(&repo_dir).unwrap();
    set_hooks_path(&repo, &shared.display().to_string());

    let err = hooks::install(&repo_dir, false).unwrap_err();
    let HookError::GlobalHooksPath { path, snippet } = &err else {
        panic!("expected a hooksPath refusal, got {err}");
    };
    assert_eq!(path, &shared);
    assert!(
        snippet.contains("check --fail-fast --mute-success"),
        "{snippet}"
    );
    assert!(!shared.join("pre-commit").exists());
    assert!(!repo_dir.join(".git/hooks/pre-commit").exists());
}

#[test]
fn subdir_config_is_installed() {
    let (_tmp, root) = tempdir();
    Repository::init(&root).unwrap();
    let app = root.join("app");
    std::fs::create_dir(&app).unwrap();

    hooks::install(&app, false).unwrap();

    assert!(root.join(".git/hooks/pre-commit").exists());
    assert!(hooks::is_installed(&app));
    assert_eq!(hooks::resolve(&app).unwrap().config_rel, Path::new("app"));
}

#[test]
fn bare_repo_is_an_error() {
    let (_tmp, root) = tempdir();
    Repository::init_bare(&root).unwrap();
    let err = hooks::resolve(&root).unwrap_err();
    assert!(matches!(err, HookError::BareRepo { .. }), "{err}");
}
