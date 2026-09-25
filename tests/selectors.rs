//! Tests for auto-selection: git scopes, path and regex matching, selection issues, and file
//! watching.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use fnug::commands::command::Command;
use fnug::load_config;
use fnug::selectors::watch::watch_commands;
use fnug::selectors::{
    GitScope, IndexOverride, SelectOptions, SelectedBy, SelectionIssue, SelectorOutput,
    get_selected_commands, select,
};
use git2::{IndexAddOption, Repository, RepositoryInitOptions, RepositoryOpenFlags, Signature};

const GIT_CONFIG: &str = r"
fnug_version: 0.1.0
name: root
commands:
  - name: lint
    cmd: 'true'
    auto:
      git: true
";

fn commit_all(repo: &Repository) {
    let mut index = repo.index().unwrap();
    index.add_all(["*"], IndexAddOption::DEFAULT, None).unwrap();
    index.update_all(["*"], None).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let sig = Signature::now("fnug", "fnug@example.com").unwrap();
    let parent = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
    let parents: Vec<_> = parent.iter().collect();
    repo.commit(Some("HEAD"), &sig, &sig, "commit", &tree, &parents)
        .unwrap();
}

/// A repo whose gitdir lives outside its work tree, linked by a `.git` file.
fn init_gitlink_repo(gitdir: &Path, workdir: &Path) -> Repository {
    Repository::init_opts(
        gitdir,
        RepositoryInitOptions::new()
            .no_dotgit_dir(true)
            .workdir_path(workdir),
    )
    .unwrap()
}

/// Write `yaml` as the config in `dir`, returning its path.
fn write_config(dir: &Path, yaml: &str) -> PathBuf {
    let path = dir.join(".fnug.yaml");
    std::fs::write(&path, yaml).unwrap();
    path
}

/// Run selection with `opts` on the config at `config_path`.
fn select_with(config_path: &Path, opts: &SelectOptions) -> SelectorOutput {
    let (config, _) = load_config(Some(config_path.to_str().unwrap()), true).unwrap();
    select(&config.all_commands(), opts)
}

/// Ids of the commands selected in the working tree for the config at `config_path`.
fn selected(config_path: &Path) -> Vec<String> {
    select_with(config_path, &SelectOptions::default())
        .ids()
        .map(String::from)
        .collect()
}

#[test]
fn git_selection_in_linked_worktree() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let main = root.join("main");
    std::fs::create_dir(&main).unwrap();
    let repo = Repository::init(&main).unwrap();
    std::fs::write(main.join(".fnug.yaml"), GIT_CONFIG).unwrap();
    std::fs::write(main.join("a.txt"), "one\n").unwrap();
    commit_all(&repo);

    let wt = root.join("wt");
    repo.worktree("wt", &wt, None).unwrap();
    let config = wt.join(".fnug.yaml");
    assert!(
        selected(&config).is_empty(),
        "clean worktree selects nothing"
    );

    std::fs::write(wt.join("a.txt"), "two\n").unwrap();
    assert_eq!(selected(&config), ["lint"]);
}

#[test]
fn git_selection_in_gitlink_submodule() {
    let tmp = tempfile::tempdir().unwrap();
    let outer = tmp.path().canonicalize().unwrap();
    let outer_repo = Repository::init(&outer).unwrap();
    std::fs::write(
        outer.join(".fnug.yaml"),
        GIT_CONFIG.replace("    auto:", "    cwd: sub\n    auto:"),
    )
    .unwrap();
    commit_all(&outer_repo);

    let sub = outer.join("sub");
    let inner = init_gitlink_repo(&outer.join(".git/modules/sub"), &sub);
    std::fs::write(sub.join("a.txt"), "one\n").unwrap();
    commit_all(&inner);
    let config = outer.join(".fnug.yaml");
    assert!(
        selected(&config).is_empty(),
        "clean submodule selects nothing"
    );

    std::fs::write(sub.join("new.txt"), "untracked\n").unwrap();
    assert_eq!(selected(&config), ["lint"]);
}

