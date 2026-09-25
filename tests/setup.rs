//! Tests for `fnug setup`'s git hook handling. Hooks are run with `sh` and a shim `fnug` on
//! `PATH` that records its arguments.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Once;

use fnug::setup::hooks::{
    self, ForeignPolicy, HookError, HookLocation, HookStatus, InstallOptions, InstallOutcome,
};
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

fn init_gitlink_repo(gitdir: &Path, workdir: &Path) -> Repository {
    Repository::init_opts(
        gitdir,
        RepositoryInitOptions::new()
            .no_dotgit_dir(true)
            .workdir_path(workdir),
    )
    .unwrap()
}

#[test]
fn sub_repos_are_packages_in_other_repos() {
    let (_tmp, root) = tempdir();
    Repository::init(&root).unwrap();
    std::fs::write(
        root.join(".fnug.yaml"),
        "name: root\nworkspace:\n  paths: [./sub, ./same]\nchildren:\n  - name: plain\n    cwd: plain-sub\n    commands: []\n",
    )
    .unwrap();
    // A package in a submodule whose commands run in a subdirectory
    let sub = root.join("sub");
    init_gitlink_repo(&root.join(".git/modules/sub"), &sub);
    std::fs::create_dir(sub.join("src")).unwrap();
    std::fs::write(
        sub.join(".fnug.yaml"),
        "name: sub\ncwd: src\ncommands:\n  - name: test\n    cmd: 'true'\n",
    )
    .unwrap();
    // A package in the root repo
    std::fs::create_dir(root.join("same")).unwrap();
    std::fs::write(
        root.join("same/.fnug.yaml"),
        "name: same\ncommands:\n  - name: test\n    cmd: 'true'\n",
    )
    .unwrap();
    // A plain group whose cwd is another repo, but which has no config of its own
    init_gitlink_repo(&root.join(".git/modules/plain"), &root.join("plain-sub"));

    let loaded = fnug::load(&fnug::LoadOptions {
        config: Some(root.join(".fnug.yaml")),
        ..fnug::LoadOptions::default()
    })
    .unwrap();
    let repos = fnug::setup::workspace::find_sub_repos(&loaded.cwd, &loaded.root);

    let found: Vec<(&str, &Path)> = repos
        .iter()
        .map(|r| (r.name.as_str(), r.path.as_path()))
        .collect();
    assert_eq!(found, [("sub", sub.as_path())]);
}

#[test]
fn packages_sharing_a_sub_repo_get_one_hook() {
    let (_tmp, root) = tempdir();
    Repository::init(&root).unwrap();
    std::fs::write(
        root.join(".fnug.yaml"),
        "name: root\nworkspace:\n  paths: [./sub/a, ./sub/b]\ncommands: []\n",
    )
    .unwrap();
    let sub = root.join("sub");
    init_gitlink_repo(&root.join(".git/modules/sub"), &sub);
    for name in ["a", "b"] {
        std::fs::create_dir(sub.join(name)).unwrap();
        std::fs::write(
            sub.join(name).join(".fnug.yaml"),
            format!("name: {name}\ncommands:\n  - name: test\n    cmd: 'true'\n"),
        )
        .unwrap();
    }

    let loaded = fnug::load(&fnug::LoadOptions {
        config: Some(root.join(".fnug.yaml")),
        ..fnug::LoadOptions::default()
    })
    .unwrap();
    let repos = fnug::setup::workspace::find_sub_repos(&loaded.cwd, &loaded.root);

    let found: Vec<&str> = repos.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(found, ["a"], "one hook can only cd into one package");
}

#[test]
fn bare_repo_is_an_error() {
    let (_tmp, root) = tempdir();
    Repository::init_bare(&root).unwrap();
    let err = hooks::resolve(&root).unwrap_err();
    assert!(matches!(err, HookError::BareRepo { .. }), "{err}");
}

// ─── hook contents, run with sh ───

const BEGIN: &str = "# >>> fnug >>>";

/// A fresh repository and the path of its pre-commit hook.
fn repo() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let (tmp, root) = tempdir();
    Repository::init(&root).unwrap();
    let hook = root.join(".git/hooks/pre-commit");
    (tmp, root, hook)
}

fn write_executable(path: &Path, content: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap()
}

/// A directory holding a `fnug` that logs its working directory and arguments, then exits with
/// `$FNUG_SHIM_EXIT`.
struct Shim {
    _dir: tempfile::TempDir,
    bin: PathBuf,
    log: PathBuf,
}

