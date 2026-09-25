use std::path::Path;

use fnug::load_config;
use fnug::selectors::{SelectorError, get_selected_commands};
use git2::{IndexAddOption, Repository, RepositoryInitOptions, Signature};

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

/// Names of the commands selected for the config at `config_path`.
fn select(config_path: &Path) -> Result<Vec<String>, SelectorError> {
    let (config, _) = load_config(Some(config_path.to_str().unwrap()), true).unwrap();
    let commands = config.all_commands().into_iter().cloned().collect();
    Ok(get_selected_commands(commands)?
        .into_iter()
        .map(|c| c.name)
        .collect())
}

fn selected(config_path: &Path) -> Vec<String> {
    select(config_path).unwrap_or_else(|e| panic!("selection failed: {e}"))
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
fn git_selection_in_bare_repo_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    Repository::init_bare(root.join("bare")).unwrap();
    std::fs::write(
        root.join(".fnug.yaml"),
        GIT_CONFIG.replace("    auto:", "    cwd: bare\n    auto:"),
    )
    .unwrap();

    let err = select(&root.join(".fnug.yaml")).unwrap_err();
    assert!(
        err.to_string()
            .contains("bare repository has no working tree"),
        "{err}"
    );
}