#[test]
fn git_selection_with_separate_git_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let proj = root.join("proj");
    let repo = init_gitlink_repo(&root.join("proj.git"), &proj);
    // Like `git init --separate-git-dir`: only the `.git` file names the work tree.
    repo.config().unwrap().remove("core.worktree").unwrap();
    std::fs::write(proj.join(".fnug.yaml"), GIT_CONFIG).unwrap();
    commit_all(&repo);
    let config = proj.join(".fnug.yaml");
    assert!(selected(&config).is_empty(), "clean repo selects nothing");

    std::fs::write(proj.join("new.txt"), "untracked\n").unwrap();
    assert_eq!(selected(&config), ["lint"]);
}

#[test]
fn git_selection_in_bare_repo_reports_path() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    Repository::init_bare(root.join("bare")).unwrap();
    let config = write_config(
        &root,
        &GIT_CONFIG.replace("    auto:", "    cwd: bare\n    auto:"),
    );

    let output = select_with(&config, &SelectOptions::default());
    assert!(output.commands.is_empty());
    let [issue] = output.issues.as_slice() else {
        panic!("{:?}", output.issues);
    };
    assert!(!issue.is_fatal());
    let SelectionIssue::NotInRepo {
        path, command_ids, ..
    } = issue
    else {
        panic!("{issue:?}");
    };
    assert_eq!(path, &root.join("bare"));
    assert_eq!(command_ids, &["lint"]);
    let message = issue.to_string();
    assert!(message.contains("bare repository"), "{message}");
    assert!(
        message.contains(&root.join("bare").display().to_string()),
        "{message}"
    );
}

fn outside_any_repo(dir: &Path) -> bool {
    Repository::open_ext(dir, RepositoryOpenFlags::CROSS_FS, &[] as &[&Path]).is_err()
}

#[test]
fn always_survives_git_error() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    if !outside_any_repo(&root) {
        eprintln!("skipping: the temp dir is inside a git repo");
        return;
    }
    let config = write_config(
        &root,
        r"
name: root
commands:
  - name: always
    cmd: 'true'
    auto:
      always: true
  - name: lint
    cmd: 'true'
    auto:
      git: true
",
    );

    let output = select_with(&config, &SelectOptions::default());
    assert_eq!(output.ids().collect::<Vec<_>>(), ["always"]);
    assert!(!output.has_fatal());
    assert!(
        matches!(
            output.issues.as_slice(),
            [SelectionIssue::NotInRepo { path, command_ids, .. }]
                if path == &root && command_ids == &["lint"]
        ),
        "{:?}",
        output.issues
    );

    // The compatibility wrapper logs the issue instead of failing.
    let (loaded, _) = load_config(Some(config.to_str().unwrap()), true).unwrap();
    let commands = loaded.all_commands().into_iter().cloned().collect();
    let names: Vec<String> = get_selected_commands(commands)
        .unwrap()
        .into_iter()
        .map(|c| c.name)
        .collect();
    assert_eq!(names, ["always"]);
}

#[test]
fn git_error_in_one_path_keeps_other_repos() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    if !outside_any_repo(&root) {
        eprintln!("skipping: the temp dir is inside a git repo");
        return;
    }
    std::fs::create_dir(root.join("a")).unwrap();
    std::fs::create_dir(root.join("b")).unwrap();
    Repository::init(root.join("a")).unwrap();
    std::fs::write(root.join("a/new.txt"), "untracked\n").unwrap();
    let config = write_config(
        &root,
        r"
name: root
commands:
  - name: a-lint
    cmd: 'true'
    cwd: a
    auto:
      git: true
  - name: b-lint
    cmd: 'true'
    cwd: b
    auto:
      git: true
",
    );

    assert_eq!(selected(&config), ["a-lint"]);
}

#[test]
fn unreadable_index_is_scan_failure() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    for name in ["good", "broken"] {
        let dir = root.join(name);
        std::fs::create_dir(&dir).unwrap();
        let repo = Repository::init(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        commit_all(&repo);
        std::fs::write(dir.join("a.txt"), "two\n").unwrap();
    }
    std::fs::write(root.join("broken/.git/index"), "not an index").unwrap();
    let config = write_config(
        &root,
        r"
name: root
auto:
  git: true
commands:
  - name: good-lint
    cmd: 'true'
    cwd: good
  - name: broken-lint
    cmd: 'true'
    cwd: broken
",
    );

    let output = select_with(&config, &SelectOptions::default());
    assert_eq!(output.ids().collect::<Vec<_>>(), ["good-lint"]);
    assert!(
        matches!(
            output.issues.as_slice(),
            [SelectionIssue::ScanFailed { repo, .. }] if repo == &root.join("broken")
        ),
        "{:?}",
        output.issues
    );
    assert!(!output.has_fatal());
}