impl Shim {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        let log = dir.path().join("log");
        write_executable(
            &bin.join("fnug"),
            "#!/bin/sh\n{ printf 'pwd=%s\\n' \"$PWD\"; printf 'arg=%s\\n' \"$@\"; } >> \"$FNUG_SHIM_LOG\"\nexit \"${FNUG_SHIM_EXIT:-0}\"\n",
        );
        Self {
            _dir: dir,
            bin,
            log,
        }
    }

    /// Run `hook` from `workdir` like git does, with the shim first on `PATH`.
    fn run(&self, hook: &Path, workdir: &Path, fnug_exit: i32) -> i32 {
        let path = format!(
            "{}:{}",
            self.bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        self.run_with_path(hook, workdir, fnug_exit, &path)
    }

    fn run_with_path(&self, hook: &Path, workdir: &Path, fnug_exit: i32, path: &str) -> i32 {
        let _ = std::fs::remove_file(&self.log);
        let output = Command::new("sh")
            .arg(hook)
            .current_dir(workdir)
            .env("PATH", path)
            .env("FNUG_SHIM_LOG", &self.log)
            .env("FNUG_SHIM_EXIT", fnug_exit.to_string())
            .output()
            .unwrap();
        output.status.code().expect("hook killed by a signal")
    }

    fn ran(&self) -> bool {
        self.log.exists()
    }

    fn args(&self) -> Vec<String> {
        read(&self.log)
            .lines()
            .filter_map(|l| l.strip_prefix("arg="))
            .map(str::to_string)
            .collect()
    }
}

fn options(foreign: ForeignPolicy) -> InstallOptions {
    InstallOptions {
        no_workspace: false,
        foreign,
    }
}

#[test]
fn new_hook_runs_fnug_with_hook_args() {
    for no_workspace in [false, true] {
        let (_tmp, root, hook) = repo();
        let shim = Shim::new();
        hooks::install(&root, no_workspace).unwrap();

        let mode = std::fs::metadata(&hook).unwrap().permissions().mode();
        assert_eq!(mode & 0o111, 0o111, "hook should be executable");
        assert!(read(&hook).starts_with("#!/bin/sh\n# >>> fnug >>>\n"));
        assert_eq!(shim.run(&hook, &root, 0), 0);
        assert_eq!(shim.args(), hooks::hook_args(no_workspace));
        assert_eq!(
            shim.run(&hook, &root, 3),
            3,
            "fnug's failure blocks the commit"
        );
    }
}

#[test]
fn existing_failing_hook_still_blocks() {
    let (_tmp, root, hook) = repo();
    let shim = Shim::new();
    write_executable(&hook, "#!/bin/sh\necho old-hook-says-no\nfalse\n");

    hooks::install(&root, false).unwrap();

    assert_eq!(shim.run(&hook, &root, 0), 1);
    assert!(shim.ran());
}

#[test]
fn exec_style_hook_runs_fnug_first() {
    let (_tmp, root, hook) = repo();
    let shim = Shim::new();
    write_executable(&hook, "#!/usr/bin/env bash\nexec true\n");

    hooks::install(&root, false).unwrap();

    assert_eq!(shim.run(&hook, &root, 3), 3);
    assert!(shim.ran());
    assert_eq!(shim.run(&hook, &root, 0), 0);
}

