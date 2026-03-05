use fnug::config_file::ConfigError;
use fnug::load_config;
use fnug::tui::app::App;
use fnug::tui::log_state::LogBuffer;

fn write_config(dir: &std::path::Path, content: &str) {
    std::fs::write(dir.join(".fnug.yaml"), content).unwrap();
}

fn load_and_check(dir: &std::path::Path, fail_fast: bool) -> i32 {
    let path = dir.join(".fnug.yaml").to_string_lossy().to_string();
    let (config, cwd) = load_config(Some(&path), false).unwrap();
    fnug::check::run(&config, &cwd, fail_fast, false, false)
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
    let (config, cwd) = load_config(Some(&path), false).unwrap();
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
    let (config, _) = load_config(Some(&path), false).unwrap();
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
    let result = load_config(Some(&path), false);
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
    let result = load_config(Some(&path), false);
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
    let (config, _) = load_config(Some(&path), false).unwrap();
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
    let result = load_config(Some(&path), false);
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
    let result = load_config(Some(&path), false);
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
    let (config, cwd) = load_config(Some(&path), false).unwrap();
    let app = App::new(config, cwd, LogBuffer::new());

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

// ─── setup hooks tests ───

fn init_git_repo(dir: &std::path::Path) -> git2::Repository {
    git2::Repository::init(dir).unwrap()
}

#[test]
fn test_setup_hooks_install() {
    let dir = tempfile::tempdir().unwrap();
    init_git_repo(dir.path());

    assert!(!fnug::setup::hooks::is_installed(dir.path()));
    fnug::setup::hooks::install(dir.path(), false).unwrap();

    let hook_path = dir.path().join(".git/hooks/pre-commit");
    assert!(hook_path.exists());
    assert!(fnug::setup::hooks::is_installed(dir.path()));

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&hook_path).unwrap().permissions().mode();
        assert!(mode & 0o111 != 0, "hook should be executable");
    }

    let contents = std::fs::read_to_string(&hook_path).unwrap();
    assert!(contents.contains("fnug check --fail-fast --mute-success"));
    assert!(!contents.contains("--no-workspace"));
}

#[test]
fn test_setup_hooks_install_no_workspace() {
    let dir = tempfile::tempdir().unwrap();
    init_git_repo(dir.path());

    fnug::setup::hooks::install(dir.path(), true).unwrap();

    let contents = std::fs::read_to_string(dir.path().join(".git/hooks/pre-commit")).unwrap();
    assert!(contents.contains("--no-workspace"));
}

#[test]
fn test_setup_hooks_remove() {
    let dir = tempfile::tempdir().unwrap();
    init_git_repo(dir.path());

    fnug::setup::hooks::install(dir.path(), false).unwrap();
    assert!(fnug::setup::hooks::is_installed(dir.path()));

    fnug::setup::hooks::remove(dir.path()).unwrap();
    assert!(!fnug::setup::hooks::is_installed(dir.path()));
    assert!(!dir.path().join(".git/hooks/pre-commit").exists());
}

#[test]
fn test_setup_hooks_reinstall() {
    let dir = tempfile::tempdir().unwrap();
    init_git_repo(dir.path());

    fnug::setup::hooks::install(dir.path(), false).unwrap();
    fnug::setup::hooks::install(dir.path(), false).unwrap();

    let hook_path = dir.path().join(".git/hooks/pre-commit");
    assert!(hook_path.exists());
    // Should not duplicate fnug lines
    let contents = std::fs::read_to_string(&hook_path).unwrap();
    assert_eq!(contents.matches("# fnug").count(), 1);
}

