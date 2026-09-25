//! Tests for config loading: parsing, validation and inheritance.

use std::path::Path;
use std::sync::{Mutex, Once};

use fnug::commands::command::Command;
use fnug::commands::group::CommandGroup;
use fnug::commands::ids::{ResolveError, resolve_command};
use fnug::{LoadOptions, load_config};

fn write_config(dir: &Path, content: &str) -> String {
    let path = dir.join(".fnug.yaml");
    std::fs::write(&path, content).unwrap();
    path.to_string_lossy().into_owned()
}

fn load(content: &str) -> (tempfile::TempDir, CommandGroup) {
    let dir = tempfile::tempdir().unwrap();
    let path = write_config(dir.path(), content);
    let (config, _) = load_config(Some(&path), true).unwrap();
    (dir, config)
}

fn load_err(content: &str) -> String {
    let dir = tempfile::tempdir().unwrap();
    let path = write_config(dir.path(), content);
    match load_config(Some(&path), true) {
        Ok(_) => panic!("config loaded:\n{content}"),
        Err(e) => e.to_string(),
    }
}

fn command<'a>(config: &'a CommandGroup, name: &str) -> &'a Command {
    config
        .all_commands()
        .into_iter()
        .find(|c| c.name == name)
        .unwrap_or_else(|| panic!("no command named {name}"))
}

#[test]
fn group_check_false_reaches_commands_without_triggers() {
    let (_dir, config) = load(
        r"
fnug_version: 0.1.0
name: root
auto:
  check: false
commands:
  - name: plain
    cmd: 'true'
children:
  - name: nested
    auto:
      always: true
    commands:
      - name: always-on
        cmd: 'true'
      - name: override
        cmd: 'true'
        auto:
          check: true
",
    );
    assert_eq!(command(&config, "plain").auto.check, Some(false));
    let always_on = &command(&config, "always-on").auto;
    assert_eq!(always_on.check, Some(false));
    assert_eq!(always_on.always, Some(true));
    assert_eq!(command(&config, "override").auto.check, Some(true));
}

#[test]
fn docs_demo_config_is_tui_only() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/docs/.fnug.yaml");
    let (config, _) = load_config(Some(path), true).unwrap();
    for cmd in config.all_commands() {
        assert_eq!(cmd.auto.check, Some(false), "{} runs in check", cmd.name);
    }
    assert_eq!(command(&config, "test-auto").auto.always, Some(true));
    assert_eq!(command(&config, "test-not-auto").auto.always, Some(false));
}

#[test]
fn missing_cmd_error_has_path_and_line() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_config(
        dir.path(),
        r"fnug_version: 0.1.0
name: root
children:
  - name: first
    commands:
      - name: ok
        cmd: 'true'
  - name: second
    commands:
      - name: broken
",
    );
    let err = load_config(Some(&path), true).unwrap_err().to_string();
    assert!(err.contains("children[1].commands[0]"), "{err}");
    assert!(err.contains("line"), "{err}");
}

#[test]
fn unknown_command_key_rejected_with_location() {
    let err = load_err(
        r"fnug_version: 0.1.0
name: root
commands:
  - name: setup
    cmd: 'true'
  - name: lint
    cmd: 'true'
    depends-on: [setup]
",
    );
    assert!(err.contains("commands[1]"), "{err}");
    assert!(err.contains("line 8"), "{err}");
    assert!(err.contains("did you mean `depends_on`?"), "{err}");
}

#[test]
fn unknown_root_key_rejected() {
    let err = load_err(
        r"fnug_version: 0.1.0
name: root
childern:
  - name: child
",
    );
    assert!(err.contains("unknown field `childern`"), "{err}");
    assert!(err.contains("did you mean `children`?"), "{err}");
}

#[test]
fn unknown_auto_key_hint() {
    let err = load_err(
        r"fnug_version: 0.1.0
name: root
commands:
  - name: lint
    cmd: 'false'
    auto:
      gti: true
",
    );
    assert!(err.contains("commands[0].auto"), "{err}");
    assert!(err.contains("did you mean `git`?"), "{err}");
}

#[test]
fn camel_case_key_hint() {
    let err = load_err(
        r"fnug_version: 0.1.0
name: root
commands:
  - name: lint
    cmd: 'true'
    dependsOn: [lint]
",
    );
    assert!(err.contains("did you mean `depends_on`?"), "{err}");
}

#[test]
fn unrelated_unknown_key_has_no_hint() {
    let err = load_err(
        r"fnug_version: 0.1.0
name: root
colour: blue
",
    );
    assert!(err.contains("unknown field `colour`"), "{err}");
    assert!(!err.contains("did you mean"), "{err}");
}

#[test]
fn yaml_merge_key_explained() {
    let err = load_err(
        r"fnug_version: 0.1.0
name: root
commands:
  - name: fmt
    cmd: 'true'
    auto: &defaults
      git: true
  - name: lint
    cmd: 'true'
    auto:
      <<: *defaults
",
    );
    assert!(err.contains("merge keys"), "{err}");
}

#[test]
fn inline_yaml_alias_still_works() {
    let (_dir, config) = load(
        r"fnug_version: 0.1.0
name: root
commands:
  - name: fmt
    cmd: 'true'
    auto: &defaults
      always: true
  - name: lint
    cmd: 'true'
    auto: *defaults
",
    );
    assert_eq!(command(&config, "lint").auto.always, Some(true));
}

