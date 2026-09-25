//! Tests for auto-selection: git scopes, path and regex matching, and selection issues.

use std::path::{Path, PathBuf};

use fnug::load_config;
use fnug::selectors::{
    SelectOptions, SelectedBy, SelectionIssue, SelectorOutput, get_selected_commands, select,
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