#[test]
fn test_setup_hooks_preserves_existing_hook() {
    let dir = tempfile::tempdir().unwrap();
    init_git_repo(dir.path());

    // Simulate an existing hook (e.g. husky)
    let hook_path = dir.path().join(".git/hooks/pre-commit");
    std::fs::create_dir_all(hook_path.parent().unwrap()).unwrap();
    std::fs::write(&hook_path, "#!/bin/sh\nnpx lint-staged\n").unwrap();

    fnug::setup::hooks::install(dir.path(), false).unwrap();

    let contents = std::fs::read_to_string(&hook_path).unwrap();
    assert!(
        contents.contains("npx lint-staged"),
        "existing hook should be preserved"
    );
    assert!(contents.contains("fnug check"), "fnug should be appended");
}

#[test]
fn test_setup_hooks_remove_preserves_other_hooks() {
    let dir = tempfile::tempdir().unwrap();
    init_git_repo(dir.path());

    // Create a hook with both fnug and other content
    let hook_path = dir.path().join(".git/hooks/pre-commit");
    std::fs::create_dir_all(hook_path.parent().unwrap()).unwrap();
    std::fs::write(&hook_path, "#!/bin/sh\nnpx lint-staged\n").unwrap();

    fnug::setup::hooks::install(dir.path(), false).unwrap();
    fnug::setup::hooks::remove(dir.path()).unwrap();

    // File should still exist with the other hook content
    assert!(hook_path.exists());
    let contents = std::fs::read_to_string(&hook_path).unwrap();
    assert!(
        contents.contains("npx lint-staged"),
        "other hook should be preserved"
    );
    assert!(!contents.contains("fnug"), "fnug lines should be removed");
}

// ─── setup MCP tests ───

use fnug::setup::mcp::Editor;

#[test]
fn test_setup_mcp_install_and_detect() {
    let dir = tempfile::tempdir().unwrap();

    for editor in Editor::ALL {
        assert!(!editor.is_installed(dir.path()));
        editor.install(dir.path()).unwrap();
        assert!(editor.is_installed(dir.path()));
    }
}

#[test]
fn test_setup_mcp_remove_deletes_empty_file() {
    let dir = tempfile::tempdir().unwrap();

    Editor::ClaudeCode.install(dir.path()).unwrap();
    Editor::ClaudeCode.remove(dir.path()).unwrap();

    assert!(!Editor::ClaudeCode.is_installed(dir.path()));
    assert!(!dir.path().join(".mcp.json").exists());
}

#[test]
fn test_setup_mcp_remove_cleans_empty_dir() {
    let dir = tempfile::tempdir().unwrap();

    Editor::VsCode.install(dir.path()).unwrap();
    assert!(dir.path().join(".vscode/mcp.json").exists());

    Editor::VsCode.remove(dir.path()).unwrap();
    assert!(!dir.path().join(".vscode/mcp.json").exists());
    assert!(!dir.path().join(".vscode").exists());
}