#[test]
fn workspace_options_typo_rejected() {
    let err = load_err(
        r"fnug_version: 0.1.0
name: root
workspace:
  path: [packages/*]
",
    );
    assert!(err.contains("workspace"), "{err}");
    assert!(err.contains("did you mean `paths`?"), "{err}");
}

#[test]
fn workspace_paths_type_error_names_field() {
    let err = load_err(
        r"fnug_version: 0.1.0
name: root
workspace:
  paths: packages/*
",
    );
    assert!(err.contains("workspace.paths"), "{err}");
    assert!(!err.contains("untagged"), "{err}");
}

#[test]
fn workspace_bool_and_options_still_parse() {
    for workspace in ["true", "false", "{max_depth: 2}", "{paths: [a/*]}"] {
        let dir = tempfile::tempdir().unwrap();
        git2::Repository::init(dir.path()).unwrap();
        let path = write_config(
            dir.path(),
            &format!("fnug_version: 0.1.0\nname: root\nworkspace: {workspace}\n"),
        );
        load_config(Some(&path), false).unwrap();
    }
}

#[test]
fn fnug_version_optional() {
    let (_dir, config) = load("name: root\ncommands:\n  - name: a\n    cmd: 'true'\n");
    assert_eq!(config.name, "root");
}

#[test]
fn json_schema_key_accepted() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(".fnug.json");
    std::fs::write(
        &path,
        r#"{
  "$schema": "https://example.com/fnug.schema.json",
  "fnug_version": "0.1.0",
  "name": "root",
  "commands": [{"name": "a", "cmd": "true"}]
}"#,
    )
    .unwrap();
    load_config(path.to_str(), true).unwrap();
}

#[test]
fn json_unknown_key_hint() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(".fnug.json");
    std::fs::write(
        &path,
        r#"{"fnug_version": "0.1.0", "name": "root", "comands": []}"#,
    )
    .unwrap();
    let err = load_config(path.to_str(), true).unwrap_err().to_string();
    assert!(err.contains("did you mean `commands`?"), "{err}");
}

#[test]
fn schema_matches_committed_file() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("schema/fnug.schema.json");
    let committed = std::fs::read_to_string(&path).unwrap();
    assert!(
        committed == fnug::schema::config_schema_json(),
        "{} is stale; regenerate it with `cargo run --bin fnug -- schema > schema/fnug.schema.json`",
        path.display()
    );
}

#[test]
fn schema_denies_additional_properties() {
    let schema: serde_json::Value =
        serde_json::from_str(&fnug::schema::config_schema_json()).unwrap();
    assert_eq!(schema["additionalProperties"], false);
    for definition in ["ConfigCommand", "ConfigCommandGroup", "ConfigAuto"] {
        let def = &schema["definitions"][definition];
        assert_eq!(def["additionalProperties"], false, "{definition}");
    }
    let command = &schema["definitions"]["ConfigCommand"];
    assert!(command["properties"]["depends_on"].is_object());
    assert_eq!(command["required"], serde_json::json!(["name", "cmd"]));
}

#[test]
fn config_serializes_without_nulls() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_config(
        dir.path(),
        "name: root\ncommands:\n  - name: a\n    cmd: 'true'\n    auto:\n      git: true\n",
    );
    let config = fnug::config_file::Config::from_file(Path::new(&path)).unwrap();
    let yaml = serde_yaml::to_string(&config).unwrap();
    assert!(!yaml.contains("null"), "{yaml}");
    assert!(!yaml.contains('~'), "{yaml}");
}

#[test]
fn schema_subcommand_needs_no_config() {
    let dir = tempfile::tempdir().unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_fnug"))
        .current_dir(dir.path())
        .arg("schema")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        fnug::schema::config_schema_json()
    );
}

/// Full configs (with a top-level `name`) from the README's yaml blocks.
fn readme_configs() -> Vec<String> {
    let readme = include_str!("../README.md");
    let mut blocks = Vec::new();
    let mut current: Option<String> = None;
    for line in readme.lines() {
        match (&mut current, line) {
            (None, "```yaml") => current = Some(String::new()),
            (Some(block), "```") => {
                if block.lines().any(|l| l.starts_with("name:")) {
                    blocks.push(std::mem::take(block));
                }
                current = None;
            }
            (Some(block), _) => {
                block.push_str(line);
                block.push('\n');
            }
            (None, _) => {}
        }
    }
    blocks
}

#[test]
fn readme_and_dogfood_configs_parse() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for file in [".fnug.yaml", "docs/.fnug.yaml"] {
        fnug::config_file::Config::from_file(&root.join(file))
            .unwrap_or_else(|e| panic!("{file}: {e}"));
    }
    let blocks = readme_configs();
    assert!(blocks.len() >= 5, "found {} README configs", blocks.len());
    for block in blocks {
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(dir.path(), &block);
        fnug::config_file::Config::from_file(Path::new(&path))
            .unwrap_or_else(|e| panic!("{e}\n{block}"));
    }
}

fn commit_all(repo: &git2::Repository) {
    let mut index = repo.index().unwrap();
    index
        .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
        .unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let sig = git2::Signature::now("fnug", "fnug@example.com").unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "commit", &tree, &[])
        .unwrap();
}

const LOCKFILE_CONFIG: &str = r"
fnug_version: 0.1.0
name: root
children:
  - name: rust
    auto:
      git: true
      path: [./src]
      regex: ['\.rs$']
    commands:
      - name: lockfile
        cmd: 'true'
        auto:
          path: [./Cargo.toml]
          regex: []
      - name: sub
        cmd: 'true'
        auto:
          path: [./src/sub]
      - name: everything
        cmd: 'true'
        auto:
          path: []
";

/// A git repo with `src/sub/lib.rs` and `Cargo.toml` committed, and the lockfile config.
fn lockfile_repo() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    std::fs::create_dir_all(dir.path().join("src/sub")).unwrap();
    std::fs::write(dir.path().join("src/sub/lib.rs"), "").unwrap();
    std::fs::write(dir.path().join("Cargo.toml"), "").unwrap();
    let path = write_config(dir.path(), LOCKFILE_CONFIG);
    commit_all(&repo);
    (dir, path)
}

#[test]
fn regex_empty_list_clears_inherited() {
    let (dir, path) = lockfile_repo();
    let (config, _) = load_config(Some(&path), true).unwrap();
    assert!(command(&config, "lockfile").auto.regexes().is_empty());

    std::fs::write(dir.path().join("Cargo.toml"), "[package]\n").unwrap();
    let commands = config.all_commands().into_iter().cloned().collect();
    let selected: Vec<String> = fnug::selectors::get_selected_commands(commands)
        .unwrap()
        .into_iter()
        .map(|c| c.name)
        .collect();
    assert_eq!(selected, ["lockfile"]);
}

#[test]
fn path_override_keeps_inherited_regex() {
    let (dir, path) = lockfile_repo();
    let (config, _) = load_config(Some(&path), true).unwrap();
    let sub = &command(&config, "sub").auto;
    let root = dir.path().canonicalize().unwrap();
    assert_eq!(sub.paths(), [root.join("src/sub")]);
    assert_eq!(sub.regexes().len(), 1);
}

#[test]
fn missing_auto_path_does_not_fail_load() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("backend")).unwrap();
    let path = write_config(
        dir.path(),
        r"
name: root
children:
  - name: backend
    cwd: backend
    auto:
      git: true
      path: [srcc, ./legacy/../gone/x]
    commands:
      - name: lint
        cmd: 'true'
",
    );
    let (config, _) = load_config(Some(&path), true).unwrap();
    let backend = dir.path().canonicalize().unwrap().join("backend");
    assert_eq!(
        command(&config, "lint").auto.paths(),
        [backend.join("srcc"), backend.join("gone/x")]
    );
}

#[cfg(unix)]
#[test]
fn missing_auto_path_resolves_symlinked_ancestor() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("real");
    std::fs::create_dir(&real).unwrap();
    std::os::unix::fs::symlink(&real, dir.path().join("link")).unwrap();
    let path = write_config(
        dir.path(),
        "name: root\ncommands:\n  - name: a\n    cmd: 'true'\n    auto:\n      path: [link/missing]\n",
    );
    let (config, _) = load_config(Some(&path), true).unwrap();
    assert_eq!(
        command(&config, "a").auto.paths(),
        [real.canonicalize().unwrap().join("missing")]
    );
}

#[test]
fn missing_cwd_error_names_missing_dir() {
    let err = load_err(
        r"
name: root
children:
  - name: backend
    cwd: backendx
    commands:
      - name: lint
        cmd: 'true'
",
    );
    assert!(err.contains("backendx"), "{err}");
    assert!(err.contains("No such file"), "{err}");
    assert!(err.contains("root > backend"), "{err}");
}

#[test]
fn start_dir_search_finds_ancestor_config() {
    let dir = tempfile::tempdir().unwrap();
    write_config(dir.path(), "name: root\n");
    let nested = dir.path().join("a/b");
    std::fs::create_dir_all(&nested).unwrap();
    let loaded = fnug::load(&LoadOptions {
        start_dir: Some(nested),
        no_workspace: true,
        ..LoadOptions::default()
    })
    .unwrap();
    let root = dir.path().canonicalize().unwrap();
    assert_eq!(loaded.config_path, root.join(".fnug.yaml"));
    assert_eq!(loaded.cwd, root);
    assert_eq!(loaded.sources, [root.join(".fnug.yaml")]);
}

#[test]
fn start_dir_without_config_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let err = fnug::load(&LoadOptions {
        start_dir: Some(dir.path().to_path_buf()),
        no_workspace: true,
        ..LoadOptions::default()
    });
    // A config above the tempdir (e.g. in $TMPDIR's parents) would be found instead.
    if let Err(err) = err {
        assert!(
            matches!(err, fnug::config_file::ConfigError::ConfigNotFound(_)),
            "{err}"
        );
    }
}

#[test]
fn relative_config_resolves_against_start_dir() {
    let dir = tempfile::tempdir().unwrap();
    let sub = dir.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    std::fs::write(sub.join("ci.yaml"), "name: ci\n").unwrap();
    let loaded = fnug::load(&LoadOptions {
        config: Some("sub/ci.yaml".into()),
        start_dir: Some(dir.path().to_path_buf()),
        no_workspace: true,
        ..LoadOptions::default()
    })
    .unwrap();
    assert_eq!(loaded.root.name, "ci");
    assert_eq!(loaded.cwd, sub.canonicalize().unwrap());
}

#[test]
fn workspace_sources_list_every_config() {
    let dir = tempfile::tempdir().unwrap();
    git2::Repository::init(dir.path()).unwrap();
    write_config(dir.path(), "name: root\nworkspace: true\n");
    let pkg = dir.path().join("pkg");
    std::fs::create_dir(&pkg).unwrap();
    write_config(&pkg, "name: pkg\ncommands:\n  - name: a\n    cmd: 'true'\n");
    let loaded = fnug::load(&LoadOptions {
        config: Some(dir.path().join(".fnug.yaml")),
        ..LoadOptions::default()
    })
    .unwrap();
    let root = dir.path().canonicalize().unwrap();
    assert_eq!(
        loaded.sources,
        [root.join(".fnug.yaml"), root.join("pkg/.fnug.yaml")]
    );
}

#[test]
fn path_empty_list_resets_to_cwd() {
    let (dir, path) = lockfile_repo();
    let (config, _) = load_config(Some(&path), true).unwrap();
    let everything = &command(&config, "everything").auto;
    assert_eq!(everything.paths(), [dir.path().canonicalize().unwrap()]);
    assert_eq!(everything.regexes().len(), 1);
}

// ─── ids ───

fn ids(config: &CommandGroup) -> Vec<(String, String)> {
    config
        .all_commands()
        .into_iter()
        .map(|c| (c.name.clone(), c.id.clone()))
        .collect()
}

#[test]
fn ids_default_to_name() {
    let (_dir, config) = load(
        r"
name: root
commands:
  - name: fmt
    cmd: 'true'
  - name: clippy
    cmd: 'true'
    depends_on: [fmt]
",
    );
    assert_eq!(command(&config, "fmt").id, "fmt");
    assert_eq!(command(&config, "clippy").depends_on, ["fmt"]);
    assert_eq!(config.id, "root");
}

#[test]
fn ids_stable_across_loads() {
    let content = r"
name: root
children:
  - name: a
    commands:
      - name: fmt
        cmd: 'true'
  - name: b
    commands:
      - name: fmt
        cmd: 'true'
";
    let (_d1, first) = load(content);
    let (_d2, second) = load(content);
    assert_eq!(ids(&first), ids(&second));
    assert_eq!(first.children[0].id, second.children[0].id);
}

#[test]
fn name_slash_replaced_in_default_id() {
    let (_dir, config) = load("name: root\ncommands:\n  - name: a/b\n    cmd: 'true'\n");
    assert_eq!(command(&config, "a/b").id, "a-b");
}

#[test]
fn duplicate_names_are_group_qualified() {
    let (_dir, config) = load(
        r"
name: root
children:
  - name: backend
    commands:
      - name: test
        cmd: 'true'
      - name: lint
        cmd: 'true'
  - name: frontend
    commands:
      - name: test
        cmd: 'true'
        depends_on: [backend/test]
",
    );
    assert_eq!(
        ids(&config),
        [
            ("test".into(), "backend/test".into()),
            ("lint".into(), "lint".into()),
            ("test".into(), "frontend/test".into()),
        ]
    );
    assert_eq!(config.children[1].commands[0].depends_on, ["backend/test"]);
}

#[test]
fn explicit_id_wins_over_defaulted_name() {
    // This repo's own config: rust `fmt` has `id: rust-fmt`, so nix `fmt` keeps `fmt`.
    let (_dir, config) = load(
        r"
name: root
children:
  - name: rust
    commands:
      - name: fmt
        id: rust-fmt
        cmd: 'true'
  - name: nix
    commands:
      - name: fmt
        cmd: 'true'
      - name: after
        cmd: 'true'
        depends_on: [rust-fmt, fmt]
",
    );
    assert_eq!(command(&config, "after").depends_on, ["rust-fmt", "fmt"]);
    assert_eq!(config.children[1].commands[0].id, "fmt");
}

#[test]
fn depends_on_prefers_sibling() {
    let (_dir, config) = load(
        r"
name: root
children:
  - name: backend
    commands:
      - name: build
        cmd: 'true'
      - name: test
        cmd: 'true'
        depends_on: [build]
  - name: frontend
    commands:
      - name: build
        cmd: 'true'
      - name: test
        cmd: 'true'
        depends_on: [build]
",
    );
    assert_eq!(config.children[0].commands[1].depends_on, ["backend/build"]);
    assert_eq!(
        config.children[1].commands[1].depends_on,
        ["frontend/build"]
    );
}

#[test]
fn depends_on_root_command_does_not_shadow_sibling() {
    let content = r"
name: root
commands:
  - name: lint
    cmd: 'true'
children:
  - name: backend
    commands:
      - name: lint
        cmd: 'true'
      - name: test
        cmd: 'true'
        depends_on: [DEP]
";
    let err = load_err(&content.replace("DEP", "lint"));
    assert!(err.contains("root > backend > test"), "{err}");
    assert!(err.contains("'lint' (root > lint)"), "{err}");
    assert!(
        err.contains("'backend/lint' (root > backend > lint)"),
        "{err}"
    );

    let (_dir, config) = load(&content.replace("DEP", "backend/lint"));
    assert_eq!(config.children[0].commands[1].depends_on, ["backend/lint"]);
}

#[test]
fn depends_on_own_name_is_not_a_sibling() {
    let (_dir, config) = load(
        r"
name: root
commands:
  - name: lint
    cmd: 'true'
children:
  - name: backend
    commands:
      - name: lint
        cmd: 'true'
        depends_on: [lint]
",
    );
    assert_eq!(config.children[0].commands[0].depends_on, ["lint"]);
}

#[test]
fn depends_on_explicit_id_does_not_shadow_sibling() {
    let err = load_err(
        r"
name: root
children:
  - name: backend
    commands:
      - name: build
        id: build
        cmd: 'true'
  - name: frontend
    commands:
      - name: build
        cmd: 'true'
      - name: test
        cmd: 'true'
        depends_on: [build]
",
    );
    assert!(err.contains("root > frontend > test"), "{err}");
    assert!(err.contains("'build' (root > backend > build)"), "{err}");
    assert!(
        err.contains("'frontend/build' (root > frontend > build)"),
        "{err}"
    );
}

#[test]
fn depends_on_ambiguous_lists_candidates() {
    let err = load_err(
        r"
name: root
children:
  - name: backend
    commands:
      - name: build
        cmd: 'true'
  - name: frontend
    commands:
      - name: build
        cmd: 'true'
commands:
  - name: deploy
    cmd: 'true'
    depends_on: [build]
",
    );
    assert!(err.contains("'build'"), "{err}");
    assert!(err.contains("backend/build"), "{err}");
    assert!(err.contains("frontend/build"), "{err}");
}

#[test]
fn depends_on_unknown_names_id_of_named_command() {
    let err = load_err(
        r"
name: root
commands:
  - name: fmt
    id: rust-fmt
    cmd: 'true'
  - name: clippy
    cmd: 'true'
    depends_on: [fmt]
",
    );
    assert!(err.contains("root > clippy"), "{err}");
    assert!(err.contains("'fmt'"), "{err}");
    assert!(err.contains("has id 'rust-fmt'"), "{err}");
}

#[test]
fn depends_on_typo_suggests_close_id() {
    let err = load_err(
        r"
name: root
commands:
  - name: build
    cmd: 'true'
  - name: test
    cmd: 'true'
    depends_on: [biuld]
",
    );
    assert!(err.contains("did you mean 'build'?"), "{err}");
}

#[test]
fn explicit_duplicate_id_reports_both_locations() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_config(
        dir.path(),
        r"
name: root
children:
  - name: backend
    commands:
      - name: lint
        id: check
        cmd: 'true'
  - name: frontend
    commands:
      - name: eslint
        id: check
        cmd: 'true'
",
    );
    let err = load_config(Some(&path), true).unwrap_err();
    assert!(
        matches!(&err, fnug::config_file::ConfigError::DuplicateId { id, .. } if id == "check"),
        "{err:?}"
    );
    let msg = err.to_string();
    assert!(msg.contains("root > backend > lint"), "{msg}");
    assert!(msg.contains("root > frontend > eslint"), "{msg}");
    assert!(msg.contains(".fnug.yaml"), "{msg}");
}

#[test]
fn explicit_id_with_slash_rejected() {
    let err = load_err("name: root\ncommands:\n  - name: a\n    id: x/y\n    cmd: 'true'\n");
    assert!(err.contains("'x/y'"), "{err}");
    assert!(err.contains("must not contain '/'"), "{err}");
}

#[test]
fn explicit_empty_id_rejected() {
    let err = load_err("name: root\ncommands:\n  - name: a\n    id: ''\n    cmd: 'true'\n");
    assert!(err.contains("empty id"), "{err}");
}

#[test]
fn sibling_duplicate_names_error() {
    let err = load_err(
        r"
name: root
children:
  - name: backend
    commands:
      - name: test
        cmd: 'true'
      - name: test
        cmd: 'false'
",
    );
    assert!(err.contains("backend/test"), "{err}");
    assert!(err.contains("root > backend > test"), "{err}");
}

#[test]
fn group_and_command_share_namespace() {
    let (_dir, config) = load(
        r"
name: root
children:
  - name: lint
    commands:
      - name: lint
        cmd: 'true'
",
    );
    assert_eq!(config.children[0].id, "lint");
    assert_eq!(config.children[0].commands[0].id, "lint/lint");
}

#[test]
fn command_may_share_the_root_name() {
    let (_dir, config) = load(
        r"
name: lint
commands:
  - name: lint
    cmd: 'true'
  - name: after
    cmd: 'true'
    depends_on: [lint]
",
    );
    assert_eq!(config.commands[0].id, "lint");
    assert_eq!(config.commands[1].depends_on, ["lint"]);
    assert_ne!(config.id, "lint");
}

#[test]
fn explicit_root_id_still_unique() {
    let err = load_err("name: x\nid: lint\ncommands:\n  - name: lint\n    cmd: 'true'\n");
    assert!(err.contains("Duplicate id 'lint'"), "{err}");
}

#[test]
fn dependency_cycle_lists_whole_cycle() {
    let err = load_err(
        r"
name: root
commands:
  - name: a
    cmd: 'true'
    depends_on: [b]
  - name: b
    cmd: 'true'
    depends_on: [c]
  - name: c
    cmd: 'true'
    depends_on: [a]
",
    );
    assert!(err.contains("a -> b -> c -> a"), "{err}");
}

#[test]
fn dogfood_config_ids() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/.fnug.yaml");
    let (config, _) = load_config(Some(path), true).unwrap();
    let ids: Vec<&str> = config
        .all_commands()
        .iter()
        .map(|c| c.id.as_str())
        .collect();
    for id in ["rust-fmt", "fmt", "test", "clippy check"] {
        assert!(ids.contains(&id), "{id} not in {ids:?}");
    }
}

// ─── resolve_command ───

fn resolve_fixture() -> (tempfile::TempDir, CommandGroup) {
    load(
        r"
name: root
children:
  - name: backend
    commands:
      - name: test
        cmd: 'true'
      - name: fmt
        id: rust-fmt
        cmd: 'true'
  - name: frontend
    commands:
      - name: test
        cmd: 'true'
      - name: Build
        cmd: 'true'
",
    )
}

#[test]
fn resolve_command_exact_id_then_unique_name() {
    let (_dir, config) = resolve_fixture();
    assert_eq!(resolve_command(&config, "rust-fmt").unwrap().name, "fmt");
    assert_eq!(resolve_command(&config, "fmt").unwrap().id, "rust-fmt");
    assert_eq!(resolve_command(&config, "build").unwrap().id, "Build");
    assert_eq!(
        resolve_command(&config, "backend/test").unwrap().id,
        "backend/test"
    );
}

#[test]
fn resolve_command_ambiguous_name_lists_candidates() {
    let (_dir, config) = resolve_fixture();
    match resolve_command(&config, "TEST") {
        Err(ResolveError::Ambiguous { candidates, .. }) => assert_eq!(
            candidates,
            [
                ("backend/test".to_string(), "root > backend".to_string()),
                ("frontend/test".to_string(), "root > frontend".to_string()),
            ]
        ),
        other => panic!("{other:?}"),
    }
}

#[test]
fn resolve_command_not_found_suggests() {
    let (_dir, config) = resolve_fixture();
    match resolve_command(&config, "rust-fnt") {
        Err(err @ ResolveError::NotFound { .. }) => {
            let ResolveError::NotFound { suggestions, .. } = &err else {
                unreachable!()
            };
            assert_eq!(suggestions, &["rust-fmt"]);
            assert!(err.to_string().contains("rust-fmt"), "{err}");
        }
        other => panic!("{other:?}"),
    }
}

// ─── workspace packages ───

/// A tempdir with `.fnug.yaml` = `root` and each `(dir, config)` package written below it.
fn workspace(root: &str, packages: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    write_config(dir.path(), root);
    for (pkg, content) in packages {
        let pkg_dir = dir.path().join(pkg);
        std::fs::create_dir_all(&pkg_dir).unwrap();
        write_config(&pkg_dir, content);
    }
    dir
}

fn load_workspace(dir: &Path) -> CommandGroup {
    let path = dir.join(".fnug.yaml");
    load_config(path.to_str(), false).unwrap().0
}

const GLOB_ROOT: &str = "name: root\nworkspace:\n  paths: [packages/*]\n";

#[test]
fn workspace_package_ids_namespaced() {
    let pkg = |name: &str| {
        format!(
            r"
name: {name}
commands:
  - name: build
    id: build
    cmd: 'true'
  - name: test
    id: test
    cmd: 'true'
    depends_on: [build]
"
        )
    };
    let dir = workspace(
        &format!(
            "{GLOB_ROOT}commands:\n  - name: all\n    cmd: 'true'\n    depends_on: [api/test, web/test]\n"
        ),
        &[("packages/api", &pkg("api")), ("packages/web", &pkg("web"))],
    );
    let config = load_workspace(dir.path());
    assert_eq!(
        ids(&config),
        [
            ("all".into(), "all".into()),
            ("build".into(), "api/build".into()),
            ("test".into(), "api/test".into()),
            ("build".into(), "web/build".into()),
            ("test".into(), "web/test".into()),
        ]
    );
    assert_eq!(config.children[0].id, "api");
    assert_eq!(config.children[0].commands[1].depends_on, ["api/build"]);
    assert_eq!(config.children[1].commands[1].depends_on, ["web/build"]);
    assert_eq!(config.commands[0].depends_on, ["api/test", "web/test"]);
}

#[test]
fn workspace_package_depends_on_root_command() {
    let dir = workspace(
        &format!("{GLOB_ROOT}commands:\n  - name: codegen\n    cmd: 'true'\n"),
        &[(
            "packages/a",
            "name: a\ncommands:\n  - name: build\n    cmd: 'true'\n    depends_on: [codegen]\n",
        )],
    );
    let config = load_workspace(dir.path());
    assert_eq!(command(&config, "build").id, "a/build");
    assert_eq!(command(&config, "build").depends_on, ["codegen"]);
}

#[test]
fn workspace_package_name_collision_names_both_files() {
    let pkg = "name: app\ncommands:\n  - name: build\n    cmd: 'true'\n";
    let dir = workspace(GLOB_ROOT, &[("packages/a", pkg), ("packages/b", pkg)]);
    let path = dir.path().join(".fnug.yaml");
    let err = load_config(path.to_str(), false).unwrap_err().to_string();
    assert!(err.contains("'app'"), "{err}");
    assert!(err.contains("packages/a/.fnug.yaml"), "{err}");
    assert!(err.contains("packages/b/.fnug.yaml"), "{err}");
}

#[test]
fn workspace_package_cwd_anchored() {
    let dir = workspace(
        GLOB_ROOT,
        &[(
            "packages/a",
            "name: a\ncwd: src\ncommands:\n  - name: pwd\n    cmd: pwd\n",
        )],
    );
    std::fs::create_dir(dir.path().join("packages/a/src")).unwrap();
    let config = load_workspace(dir.path());
    let root = dir.path().canonicalize().unwrap();
    assert_eq!(command(&config, "pwd").cwd, root.join("packages/a/src"));
}

#[test]
fn workspace_root_cwd_does_not_leak() {
    let dir = workspace(
        &format!("{GLOB_ROOT}cwd: app\n"),
        &[(
            "packages/a",
            "name: a\ncommands:\n  - name: pwd\n    cmd: pwd\n",
        )],
    );
    std::fs::create_dir(dir.path().join("app")).unwrap();
    let config = load_workspace(dir.path());
    let root = dir.path().canonicalize().unwrap();
    assert_eq!(config.cwd, root.join("app"));
    assert_eq!(command(&config, "pwd").cwd, root.join("packages/a"));
}

#[test]
fn workspace_root_auto_does_not_leak() {
    let dir = workspace(
        &format!("{GLOB_ROOT}auto:\n  git: true\n  path: [src]\n  regex: ['\\.rs$']\n"),
        &[(
            "packages/a",
            "name: a\ncommands:\n  - name: a-lint\n    cmd: 'true'\n    auto:\n      git: true\n",
        )],
    );
    let repo = git2::Repository::init(dir.path()).unwrap();
    std::fs::create_dir_all(dir.path().join("packages/a/lib")).unwrap();
    std::fs::write(dir.path().join("packages/a/lib/mod.py"), "").unwrap();
    commit_all(&repo);

    let config = load_workspace(dir.path());
    let lint = command(&config, "a-lint");
    let pkg = dir.path().canonicalize().unwrap().join("packages/a");
    assert_eq!(lint.auto.paths(), [pkg]);
    assert!(lint.auto.regexes().is_empty());
    assert_eq!(config.children[0].auto.git, None);

    std::fs::write(dir.path().join("packages/a/lib/mod.py"), "x = 1\n").unwrap();
    let commands = config.all_commands().into_iter().cloned().collect();
    let selected: Vec<String> = fnug::selectors::get_selected_commands(commands)
        .unwrap()
        .into_iter()
        .map(|c| c.name)
        .collect();
    assert_eq!(selected, ["a-lint"]);
}

#[test]
fn workspace_root_env_does_not_leak() {
    let dir = workspace(
        &format!(
            "{GLOB_ROOT}env:\n  ROOT_ONLY: '1'\ncommands:\n  - name: root-cmd\n    cmd: 'true'\n"
        ),
        &[(
            "packages/a",
            "name: a\nenv:\n  PKG: '2'\ncommands:\n  - name: a-cmd\n    cmd: 'true'\n",
        )],
    );
    let config = load_workspace(dir.path());
    assert_eq!(command(&config, "root-cmd").env["ROOT_ONLY"], "1");
    let pkg_env = &command(&config, "a-cmd").env;
    assert!(!pkg_env.contains_key("ROOT_ONLY"), "{pkg_env:?}");
    assert_eq!(pkg_env["PKG"], "2");
}

#[test]
fn workspace_package_matches_standalone() {
    let pkg = r"
name: a
cwd: src
auto:
  git: true
env:
  PKG: '1'
commands:
  - name: lint
    cmd: 'true'
    auto:
      regex: ['\.py$']
";
    let dir = workspace(
        &format!("{GLOB_ROOT}cwd: app\nauto:\n  watch: true\n  path: [x]\nenv:\n  ROOT: '1'\n"),
        &[("packages/a", pkg)],
    );
    std::fs::create_dir(dir.path().join("app")).unwrap();
    std::fs::create_dir(dir.path().join("packages/a/src")).unwrap();
    let merged = load_workspace(dir.path());
    let pkg_path = dir.path().join("packages/a/.fnug.yaml");
    let (standalone, _) = load_config(pkg_path.to_str(), true).unwrap();

    let (a, b) = (command(&merged, "lint"), command(&standalone, "lint"));
    assert_eq!(a.cwd, b.cwd);
    assert_eq!(a.env, b.env);
    assert_eq!(a.auto.paths(), b.auto.paths());
    assert_eq!(
        (a.auto.git, a.auto.watch, a.auto.always, a.auto.check),
        (b.auto.git, b.auto.watch, b.auto.always, b.auto.check)
    );
    assert_eq!(a.auto.regexes().len(), b.auto.regexes().len());
}

/// Every warning logged by this test binary so far.
fn logged_warnings() -> &'static Mutex<Vec<String>> {
    struct Capture;
    impl log::Log for Capture {
        fn enabled(&self, metadata: &log::Metadata) -> bool {
            metadata.level() <= log::Level::Warn
        }
        fn log(&self, record: &log::Record) {
            if self.enabled(record.metadata()) {
                WARNINGS.lock().unwrap().push(record.args().to_string());
            }
        }
        fn flush(&self) {}
    }
    static WARNINGS: Mutex<Vec<String>> = Mutex::new(Vec::new());
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        log::set_logger(&Capture).unwrap();
        log::set_max_level(log::LevelFilter::Warn);
    });
    &WARNINGS
}

#[test]
fn workspace_package_fnug_version_checked() {
    let warnings = logged_warnings();
    let dir = workspace(
        GLOB_ROOT,
        &[(
            "packages/a",
            "fnug_version: 99.0.0\nname: a\ncommands:\n  - name: x\n    cmd: 'true'\n",
        )],
    );
    load_workspace(dir.path());
    let pkg_path = dir
        .path()
        .canonicalize()
        .unwrap()
        .join("packages/a/.fnug.yaml");
    let warnings = warnings.lock().unwrap();
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("requires fnug >= 99.0.0")
                && w.contains(&pkg_path.display().to_string())),
        "{warnings:?}"
    );
}

// ─── workspace root promotion ───

fn load_from(start: &Path) -> fnug::LoadedConfig {
    fnug::load(&LoadOptions {
        start_dir: Some(start.to_path_buf()),
        ..LoadOptions::default()
    })
    .unwrap()
}

fn names(config: &CommandGroup) -> Vec<String> {
    config
        .all_commands()
        .into_iter()
        .map(|c| c.name.clone())
        .collect()
}

fn one_command(name: &str) -> String {
    format!("name: {name}\ncommands:\n  - name: {name}-cmd\n    cmd: 'true'\n")
}

#[test]
fn explicit_config_skips_promotion() {
    let dir = workspace(
        &format!("{GLOB_ROOT}commands:\n  - name: root-cmd\n    cmd: 'true'\n"),
        &[("packages/a", &one_command("a"))],
    );
    let loaded = fnug::load(&LoadOptions {
        config: Some(dir.path().join("packages/a/.fnug.yaml")),
        ..LoadOptions::default()
    })
    .unwrap();
    assert_eq!(names(&loaded.root), ["a-cmd"]);
}

#[test]
fn start_dir_promotes_only_when_covered() {
    let dir = workspace(
        &format!("{GLOB_ROOT}commands:\n  - name: root-cmd\n    cmd: 'true'\n"),
        &[
            ("packages/a", &one_command("a")),
            ("tools/x", &one_command("x")),
        ],
    );
    let root = dir.path().canonicalize().unwrap();

    let local = load_from(&dir.path().join("tools/x"));
    assert_eq!(names(&local.root), ["x-cmd"]);
    assert_eq!(local.config_path, root.join("tools/x/.fnug.yaml"));

    let promoted = load_from(&dir.path().join("packages/a"));
    assert_eq!(names(&promoted.root), ["root-cmd", "a-cmd"]);
    assert_eq!(promoted.config_path, root.join(".fnug.yaml"));
}

#[test]
fn empty_paths_workspace_does_not_hijack() {
    let dir = workspace(
        "name: root\nworkspace:\n  paths: []\ncommands:\n  - name: planted\n    cmd: 'true'\n",
        &[("victim", &one_command("victim"))],
    );
    let loaded = load_from(&dir.path().join("victim"));
    assert_eq!(names(&loaded.root), ["victim-cmd"]);
}

#[test]
fn nested_config_not_hijacked() {
    // The root's walk stops at services/, which has a config, so it never reaches services/api.
    let dir = workspace(
        "name: root\nworkspace: true\n",
        &[
            ("services", &one_command("svc")),
            ("services/api", &one_command("api")),
        ],
    );
    git2::Repository::init(dir.path()).unwrap();
    let loaded = load_from(&dir.path().join("services/api"));
    assert_eq!(names(&loaded.root), ["api-cmd"]);

    let from_services = load_from(&dir.path().join("services"));
    assert_eq!(names(&from_services.root), ["svc-cmd"]);
    assert_eq!(from_services.root.name, "root");
}

#[test]
fn gitignored_nested_worktree_not_hijacked() {
    // Like a linked worktree under .claude/worktrees/, or any gitignored checkout copy.
    let dir = workspace(
        "name: root\nworkspace: true\ncommands:\n  - name: main-cmd\n    cmd: 'true'\n",
        &[
            (
                "worktrees/wt",
                &format!("{}workspace: true\n", one_command("wt")),
            ),
            (".claude/worktrees/wt2", &one_command("wt2")),
            ("pkg", &one_command("pkg")),
        ],
    );
    git2::Repository::init(dir.path()).unwrap();
    std::fs::write(dir.path().join(".gitignore"), "worktrees/\n").unwrap();

    assert_eq!(
        names(&load_from(&dir.path().join("worktrees/wt")).root),
        ["wt-cmd"]
    );
    assert_eq!(
        names(&load_from(&dir.path().join(".claude/worktrees/wt2")).root),
        ["wt2-cmd"]
    );
    assert_eq!(
        names(&load_from(&dir.path().join("pkg")).root),
        ["main-cmd", "pkg-cmd"]
    );
}

#[test]
fn max_depth_limits_promotion() {
    let dir = workspace(
        "name: root\nworkspace:\n  max_depth: 1\n",
        &[("a/b", &one_command("deep"))],
    );
    git2::Repository::init(dir.path()).unwrap();
    assert_eq!(
        names(&load_from(&dir.path().join("a/b")).root),
        ["deep-cmd"]
    );
}

#[test]
fn unparseable_ancestor_skipped() {
    let dir = workspace("name: [unterminated\n", &[("repo", &one_command("repo"))]);
    let loaded = load_from(&dir.path().join("repo"));
    assert_eq!(names(&loaded.root), ["repo-cmd"]);
}

#[test]
fn workspace_root_outside_git_walks_filesystem() {
    let dir = workspace(
        "name: root\nworkspace: true\ncommands: []\n",
        &[("pkg", &one_command("pkg"))],
    );
    if git2::Repository::discover(dir.path()).is_ok() {
        return; // the tempdir is inside a git repo, so this is the git walk
    }
    assert_eq!(names(&load_workspace(dir.path())), ["pkg-cmd"]);
}

#[test]
fn non_git_ancestor_workspace_skipped() {
    let dir = workspace(
        "name: root\nworkspace: true\ncommands:\n  - name: root-cmd\n    cmd: 'true'\n",
        &[("repo", &one_command("repo"))],
    );
    if git2::Repository::discover(dir.path()).is_ok() {
        return; // the tempdir is inside a git repo, so the ancestor's discovery works
    }
    git2::Repository::init(dir.path().join("repo")).unwrap();
    let loaded = load_from(&dir.path().join("repo"));
    assert_eq!(names(&loaded.root), ["repo-cmd"]);
}

// ─── trust ───

/// A parent config that would take over `victim` (its workspace includes every subdirectory),
/// and a policy under which only `victim` is trusted, as if another user owned the rest.
/// `None` when running as root, whose files are always trusted.
fn planted_parent() -> Option<(tempfile::TempDir, fnug::trust::TrustPolicy)> {
    use std::os::unix::fs::MetadataExt;

    let dir = workspace(
        "name: parent\nworkspace:\n  paths: ['*']\ncommands:\n  - name: planted\n    cmd: 'true'\n",
        &[("victim", &one_command("victim"))],
    );
    if std::fs::metadata(dir.path()).unwrap().uid() == 0 {
        return None;
    }
    std::fs::create_dir(dir.path().join("victim-no-config")).unwrap();
    let trust = fnug::trust::TrustPolicy {
        uid: Some(u32::MAX - 1),
        safe_dirs: vec![dir.path().join("victim")],
        trust_all: false,
    };
    Some((dir, trust))
}

fn load_trusting(
    start: &Path,
    trust: &fnug::trust::TrustPolicy,
) -> Result<fnug::LoadedConfig, fnug::config_file::ConfigError> {
    fnug::load(&LoadOptions {
        start_dir: Some(start.to_path_buf()),
        trust: trust.clone(),
        ..LoadOptions::default()
    })
}

#[test]
fn untrusted_ancestor_skipped() {
    let Some((dir, trust)) = planted_parent() else {
        return;
    };
    let loaded = load_trusting(&dir.path().join("victim"), &trust).unwrap();
    assert_eq!(names(&loaded.root), ["victim-cmd"]);
}

#[test]
fn untrusted_nearest_config_errors() {
    let Some((dir, trust)) = planted_parent() else {
        return;
    };
    let err = load_trusting(&dir.path().join("victim-no-config"), &trust).unwrap_err();
    assert!(
        matches!(err, fnug::config_file::ConfigError::UntrustedConfig { .. }),
        "{err:?}"
    );
    let msg = err.to_string();
    assert!(msg.contains("FNUG_SAFE_DIRECTORIES"), "{msg}");
    assert!(msg.contains("-c"), "{msg}");
}

#[test]
fn explicit_config_bypasses_trust() {
    let Some((dir, trust)) = planted_parent() else {
        return;
    };
    let loaded = fnug::load(&LoadOptions {
        config: Some(dir.path().join(".fnug.yaml")),
        trust: fnug::trust::TrustPolicy {
            safe_dirs: vec![],
            ..trust
        },
        ..LoadOptions::default()
    })
    .unwrap();
    // The explicit root loads; its untrusted package is skipped.
    assert_eq!(names(&loaded.root), ["planted"]);
    assert_eq!(loaded.sources.len(), 1);
}

#[test]
fn safe_directories_trust_everything_below() {
    let Some((dir, trust)) = planted_parent() else {
        return;
    };
    let trust = fnug::trust::TrustPolicy {
        safe_dirs: vec![dir.path().to_path_buf()],
        ..trust
    };
    let loaded = load_trusting(&dir.path().join("victim"), &trust).unwrap();
    assert_eq!(names(&loaded.root), ["planted", "victim-cmd"]);
}

// ─── env ───

#[test]
fn env_expands_parent_then_process() {
    let (_dir, config) = load(
        r"
name: root
env:
  FOO: foo
  HOME: /custom-home
  BIN: './bin:$PATH'
children:
  - name: child
    env:
      FNUG_TEST_SIBLING_A: a
      FNUG_TEST_SIBLING_B: '$FNUG_TEST_SIBLING_A'
      BRACED: '${FOO}-x'
      PARENT_WINS: '$HOME/x'
      LITERAL: '$$HOME and $1 and ${ and $'
      UNDEFINED: '[$FNUG_TEST_UNDEFINED_VAR]'
      PATH: '/extra:$PATH'
    commands:
      - name: cmd
        cmd: 'true'
        env:
          FOO: '$FOO$FOO'
",
    );
    let env = &command(&config, "cmd").env;
    let path = std::env::var("PATH").unwrap();
    assert_eq!(env["BIN"], format!("./bin:{path}"));
    assert_eq!(env["PATH"], format!("/extra:{path}"));
    assert_eq!(env["BRACED"], "foo-x");
    assert_eq!(env["PARENT_WINS"], "/custom-home/x");
    assert_eq!(env["LITERAL"], "$HOME and $1 and ${ and $");
    assert_eq!(env["UNDEFINED"], "[]");
    assert_eq!(env["FNUG_TEST_SIBLING_B"], "");
    assert_eq!(env["FOO"], "foofoo");
}

#[test]
fn env_path_prepend_check_passes() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_config(
        dir.path(),
        r"
name: root
env:
  PATH: './node_modules/.bin:$PATH'
commands:
  - name: show
    cmd: 'true'
    auto:
      always: true
",
    );
    let (config, cwd) = load_config(Some(&path), true).unwrap();
    let result = fnug::check::run(&config, &cwd, false, true, false).unwrap();
    assert_eq!(result.exit_code, 0);
}

#[test]
fn workspace_package_env_expands_against_process_only() {
    let dir = workspace(
        &format!("{GLOB_ROOT}env:\n  FNUG_TEST_ROOT_VAR: root\n"),
        &[(
            "packages/a",
            "name: a\nenv:\n  SEEN: '[$FNUG_TEST_ROOT_VAR]'\n  P: '$PATH'\ncommands:\n  - name: a-cmd\n    cmd: 'true'\n",
        )],
    );
    let config = load_workspace(dir.path());
    let env = &command(&config, "a-cmd").env;
    assert_eq!(env["SEEN"], "[]");
    assert_eq!(env["P"], std::env::var("PATH").unwrap());
}

// ─── --root ───

const ROOTED_CONFIG: &str = r"
name: root
workspace:
  paths: [packages/*]
children:
  - name: sub
    cwd: sub
    auto:
      path: [src]
    commands:
      - name: pwd
        cmd: pwd
        auto:
          always: true
";

/// A config in its own tempdir, and a separate root dir with `sub/` and a package.
fn rooted() -> (tempfile::TempDir, tempfile::TempDir) {
    let config_dir = tempfile::tempdir().unwrap();
    write_config(config_dir.path(), ROOTED_CONFIG);
    let root = workspace("name: unused\n", &[("packages/p", &one_command("p"))]);
    std::fs::create_dir(root.path().join("sub")).unwrap();
    (config_dir, root)
}

#[test]
fn root_dir_override_anchors_cwd() {
    let (config_dir, root) = rooted();
    let loaded = fnug::load(&LoadOptions {
        config: Some(config_dir.path().join(".fnug.yaml")),
        root_dir: Some(root.path().to_path_buf()),
        ..LoadOptions::default()
    })
    .unwrap();
    let base = root.path().canonicalize().unwrap();
    assert_eq!(loaded.cwd, base);
    assert_eq!(
        loaded.config_path,
        config_dir.path().canonicalize().unwrap().join(".fnug.yaml")
    );
    let pwd = command(&loaded.root, "pwd");
    assert_eq!(pwd.cwd, base.join("sub"));
    assert_eq!(pwd.auto.paths(), [base.join("sub/src")]);
    assert_eq!(command(&loaded.root, "p-cmd").cwd, base.join("packages/p"));
}

#[test]
fn root_dir_is_where_the_search_starts() {
    let (_config_dir, root) = rooted();
    let loaded = fnug::load(&LoadOptions {
        root_dir: Some(root.path().to_path_buf()),
        start_dir: Some(std::env::temp_dir()),
        ..LoadOptions::default()
    })
    .unwrap();
    assert_eq!(loaded.root.name, "unused");
    assert_eq!(loaded.cwd, root.path().canonicalize().unwrap());
}

#[test]
fn root_dir_missing_is_an_error() {
    let (config_dir, root) = rooted();
    let err = fnug::load(&LoadOptions {
        config: Some(config_dir.path().join(".fnug.yaml")),
        root_dir: Some(root.path().join("nope")),
        ..LoadOptions::default()
    })
    .unwrap_err()
    .to_string();
    assert!(err.contains("nope"), "{err}");
}

#[test]
fn root_flag_runs_commands_in_root_dir() {
    let (config_dir, root) = rooted();
    let config = config_dir.path().join(".fnug.yaml");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_fnug"))
        .current_dir(config_dir.path())
        .arg("-c")
        .arg(&config)
        .arg("--root")
        .arg(root.path())
        .args(["check", "--no-tui"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let sub = root.path().canonicalize().unwrap().join("sub");
    assert!(stdout.contains(&*sub.to_string_lossy()), "{stdout}");
}

#[test]
fn empty_config_path_is_a_clear_error() {
    let dir = tempfile::tempdir().unwrap();
    write_config(dir.path(), &one_command("here"));
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_fnug"))
        .current_dir(dir.path())
        .args(["-c", "", "check", "--no-tui"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("config path is empty"), "{stderr}");

    let err = fnug::load(&LoadOptions {
        config: Some("".into()),
        start_dir: Some(dir.path().to_path_buf()),
        ..LoadOptions::default()
    })
    .unwrap_err();
    assert!(
        matches!(err, fnug::config_file::ConfigError::EmptyConfigPath),
        "{err:?}"
    );
}