#[test]
fn files_lists_existing_matches() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let repo = Repository::init(&root).unwrap();
    std::fs::create_dir(root.join("src")).unwrap();
    std::fs::write(root.join("src/modified.rs"), "one\n").unwrap();
    std::fs::write(root.join("src/deleted.rs"), "one\n").unwrap();
    std::fs::write(root.join("src/clean.rs"), "one\n").unwrap();
    let config = write_config(
        &root,
        r"
name: root
commands:
  - name: lint
    cmd: 'true'
    auto:
      git: true
      path: [src]
",
    );
    commit_all(&repo);
    std::fs::write(root.join("src/modified.rs"), "two\n").unwrap();
    std::fs::write(root.join("src/untracked.rs"), "new\n").unwrap();
    std::fs::remove_file(root.join("src/deleted.rs")).unwrap();

    let output = select_with(&config, &SelectOptions::default());
    let lint = output.get("lint").expect("lint is selected");
    assert_eq!(lint.by, SelectedBy::Git);
    assert_eq!(
        lint.files,
        [root.join("src/modified.rs"), root.join("src/untracked.rs")]
    );
    assert_eq!(output.changed_files, 3);
    assert!(output.issues.is_empty(), "{:?}", output.issues);
}

#[test]
fn files_leave_out_directories() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let repo = Repository::init(&root).unwrap();
    let config = write_config(&root, GIT_CONFIG);
    commit_all(&repo);
    // Git reports an untracked nested repo as one directory entry.
    Repository::init(root.join("nested")).unwrap();
    std::fs::write(root.join("nested/inner.rs"), "new\n").unwrap();

    let output = select_with(&config, &SelectOptions::default());
    let lint = output.get("lint").expect("the nested repo selects lint");
    assert!(lint.files.is_empty(), "{:?}", lint.files);

    std::fs::write(root.join("new.rs"), "new\n").unwrap();
    let output = select_with(&config, &SelectOptions::default());
    assert_eq!(output.get("lint").unwrap().files, [root.join("new.rs")]);
}

#[test]
fn worktree_scope_kinds() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let repo = Repository::init(&root).unwrap();
    for dir in ["modified", "deleted", "untracked", "clean"] {
        std::fs::create_dir(root.join(dir)).unwrap();
        std::fs::write(root.join(dir).join("keep.txt"), "one\n").unwrap();
    }
    std::fs::write(root.join("deleted/gone.txt"), "one\n").unwrap();
    let config = write_config(
        &root,
        r"
name: root
auto:
  git: true
commands:
  - name: modified
    cmd: 'true'
    cwd: modified
  - name: deleted
    cmd: 'true'
    cwd: deleted
  - name: untracked
    cmd: 'true'
    cwd: untracked
  - name: clean
    cmd: 'true'
    cwd: clean
",
    );
    commit_all(&repo);
    std::fs::write(root.join("modified/keep.txt"), "two\n").unwrap();
    std::fs::remove_file(root.join("deleted/gone.txt")).unwrap();
    std::fs::write(root.join("untracked/new.txt"), "new\n").unwrap();

    let output = select_with(&config, &SelectOptions::default());
    assert_eq!(
        output.ids().collect::<Vec<_>>(),
        ["modified", "deleted", "untracked"]
    );
    assert!(output.get("deleted").unwrap().files.is_empty());
}

#[test]
fn always_wins_and_keeps_git_files() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let repo = Repository::init(&root).unwrap();
    let config = write_config(
        &root,
        r"
name: root
commands:
  - name: both
    cmd: 'true'
    auto:
      always: true
      git: true
  - name: always
    cmd: 'true'
    auto:
      always: true
",
    );
    commit_all(&repo);
    std::fs::write(root.join("new.txt"), "new\n").unwrap();

    let output = select_with(&config, &SelectOptions::default());
    let both = output.get("both").unwrap();
    assert_eq!(both.by, SelectedBy::Always);
    assert_eq!(both.files, [root.join("new.txt")]);
    let always = output.get("always").unwrap();
    assert_eq!(always.by, SelectedBy::Always);
    assert!(always.files.is_empty());
}