#[test]
fn test_setup_mcp_preserves_other_entries() {
    let dir = tempfile::tempdir().unwrap();

    // Write a config with an existing server
    let path = dir.path().join(".mcp.json");
    std::fs::write(&path, r#"{"mcpServers": {"other": {"command": "other"}}}"#).unwrap();

    Editor::ClaudeCode.install(dir.path()).unwrap();
    assert!(Editor::ClaudeCode.is_installed(dir.path()));

    Editor::ClaudeCode.remove(dir.path()).unwrap();
    assert!(!Editor::ClaudeCode.is_installed(dir.path()));
    // File should still exist with the other server
    assert!(path.exists());
    let content: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert!(content.get("mcpServers").unwrap().get("other").is_some());
}

#[test]
fn test_setup_mcp_remove_with_empty_inputs_array() {
    let dir = tempfile::tempdir().unwrap();

    // Simulate VS Code adding an empty "inputs" array alongside servers
    let vscode_dir = dir.path().join(".vscode");
    std::fs::create_dir_all(&vscode_dir).unwrap();
    std::fs::write(
        vscode_dir.join("mcp.json"),
        r#"{"inputs": [], "servers": {"fnug": {"type": "stdio", "command": "fnug", "args": ["mcp"]}}}"#,
    )
    .unwrap();

    assert!(Editor::VsCode.is_installed(dir.path()));
    Editor::VsCode.remove(dir.path()).unwrap();

    // File and dir should be cleaned up since only empty scaffolding remains
    assert!(!vscode_dir.join("mcp.json").exists());
    assert!(!vscode_dir.exists());
}

// ─── workspace tests ───

/// Helper: create a git repo with files added to the index.
fn setup_git_workspace(dir: &std::path::Path, files: &[(&str, &str)]) {
    let repo = init_git_repo(dir);
    for (path, content) in files {
        let full = dir.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(&full, content).unwrap();
    }
    let mut index = repo.index().unwrap();
    index
        .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
        .unwrap();
    index.write().unwrap();
}

#[test]
fn test_workspace_git_discovery() {
    let dir = tempfile::tempdir().unwrap();
    let root_config = r#"
fnug_version: "0.0.27"
name: root
id: root
workspace: true
commands:
  - name: root-cmd
    id: root-cmd
    cmd: echo root
"#;
    let sub_config = r#"
fnug_version: "0.0.27"
name: sub-package
id: sub-pkg
commands:
  - name: sub-cmd
    id: sub-cmd
    cmd: echo sub
"#;
    setup_git_workspace(
        dir.path(),
        &[
            (".fnug.yaml", root_config),
            ("packages/foo/.fnug.yaml", sub_config),
        ],
    );

    let path = dir.path().join(".fnug.yaml").to_string_lossy().to_string();
    let (config, _) = load_config(Some(&path), false).unwrap();

    assert_eq!(config.name, "root");
    // Root should have the sub-package as a child group
    assert_eq!(config.children.len(), 1);
    assert_eq!(config.children[0].name, "sub-package");
    assert_eq!(config.children[0].commands.len(), 1);
    assert_eq!(config.children[0].commands[0].name, "sub-cmd");
}

#[test]
fn test_workspace_glob_discovery() {
    let dir = tempfile::tempdir().unwrap();
    let root_config = r#"
fnug_version: "0.0.27"
name: root
id: root
workspace:
  paths:
    - "./packages/*/"
commands:
  - name: root-cmd
    id: root-cmd
    cmd: echo root
"#;
    let sub_a = r#"
fnug_version: "0.0.27"
name: pkg-a
id: pkg-a
commands:
  - name: cmd-a
    id: cmd-a
    cmd: echo a
"#;
    let sub_b = r#"
fnug_version: "0.0.27"
name: pkg-b
id: pkg-b
commands:
  - name: cmd-b
    id: cmd-b
    cmd: echo b
"#;
    std::fs::create_dir_all(dir.path().join("packages/a")).unwrap();
    std::fs::create_dir_all(dir.path().join("packages/b")).unwrap();
    write_config(dir.path(), root_config);
    std::fs::write(dir.path().join("packages/a/.fnug.yaml"), sub_a).unwrap();
    std::fs::write(dir.path().join("packages/b/.fnug.yaml"), sub_b).unwrap();

    let path = dir.path().join(".fnug.yaml").to_string_lossy().to_string();
    let (config, _) = load_config(Some(&path), false).unwrap();

    assert_eq!(config.name, "root");
    assert_eq!(config.children.len(), 2);

    let mut child_names: Vec<&str> = config.children.iter().map(|c| c.name.as_str()).collect();
    child_names.sort();
    assert_eq!(child_names, vec!["pkg-a", "pkg-b"]);
}

#[test]
fn test_workspace_sub_config_inherits_cwd() {
    let dir = tempfile::tempdir().unwrap();
    let root_config = r#"
fnug_version: "0.0.27"
name: root
id: root
workspace:
  paths:
    - "./packages/*/"
commands:
  - name: root-cmd
    id: root-cmd
    cmd: echo root
"#;
    let sub_config = r#"
fnug_version: "0.0.27"
name: sub
id: sub
commands:
  - name: sub-cmd
    id: sub-cmd
    cmd: echo sub
"#;
    std::fs::create_dir_all(dir.path().join("packages/foo")).unwrap();
    write_config(dir.path(), root_config);
    std::fs::write(dir.path().join("packages/foo/.fnug.yaml"), sub_config).unwrap();

    let path = dir.path().join(".fnug.yaml").to_string_lossy().to_string();
    let (config, _) = load_config(Some(&path), false).unwrap();

    let sub_group = &config.children[0];
    assert_eq!(sub_group.cwd, dir.path().join("packages/foo"));
}

#[test]
fn test_workspace_duplicate_ids_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let root_config = r#"
fnug_version: "0.0.27"
name: root
id: root
workspace:
  paths:
    - "./packages/*/"
commands:
  - name: root-cmd
    id: dup-id
    cmd: echo root
"#;
    let sub_config = r#"
fnug_version: "0.0.27"
name: sub
id: sub
commands:
  - name: sub-cmd
    id: dup-id
    cmd: echo sub
"#;
    std::fs::create_dir_all(dir.path().join("packages/foo")).unwrap();
    write_config(dir.path(), root_config);
    std::fs::write(dir.path().join("packages/foo/.fnug.yaml"), sub_config).unwrap();

    let path = dir.path().join(".fnug.yaml").to_string_lossy().to_string();
    let result = load_config(Some(&path), false);
    assert!(result.is_err());
    match result.unwrap_err() {
        ConfigError::DuplicateId(id) => assert_eq!(id, "dup-id"),
        other => panic!("Expected DuplicateId, got: {other:?}"),
    }
}