#[test]
fn python_hook_refused_then_chained() {
    let (_tmp, root, hook) = repo();
    let shim = Shim::new();
    // An interpreter named python3 that is really sh, so the hook runs without Python installed
    let interpreter = root.join("bin/python3");
    std::fs::create_dir_all(interpreter.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(which_sh(), &interpreter).unwrap();
    let original = format!("#!{}\nexit 4\n", interpreter.display());
    write_executable(&hook, &original);
    let target = hooks::resolve(&root).unwrap();
    assert_eq!(hooks::status(&target), HookStatus::Foreign);

    let err = hooks::install_with(&target, &options(ForeignPolicy::Refuse)).unwrap_err();
    let HookError::ForeignHook {
        interpreter,
        snippet,
        ..
    } = &err
    else {
        panic!("expected a refusal, got {err}");
    };
    assert_eq!(interpreter, "python3");
    assert!(
        snippet.contains("check --fail-fast --mute-success"),
        "{snippet}"
    );
    assert_eq!(read(&hook), original, "refusing leaves the hook alone");

    let outcome = hooks::install_with(&target, &options(ForeignPolicy::Chain)).unwrap();
    let local = hook.with_file_name("pre-commit.local");
    assert_eq!(
        outcome,
        InstallOutcome::Chained {
            original: local.clone()
        }
    );
    assert_eq!(read(&local), original);
    assert_eq!(hooks::status(&target), HookStatus::Installed);
    assert_eq!(
        shim.run(&hook, &root, 0),
        4,
        "the original's status passes through"
    );
    assert_eq!(shim.run(&hook, &root, 2), 2, "fnug runs first");

    hooks::remove(&root).unwrap();
    assert_eq!(read(&hook), original);
    assert!(!local.exists());
}

fn which_sh() -> PathBuf {
    let output = Command::new("sh")
        .args(["-c", "command -v sh"])
        .output()
        .unwrap();
    PathBuf::from(String::from_utf8(output.stdout).unwrap().trim())
}

#[test]
fn user_comment_mentioning_fnug_untouched() {
    let (_tmp, root, hook) = repo();
    let original = "#!/bin/sh\n# fnug runs in CI, this hook only formats\ncargo fmt --check\n";
    write_executable(&hook, original);

    assert!(!hooks::is_installed(&root));
    hooks::remove(&root).unwrap();
    assert_eq!(read(&hook), original);

    hooks::install(&root, false).unwrap();
    hooks::remove(&root).unwrap();
    assert_eq!(read(&hook), original, "remove restores the hook exactly");
}

#[test]
fn legacy_block_migrated_in_place() {
    for legacy in [
        "# fnug\nfnug check --fail-fast --mute-success\n",
        "# fnug\nfnug check --fail-fast --mute-success --no-workspace\n",
        "# fnug\nfnug --no-workspace check --fail-fast --mute-success\n",
    ] {
        let (_tmp, root, hook) = repo();
        let shim = Shim::new();
        write_executable(&hook, &format!("#!/bin/sh\necho first\nfalse\n\n{legacy}"));
        let target = hooks::resolve(&root).unwrap();
        assert_eq!(hooks::status(&target), HookStatus::Outdated);

        hooks::install(&root, false).unwrap();

        let content = read(&hook);
        assert!(!content.contains("\n# fnug\n"), "{content}");
        assert_eq!(content.matches(BEGIN).count(), 1, "{content}");
        assert!(
            content.starts_with("#!/bin/sh\n# >>> fnug >>>\n"),
            "{content}"
        );
        assert!(content.contains("echo first\nfalse\n"), "{content}");
        assert_eq!(
            shim.run(&hook, &root, 0),
            1,
            "the old hook's failure now counts"
        );
        assert_eq!(hooks::status(&target), HookStatus::Installed);
    }
}

#[test]
fn legacy_only_hook_is_removed() {
    let (_tmp, root, hook) = repo();
    write_executable(
        &hook,
        "#!/bin/sh\n# fnug\nfnug check --fail-fast --mute-success\n",
    );
    assert!(hooks::is_installed(&root));
    hooks::remove(&root).unwrap();
    assert!(!hook.exists());
}

#[test]
fn reinstall_keeps_block_position() {
    let (_tmp, root, hook) = repo();
    let shim = Shim::new();
    write_executable(&hook, "#!/bin/sh\necho one\n");
    hooks::install(&root, false).unwrap();

    // The user moves the block below their own line
    let content = read(&hook);
    let block_start = content.find(BEGIN).unwrap();
    let block = &content[block_start..content.find("echo one").unwrap()];
    let moved = format!("#!/bin/sh\necho one\n{block}");
    std::fs::write(&hook, &moved).unwrap();

    hooks::install(&root, true).unwrap();

    let content = read(&hook);
    assert!(
        content.starts_with("#!/bin/sh\necho one\n# >>> fnug >>>\n"),
        "{content}"
    );
    assert_eq!(content.matches(BEGIN).count(), 1, "{content}");
    assert_eq!(shim.run(&hook, &root, 0), 0);
    assert_eq!(shim.args(), hooks::hook_args(true));
}

#[test]
fn remove_keeps_other_hook_lines() {
    let (_tmp, root, hook) = repo();
    let original = "#!/bin/sh\nnpx lint-staged\n";
    write_executable(&hook, original);

    hooks::install(&root, false).unwrap();
    assert!(hooks::is_installed(&root));
    hooks::remove(&root).unwrap();

    assert_eq!(read(&hook), original);
    assert!(!hooks::is_installed(&root));
}

#[test]
fn remove_deletes_a_hook_fnug_created() {
    let (_tmp, root, hook) = repo();
    hooks::install(&root, false).unwrap();
    hooks::install(&root, false).unwrap();
    assert_eq!(read(&hook).matches(BEGIN).count(), 1);

    hooks::remove(&root).unwrap();
    assert!(!hook.exists());
}

#[test]
fn newer_hook_format_is_outdated() {
    let (_tmp, root, hook) = repo();
    hooks::install(&root, false).unwrap();
    let target = hooks::resolve(&root).unwrap();
    assert_eq!(hooks::status(&target), HookStatus::Installed);

    let old = read(&hook).replace(
        &format!("# fnug-hook-version: {}", hooks::HOOK_FORMAT_VERSION),
        "# fnug-hook-version: 0",
    );
    std::fs::write(&hook, old).unwrap();
    assert_eq!(hooks::status(&target), HookStatus::Outdated);
    assert!(hooks::is_installed(&root));
}