#[test]
fn non_utf8_path_selected() {
    use std::os::unix::ffi::OsStrExt;

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let repo = Repository::init(&root).unwrap();
    let config = write_config(
        &root,
        r"
name: root
commands:
  - name: lint
    cmd: 'true'
    auto:
      git: true
      path: [data]
",
    );
    commit_all(&repo);
    std::fs::create_dir(root.join("data")).unwrap();
    let latin1 = root
        .join("data")
        .join(std::ffi::OsStr::from_bytes(b"caf\xe9.txt"));
    if std::fs::write(&latin1, "x\n").is_err() {
        eprintln!("skipping: the file system rejects non-UTF-8 names");
        return;
    }

    let output = select_with(&config, &SelectOptions::default());
    let lint = output.get("lint").expect("lint is selected");
    assert_eq!(lint.files, [latin1]);
}

#[test]
fn scan_is_limited_to_configured_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let repo = Repository::init(&root).unwrap();
    let config = write_config(
        &root,
        r"
name: root
auto:
  git: true
commands:
  - name: src
    cmd: 'true'
    auto:
      path: [src]
  - name: file
    cmd: 'true'
    auto:
      path: [docs/demo.tape]
",
    );
    commit_all(&repo);
    for dir in ["src", "src2", "docs", "other"] {
        std::fs::create_dir(root.join(dir)).unwrap();
    }
    std::fs::write(root.join("src2/a.rs"), "").unwrap();
    std::fs::write(root.join("docs/other.md"), "").unwrap();
    std::fs::write(root.join("other/b.rs"), "").unwrap();

    let output = select_with(&config, &SelectOptions::default());
    assert!(output.commands.is_empty(), "{:?}", output.commands);
    assert_eq!(output.changed_files, 0);

    std::fs::write(root.join("src/a.rs"), "").unwrap();
    std::fs::write(root.join("docs/demo.tape"), "").unwrap();
    let output = select_with(&config, &SelectOptions::default());
    assert_eq!(output.ids().collect::<Vec<_>>(), ["src", "file"]);
    assert_eq!(output.changed_files, 2);
}

#[test]
fn pathspec_literal_brackets() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let repo = Repository::init(&root).unwrap();
    let config = write_config(
        &root,
        r"
name: root
auto:
  git: true
commands:
  - name: pkg
    cmd: 'true'
    auto:
      path: ['pkg[1]']
  - name: src
    cmd: 'true'
    auto:
      path: [src]
",
    );
    std::fs::create_dir(root.join("pkg[1]")).unwrap();
    std::fs::write(root.join("pkg[1]/p.rs"), "one\n").unwrap();
    commit_all(&repo);
    std::fs::write(root.join("pkg[1]/p.rs"), "two\n").unwrap();
    std::fs::create_dir(root.join("pkg1")).unwrap();
    std::fs::write(root.join("pkg1/q.rs"), "").unwrap();

    let output = select_with(&config, &SelectOptions::default());
    assert_eq!(output.ids().collect::<Vec<_>>(), ["pkg"]);
    assert_eq!(output.get("pkg").unwrap().files, [root.join("pkg[1]/p.rs")]);
}

#[test]
fn path_inside_untracked_dir_selects() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let repo = Repository::init(&root).unwrap();
    let config = write_config(
        &root,
        r"
name: root
commands:
  - name: lint
    cmd: 'true'
    auto:
      git: true
      path: [new/sub]
",
    );
    commit_all(&repo);
    std::fs::create_dir_all(root.join("new/sub")).unwrap();
    std::fs::write(root.join("new/sub/a.txt"), "").unwrap();
    std::fs::write(root.join("new/b.txt"), "").unwrap();

    let output = select_with(&config, &SelectOptions::default());
    assert_eq!(
        output.get("lint").unwrap().files,
        [root.join("new/sub/a.txt")]
    );
}

