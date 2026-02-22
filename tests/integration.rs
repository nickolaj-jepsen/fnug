use fnug::config_file::ConfigError;
use fnug::load_config;
use fnug::tui::app::App;
use fnug::tui::log_state::LogBuffer;

fn write_config(dir: &std::path::Path, content: &str) {
    std::fs::write(dir.join(".fnug.yaml"), content).unwrap();
}

fn load_and_check(dir: &std::path::Path, fail_fast: bool) -> i32 {
    let path = dir.join(".fnug.yaml").to_string_lossy().to_string();
    let (config, cwd, _) = load_config(Some(&path)).unwrap();
    fnug::check::run(&config, &cwd, fail_fast, false)
        .unwrap()
        .exit_code
}

#[test]
fn test_load_config_minimal() {
    let dir = tempfile::tempdir().unwrap();
    write_config(
        dir.path(),
        r#"
fnug_version: "0.0.27"
name: root
id: root
commands:
  - name: test
    id: test-cmd
    cmd: echo hello
"#,
    );
    let path = dir.path().join(".fnug.yaml").to_string_lossy().to_string();
    let (config, cwd, _) = load_config(Some(&path)).unwrap();
    assert_eq!(config.name, "root");
    assert_eq!(config.commands.len(), 1);
    assert_eq!(config.commands[0].name, "test");
    assert_eq!(cwd, dir.path());
}

#[test]
fn test_load_config_inheritance() {
    let dir = tempfile::tempdir().unwrap();
    write_config(
        dir.path(),
        r#"
fnug_version: "0.0.27"
name: root
id: root
auto:
  git: true
  path:
    - "./"
children:
  - name: child
    id: child-group
    commands:
      - name: test
        id: test-cmd
        cmd: echo hello
"#,
    );
    let path = dir.path().join(".fnug.yaml").to_string_lossy().to_string();
    let (config, _, _) = load_config(Some(&path)).unwrap();
    // Child group should inherit auto settings from parent
    let child = &config.children[0];
    assert_eq!(child.auto.git, Some(true));
}

#[test]
fn test_load_config_duplicate_ids() {
    let dir = tempfile::tempdir().unwrap();
    write_config(
        dir.path(),
        r#"
fnug_version: "0.0.27"
name: root
id: dup
children:
  - name: child
    id: dup
    commands:
      - name: test
        id: test-cmd
        cmd: echo hello
"#,
    );
    let path = dir.path().join(".fnug.yaml").to_string_lossy().to_string();
    let result = load_config(Some(&path));
    assert!(result.is_err());
    match result.unwrap_err() {
        ConfigError::DuplicateId(id) => assert_eq!(id, "dup"),
        other => panic!("Expected DuplicateId, got: {other:?}"),
    }
}

#[test]
fn test_load_config_invalid_regex() {
    let dir = tempfile::tempdir().unwrap();
    write_config(
        dir.path(),
        r#"
fnug_version: "0.0.27"
name: root
id: root
commands:
  - name: test
    id: test-cmd
    cmd: echo hello
    auto:
      git: true
      path:
        - "./"
      regex:
        - "[invalid"
"#,
    );
    let path = dir.path().join(".fnug.yaml").to_string_lossy().to_string();
    let result = load_config(Some(&path));
    assert!(result.is_err());
    match result.unwrap_err() {
        ConfigError::Regex { pattern, .. } => assert_eq!(pattern, "[invalid"),
        other => panic!("Expected Regex error, got: {other:?}"),
    }
}

#[test]
fn test_always_selector_integration() {
    let dir = tempfile::tempdir().unwrap();
    write_config(
        dir.path(),
        r#"
fnug_version: "0.0.27"
name: root
id: root
commands:
  - name: always-cmd
    id: always-cmd
    cmd: echo always
    auto:
      always: true
  - name: not-always
    id: not-always
    cmd: echo not
"#,
    );
    let path = dir.path().join(".fnug.yaml").to_string_lossy().to_string();
    let (config, _, _) = load_config(Some(&path)).unwrap();
    let all_commands: Vec<_> = config.all_commands().into_iter().cloned().collect();
    let selected = fnug::selectors::get_selected_commands(all_commands).unwrap();
    // Only the always command should be selected
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].id, "always-cmd");
}

#[test]
fn test_config_empty_name_rejected() {
    let dir = tempfile::tempdir().unwrap();
    write_config(
        dir.path(),
        r#"
fnug_version: "0.0.27"
name: root
id: root
commands:
  - name: "  "
    id: empty-name
    cmd: echo hello
"#,
    );
    let path = dir.path().join(".fnug.yaml").to_string_lossy().to_string();
    let result = load_config(Some(&path));
    assert!(result.is_err());
    match result.unwrap_err() {
        ConfigError::Validation(msg) => assert!(msg.contains("empty name"), "got: {msg}"),
        other => panic!("Expected Validation error, got: {other:?}"),
    }
}

#[test]
fn test_config_empty_cmd_rejected() {
    let dir = tempfile::tempdir().unwrap();
    write_config(
        dir.path(),
        r#"
fnug_version: "0.0.27"
name: root
id: root
commands:
  - name: test
    id: test-cmd
    cmd: ""
"#,
    );
    let path = dir.path().join(".fnug.yaml").to_string_lossy().to_string();
    let result = load_config(Some(&path));
    assert!(result.is_err());
    match result.unwrap_err() {
        ConfigError::Validation(msg) => assert!(msg.contains("empty cmd"), "got: {msg}"),
        other => panic!("Expected Validation error, got: {other:?}"),
    }
}

