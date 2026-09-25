//! Tests for config loading: parsing, validation and inheritance.

use std::path::Path;

use fnug::commands::command::Command;
use fnug::commands::group::CommandGroup;
use fnug::load_config;

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