#[test]
fn regex_anchored_to_cwd() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let repo = Repository::init(&root).unwrap();
    let config = write_config(
        &root,
        r"
name: root
auto:
  git: true
  regex: ['^src/.*\.rs$']
commands:
  - name: root-src
    cmd: 'true'
  - name: app-src
    cmd: 'true'
    cwd: app
",
    );
    std::fs::create_dir_all(root.join("app/src")).unwrap();
    std::fs::write(root.join("app/keep"), "").unwrap();
    commit_all(&repo);

    std::fs::write(root.join("app/src/lib.rs"), "").unwrap();
    assert_eq!(selected(&config), ["app-src"]);

    std::fs::create_dir(root.join("src")).unwrap();
    std::fs::write(root.join("src/main.rs"), "").unwrap();
    let output = select_with(&config, &SelectOptions::default());
    assert_eq!(output.ids().collect::<Vec<_>>(), ["root-src", "app-src"]);
    assert_eq!(
        output.get("root-src").unwrap().files,
        [root.join("src/main.rs")]
    );
}

#[test]
fn regex_ignores_parent_dir_names() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap().join("selectors");
    std::fs::create_dir(&root).unwrap();
    let repo = Repository::init(&root).unwrap();
    let config = write_config(
        &root,
        r"
name: root
commands:
  - name: lint
    cmd: 'true'
    auto:
      git: true
      regex: [selectors]
",
    );
    commit_all(&repo);
    std::fs::write(root.join("a.txt"), "").unwrap();

    assert!(selected(&config).is_empty());
}

#[test]
fn regex_outside_cwd_uses_dotdot() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let repo = Repository::init(&root).unwrap();
    let config = write_config(
        &root,
        r"
name: root
commands:
  - name: lint
    cmd: 'true'
    cwd: app
    auto:
      git: true
      path: [., ../shared]
      regex: ['^\.\./shared/.*\.rs$']
",
    );
    for dir in ["app", "shared"] {
        std::fs::create_dir(root.join(dir)).unwrap();
        std::fs::write(root.join(dir).join("keep"), "").unwrap();
    }
    commit_all(&repo);
    std::fs::write(root.join("app/x.rs"), "").unwrap();
    assert!(selected(&config).is_empty());

    std::fs::write(root.join("shared/x.rs"), "").unwrap();
    assert_eq!(selected(&config), ["lint"]);
}

const LEGACY_CONFIG: &str = r"
name: root
commands:
  - name: always
    cmd: echo ALWAYS-RAN
    auto:
      always: true
  - name: legacy
    cmd: echo LEGACY-RAN
    auto:
      git: true
      path: [./legacy, ./legacy/gone.txt]
";

/// A repo with a committed `legacy/` directory, which is then deleted from disk and, if
/// `staged`, from the index (like `git rm -r legacy`).
fn repo_with_deleted_legacy_dir(staged: bool) -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let repo = Repository::init(&root).unwrap();
    std::fs::create_dir(root.join("legacy")).unwrap();
    std::fs::write(root.join("legacy/gone.txt"), "one\n").unwrap();
    let config = write_config(&root, LEGACY_CONFIG);
    commit_all(&repo);
    std::fs::remove_dir_all(root.join("legacy")).unwrap();
    if staged {
        let mut index = repo.index().unwrap();
        index.remove_dir(Path::new("legacy"), 0).unwrap();
        index.write().unwrap();
    }
    (tmp, config)
}

#[test]
fn deleted_auto_path_dir_selects() {
    for staged in [false, true] {
        let (_tmp, config) = repo_with_deleted_legacy_dir(staged);
        let output = select_with(&config, &SelectOptions::default());
        assert_eq!(
            output.ids().collect::<Vec<_>>(),
            ["always", "legacy"],
            "staged: {staged}, issues: {:?}",
            output.issues
        );
        let legacy = output.get("legacy").unwrap();
        assert_eq!(legacy.by, SelectedBy::Git);
        assert!(legacy.files.is_empty());
        assert!(output.issues.is_empty(), "{:?}", output.issues);
    }
}