#[test]
fn test_app_creation() {
    let dir = tempfile::tempdir().unwrap();
    write_config(
        dir.path(),
        r#"
fnug_version: "0.0.27"
name: root
id: root
children:
  - name: group1
    id: g1
    commands:
      - name: cmd1
        id: cmd1
        cmd: echo one
      - name: cmd2
        id: cmd2
        cmd: echo two
commands:
  - name: cmd3
    id: cmd3
    cmd: echo three
"#,
    );
    let path = dir.path().join(".fnug.yaml").to_string_lossy().to_string();
    let (config, cwd, config_path) = load_config(Some(&path)).unwrap();
    let app = App::new(config, cwd, config_path, LogBuffer::new());

    // root + g1 + cmd1 + cmd2 + cmd3 = 5 visible nodes
    assert_eq!(app.visible_nodes.len(), 5);

    // Check the IDs are correct
    let ids: Vec<&str> = app.visible_nodes.iter().map(|n| n.id.as_str()).collect();
    assert_eq!(ids, vec!["root", "g1", "cmd1", "cmd2", "cmd3"]);
}

// ─── check mode tests ───

#[test]
fn test_check_passing_commands() {
    let dir = tempfile::tempdir().unwrap();
    write_config(
        dir.path(),
        r#"
fnug_version: "0.0.27"
name: root
id: root
commands:
  - name: pass1
    id: pass1
    cmd: "true"
    auto:
      always: true
  - name: pass2
    id: pass2
    cmd: "true"
    auto:
      always: true
"#,
    );
    assert_eq!(load_and_check(dir.path(), false), 0);
}

#[test]
fn test_check_failing_command() {
    let dir = tempfile::tempdir().unwrap();
    write_config(
        dir.path(),
        r#"
fnug_version: "0.0.27"
name: root
id: root
commands:
  - name: fail1
    id: fail1
    cmd: "false"
    auto:
      always: true
"#,
    );
    assert_eq!(load_and_check(dir.path(), false), 1);
}

#[test]
fn test_check_dependency_ordering() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("marker.txt");
    write_config(
        dir.path(),
        &format!(
            r#"
fnug_version: "0.0.27"
name: root
id: root
commands:
  - name: reader
    id: reader
    cmd: "cat {}"
    auto:
      always: true
    depends_on:
      - writer
  - name: writer
    id: writer
    cmd: "echo hello > {}"
    auto:
      always: true
"#,
            marker.display(),
            marker.display()
        ),
    );
    assert_eq!(load_and_check(dir.path(), false), 0);
}

#[test]
fn test_check_dependency_failure_skips() {
    let dir = tempfile::tempdir().unwrap();
    write_config(
        dir.path(),
        r#"
fnug_version: "0.0.27"
name: root
id: root
commands:
  - name: depender
    id: depender
    cmd: "true"
    auto:
      always: true
    depends_on:
      - failing-dep
  - name: failing-dep
    id: failing-dep
    cmd: "false"
    auto:
      always: true
"#,
    );
    assert_eq!(load_and_check(dir.path(), false), 1);
}

#[test]
fn test_check_no_commands_selected() {
    let dir = tempfile::tempdir().unwrap();
    write_config(
        dir.path(),
        r#"
fnug_version: "0.0.27"
name: root
id: root
commands:
  - name: no-auto
    id: no-auto
    cmd: "true"
"#,
    );
    assert_eq!(load_and_check(dir.path(), false), 0);
}

#[test]
fn test_check_fail_fast() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("should-not-exist.txt");
    write_config(
        dir.path(),
        &format!(
            r#"
fnug_version: "0.0.27"
name: root
id: root
commands:
  - name: fail-first
    id: aaa-fail
    cmd: "false"
    auto:
      always: true
  - name: would-run
    id: zzz-marker
    cmd: "touch {}"
    auto:
      always: true
"#,
            marker.display()
        ),
    );
    assert_eq!(load_and_check(dir.path(), true), 1);
    assert!(!marker.exists(), "second command should not have run");
}

// ─── init-hooks tests ───

fn init_git_repo(dir: &std::path::Path) {
    git2::Repository::init(dir).unwrap();
}

#[test]
fn test_init_hooks_creates_hook() {
    let dir = tempfile::tempdir().unwrap();
    init_git_repo(dir.path());

    fnug::init_hooks::run(&dir.path().to_path_buf(), false).unwrap();

    let hook_path = dir.path().join(".git/hooks/pre-commit");
    assert!(hook_path.exists());

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&hook_path).unwrap().permissions().mode();
        assert!(mode & 0o111 != 0, "hook should be executable");
    }

    let contents = std::fs::read_to_string(&hook_path).unwrap();
    assert!(contents.contains("fnug check --fail-fast"));
}

#[test]
fn test_init_hooks_refuses_overwrite() {
    let dir = tempfile::tempdir().unwrap();
    init_git_repo(dir.path());

    fnug::init_hooks::run(&dir.path().to_path_buf(), false).unwrap();
    let result = fnug::init_hooks::run(&dir.path().to_path_buf(), false);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("already exists"));
}

#[test]
fn test_init_hooks_force_overwrites() {
    let dir = tempfile::tempdir().unwrap();
    init_git_repo(dir.path());

    fnug::init_hooks::run(&dir.path().to_path_buf(), false).unwrap();
    fnug::init_hooks::run(&dir.path().to_path_buf(), true).unwrap();

    let hook_path = dir.path().join(".git/hooks/pre-commit");
    assert!(hook_path.exists());
}