#[test]
fn test_workspace_no_sub_configs() {
    let dir = tempfile::tempdir().unwrap();
    let root_config = r#"
fnug_version: "0.0.27"
name: root
id: root
workspace: true
commands:
  - name: root-cmd
    id: root-cmd
    cmd: echo root
"#;
    setup_git_workspace(dir.path(), &[(".fnug.yaml", root_config)]);

    let path = dir.path().join(".fnug.yaml").to_string_lossy().to_string();
    let (config, _) = load_config(Some(&path), false).unwrap();
    assert_eq!(config.name, "root");
    assert!(config.children.is_empty());
}

#[test]
fn test_workspace_false_disables() {
    let dir = tempfile::tempdir().unwrap();
    let root_config = r#"
fnug_version: "0.0.27"
name: root
id: root
workspace: false
commands:
  - name: root-cmd
    id: root-cmd
    cmd: echo root
"#;
    let sub_config = r#"
fnug_version: "0.0.27"
name: sub
id: sub
commands:
  - name: sub-cmd
    id: sub-cmd
    cmd: echo sub
"#;
    setup_git_workspace(
        dir.path(),
        &[
            (".fnug.yaml", root_config),
            ("packages/foo/.fnug.yaml", sub_config),
        ],
    );

    let path = dir.path().join(".fnug.yaml").to_string_lossy().to_string();
    let (config, _) = load_config(Some(&path), false).unwrap();
    assert!(config.children.is_empty());
}

#[test]
fn test_workspace_sub_config_workspace_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let root_config = r#"
fnug_version: "0.0.27"
name: root
id: root
workspace:
  paths:
    - "./packages/*/"
commands:
  - name: root-cmd
    id: root-cmd
    cmd: echo root
"#;
    // Sub-config also declares workspace — should be ignored
    let sub_config = r#"
fnug_version: "0.0.27"
name: sub
id: sub
workspace: true
commands:
  - name: sub-cmd
    id: sub-cmd
    cmd: echo sub
"#;
    std::fs::create_dir_all(dir.path().join("packages/foo")).unwrap();
    write_config(dir.path(), root_config);
    std::fs::write(dir.path().join("packages/foo/.fnug.yaml"), sub_config).unwrap();

    let path = dir.path().join(".fnug.yaml").to_string_lossy().to_string();
    let (config, _) = load_config(Some(&path), false).unwrap();

    // Should still load fine, just the sub's workspace field is ignored
    assert_eq!(config.children.len(), 1);
    assert_eq!(config.children[0].name, "sub");
}