#[test]
fn check_runs_command_whose_auto_path_was_deleted() {
    let (tmp, _config) = repo_with_deleted_legacy_dir(true);
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_fnug"))
        .current_dir(tmp.path())
        .args(["--no-workspace", "check", "--no-tui"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(stdout.contains("ALWAYS-RAN"), "{stdout}");
    assert!(stdout.contains("LEGACY-RAN"), "{stdout}");
}

fn staged() -> SelectOptions {
    SelectOptions {
        scope: GitScope::Staged,
        index_override: None,
    }
}

fn stage(repo: &Repository, paths: &[&str]) {
    let mut index = repo.index().unwrap();
    for path in paths {
        index.add_path(Path::new(path)).unwrap();
    }
    index.write().unwrap();
}

/// One command per directory, each selected by any change under it.
const DIRS_CONFIG: &str = r"
name: root
auto:
  git: true
commands:
  - name: modified
    cmd: 'true'
    cwd: modified
  - name: untracked
    cmd: 'true'
    cwd: untracked
  - name: added
    cmd: 'true'
    cwd: added
  - name: removed
    cmd: 'true'
    cwd: removed
";

fn dirs_repo() -> (tempfile::TempDir, PathBuf, Repository) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let repo = Repository::init(&root).unwrap();
    for dir in ["modified", "untracked", "added", "removed"] {
        std::fs::create_dir(root.join(dir)).unwrap();
        std::fs::write(root.join(dir).join("keep.txt"), "one\n").unwrap();
    }
    write_config(&root, DIRS_CONFIG);
    commit_all(&repo);
    (tmp, root, repo)
}

#[test]
fn staged_scope_ignores_unstaged_and_untracked() {
    let (_tmp, root, repo) = dirs_repo();
    let config = root.join(".fnug.yaml");
    std::fs::write(root.join("modified/keep.txt"), "two\n").unwrap();
    std::fs::write(root.join("untracked/new.txt"), "new\n").unwrap();
    assert!(select_with(&config, &staged()).commands.is_empty());

    std::fs::write(root.join("added/new.txt"), "new\n").unwrap();
    stage(&repo, &["added/new.txt"]);
    std::fs::remove_file(root.join("removed/keep.txt")).unwrap();
    let mut index = repo.index().unwrap();
    index.remove_path(Path::new("removed/keep.txt")).unwrap();
    index.write().unwrap();

    let output = select_with(&config, &staged());
    assert_eq!(output.ids().collect::<Vec<_>>(), ["added", "removed"]);
    assert_eq!(
        output.get("added").unwrap().files,
        [root.join("added/new.txt")]
    );
    assert!(output.get("removed").unwrap().files.is_empty());
    assert_eq!(output.changed_files, 2);

    // The working tree scope still sees everything.
    assert_eq!(
        selected(&config),
        ["modified", "untracked", "added", "removed"]
    );
}

#[test]
fn staged_unborn_head() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let repo = Repository::init(&root).unwrap();
    let config = write_config(&root, GIT_CONFIG);
    std::fs::write(root.join("unstaged.txt"), "").unwrap();
    assert!(select_with(&config, &staged()).commands.is_empty());

    stage(&repo, &[".fnug.yaml"]);
    let output = select_with(&config, &staged());
    assert_eq!(output.get("lint").unwrap().files, [config]);
    assert!(
        !output
            .issues
            .iter()
            .any(|issue| matches!(issue, SelectionIssue::ScanFailed { .. })),
        "{:?}",
        output.issues
    );
}

#[test]
fn staged_missing_index_override_is_scan_failure() {
    let (_tmp, root, repo) = dirs_repo();
    let config = root.join(".fnug.yaml");
    let missing = root.join(".git/no-such-index");
    let opts = SelectOptions {
        scope: GitScope::Staged,
        index_override: Some(IndexOverride {
            git_dir: repo.path().to_path_buf(),
            index_file: missing.clone(),
        }),
    };

    // libgit2 reads a missing index as an empty one, which would stage-delete every file.
    let output = select_with(&config, &opts);
    assert!(output.commands.is_empty(), "{:?}", output.commands);
    let [SelectionIssue::ScanFailed { repo, message }] = output.issues.as_slice() else {
        panic!("{:?}", output.issues);
    };
    assert_eq!(repo, &root);
    assert!(
        message.contains(&missing.display().to_string()),
        "{message}"
    );
}

#[test]
fn staged_index_override() {
    let (_tmp, root, repo) = dirs_repo();
    let config = root.join(".fnug.yaml");
    std::fs::write(root.join("added/new.txt"), "new\n").unwrap();

    // Like `git commit -a`: git stages into a temporary index and leaves the real one alone.
    let alt_index = root.join(".git/next-index.lock");
    std::fs::copy(root.join(".git/index"), &alt_index).unwrap();
    let mut alt = git2::Index::open(&alt_index).unwrap();
    let owner = Repository::open(&root).unwrap();
    owner.set_index(&mut alt).unwrap();
    alt.add_path(Path::new("added/new.txt")).unwrap();
    alt.write().unwrap();
    drop(owner);

    assert!(select_with(&config, &staged()).commands.is_empty());

    let with_index = |git_dir: PathBuf| SelectOptions {
        scope: GitScope::Staged,
        index_override: Some(IndexOverride {
            git_dir,
            index_file: alt_index.clone(),
        }),
    };
    let output = select_with(&config, &with_index(repo.path().to_path_buf()));
    assert_eq!(output.ids().collect::<Vec<_>>(), ["added"]);
    assert_eq!(
        output.get("added").unwrap().files,
        [root.join("added/new.txt")]
    );

    // The override belongs to another repo, so this one reads its own index.
    let other = tempfile::tempdir().unwrap();
    let other_repo = Repository::init(other.path()).unwrap();
    let output = select_with(&config, &with_index(other_repo.path().to_path_buf()));
    assert!(output.commands.is_empty());

    // Only the staged scope reads it.
    let output = select_with(
        &config,
        &SelectOptions {
            scope: GitScope::WorkingTree,
            ..with_index(repo.path().to_path_buf())
        },
    );
    assert_eq!(output.ids().collect::<Vec<_>>(), ["added"]);
}

fn since(base: &str) -> SelectOptions {
    SelectOptions {
        scope: GitScope::Since(base.to_string()),
        index_override: None,
    }
}

fn head_commit(repo: &Repository) -> git2::Commit<'_> {
    repo.head().unwrap().peel_to_commit().unwrap()
}

#[test]
fn since_includes_committed_and_uncommitted() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let repo = Repository::init(&root).unwrap();
    let dirs = [
        "committed",
        "from",
        "to",
        "deleted",
        "staged",
        "unstaged",
        "untracked",
        "clean",
    ];
    let mut config = String::from("name: root\nauto:\n  git: true\ncommands:\n");
    for dir in dirs {
        std::fs::create_dir(root.join(dir)).unwrap();
        std::fs::write(root.join(dir).join("keep.txt"), "one\n").unwrap();
        writeln!(config, "  - name: {dir}\n    cmd: 'true'\n    cwd: {dir}").unwrap();
    }
    std::fs::write(root.join("from/old.txt"), "moved\n").unwrap();
    std::fs::write(root.join("deleted/gone.txt"), "one\n").unwrap();
    let config = write_config(&root, &config);
    commit_all(&repo);
    repo.branch("base", &head_commit(&repo), false).unwrap();

    std::fs::write(root.join("committed/keep.txt"), "two\n").unwrap();
    std::fs::rename(root.join("from/old.txt"), root.join("to/new.txt")).unwrap();
    std::fs::remove_file(root.join("deleted/gone.txt")).unwrap();
    commit_all(&repo);
    std::fs::write(root.join("staged/new.txt"), "new\n").unwrap();
    stage(&repo, &["staged/new.txt"]);
    std::fs::write(root.join("unstaged/keep.txt"), "two\n").unwrap();
    std::fs::write(root.join("untracked/new.txt"), "new\n").unwrap();

    let output = select_with(&config, &since("base"));
    assert_eq!(
        output.ids().collect::<Vec<_>>(),
        [
            "committed",
            "from",
            "to",
            "deleted",
            "staged",
            "unstaged",
            "untracked"
        ]
    );
    assert!(output.get("from").unwrap().files.is_empty());
    assert_eq!(output.get("to").unwrap().files, [root.join("to/new.txt")]);
    assert!(output.issues.is_empty(), "{:?}", output.issues);

    // The working tree scope sees only the uncommitted part.
    assert_eq!(selected(&config), ["staged", "unstaged", "untracked"]);
}

#[test]
fn since_unknown_ref_is_fatal_issue() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let repo = Repository::init(&root).unwrap();
    let config = write_config(&root, GIT_CONFIG);
    commit_all(&repo);
    std::fs::write(root.join("new.txt"), "").unwrap();

    let output = select_with(&config, &since("origin/nope"));
    assert!(output.commands.is_empty());
    assert!(output.has_fatal());
    let [issue @ SelectionIssue::BaseRefNotFound { repo, base, .. }] = output.issues.as_slice()
    else {
        panic!("{:?}", output.issues);
    };
    assert_eq!(repo, &root);
    assert_eq!(base, "origin/nope");
    let message = issue.to_string();
    assert!(message.contains("origin/nope"), "{message}");
    assert!(message.contains("fetch-depth: 0"), "{message}");
}

#[test]
fn since_unborn_head_is_fatal_issue() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    Repository::init(&root).unwrap();
    let config = write_config(&root, GIT_CONFIG);

    let output = select_with(&config, &since("main"));
    assert!(output.commands.is_empty());
    assert!(
        matches!(
            output.issues.as_slice(),
            [issue @ SelectionIssue::UnbornHead { repo }] if repo == &root && issue.is_fatal()
        ),
        "{:?}",
        output.issues
    );
}

#[test]
fn since_unrelated_history_is_fatal_issue() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let repo = Repository::init(&root).unwrap();
    let config = write_config(&root, GIT_CONFIG);
    commit_all(&repo);
    let sig = Signature::now("fnug", "fnug@example.com").unwrap();
    let empty = repo
        .find_tree(git2::Index::new().unwrap().write_tree_to(&repo).unwrap())
        .unwrap();
    repo.commit(Some("refs/heads/orphan"), &sig, &sig, "orphan", &empty, &[])
        .unwrap();

    let output = select_with(&config, &since("orphan"));
    assert!(
        matches!(
            output.issues.as_slice(),
            [SelectionIssue::BaseRefNotFound { message, .. }] if message.contains("no common history")
        ),
        "{:?}",
        output.issues
    );
}

#[test]
fn since_in_shallow_clone_says_so() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let repo = Repository::init(&root).unwrap();
    let config = write_config(&root, GIT_CONFIG);
    commit_all(&repo);
    repo.branch("base", &head_commit(&repo), false).unwrap();
    std::fs::write(root.join("a.txt"), "").unwrap();
    commit_all(&repo);
    // A depth-1 clone: HEAD's parent, the base, was never fetched.
    std::fs::write(
        root.join(".git/shallow"),
        format!("{}\n", head_commit(&repo).id()),
    )
    .unwrap();

    let output = select_with(&config, &since("base"));
    assert!(
        matches!(
            output.issues.as_slice(),
            [SelectionIssue::BaseRefNotFound { message, .. }] if message.contains("shallow")
        ),
        "{:?}",
        output.issues
    );
}

/// The commands of the config at `config_path`, for the watcher.
fn watched_commands(config_path: &Path) -> Vec<Command> {
    let (config, _) = load_config(Some(config_path.to_str().unwrap()), true).unwrap();
    config.all_commands().into_iter().cloned().collect()
}

const WATCH_SRC_CONFIG: &str = r"
name: root
commands:
  - name: test
    cmd: 'true'
    auto:
      watch: true
      path: [./src]
";

#[tokio::test(flavor = "multi_thread")]
async fn watch_latency_under_2s() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    std::fs::create_dir(root.join("src")).unwrap();
    let config = write_config(&root, WATCH_SRC_CONFIG);
    let (mut events, _watcher) = watch_commands(watched_commands(&config)).unwrap();

    std::fs::write(root.join("src/a.txt"), "").unwrap();
    let batch = tokio::time::timeout(Duration::from_secs(2), events.recv())
        .await
        .expect("no watch event within 2s")
        .unwrap();
    let ids: Vec<&str> = batch.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids, ["test"]);
}
