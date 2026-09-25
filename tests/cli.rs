//! Tests for the `fnug` CLI: argument parsing, config loading and setup output.

use std::path::Path;
use std::process::{Command, Output};

use fnug::setup::mcp::Editor;
use serde_json::{Value, json};

const PASSING_CONFIG: &str = r#"
fnug_version: "0.0.27"
name: root
id: root
commands:
  - name: pass
    id: pass
    cmd: "true"
    auto:
      always: true
"#;

fn fnug(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_fnug"))
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap()
}

fn git_repo_with_config(file: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    git2::Repository::init(dir.path()).unwrap();
    std::fs::write(dir.path().join(file), PASSING_CONFIG).unwrap();
    dir
}

fn read_json(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "exit {:?}, stderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn version_flag() {
    let dir = tempfile::tempdir().unwrap();
    for flag in ["--version", "-V"] {
        let output = fnug(dir.path(), &[flag]);
        assert_success(&output);
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            format!("fnug {}", env!("CARGO_PKG_VERSION"))
        );
    }
}

#[test]
fn no_workspace_after_subcommand_is_accepted() {
    let dir = git_repo_with_config(".fnug.yaml");
    let output = fnug(dir.path(), &["check", "--no-tui", "--no-workspace"]);
    assert_success(&output);
}

#[test]
fn bare_config_filename_is_resolved() {
    for file in [".fnug.yaml", "ci.yaml"] {
        let dir = git_repo_with_config(file);
        assert_success(&fnug(dir.path(), &["-c", file, "check", "--no-tui"]));
        assert_success(&fnug(
            dir.path(),
            &["-c", file, "--no-workspace", "check", "--no-tui"],
        ));
    }
}

#[test]
fn parent_relative_config_keeps_workspace_members() {
    let dir = tempfile::tempdir().unwrap();
    git2::Repository::init(dir.path()).unwrap();
    std::fs::write(
        dir.path().join(".fnug.yaml"),
        "fnug_version: 0.1.0\nname: root\nworkspace: true\ncommands: []\n",
    )
    .unwrap();
    let pkg = dir.path().join("pkg");
    std::fs::create_dir(&pkg).unwrap();
    std::fs::write(
        pkg.join(".fnug.yaml"),
        "fnug_version: 0.1.0\nname: pkg\ncommands:\n  - name: member\n    cmd: echo PKG-MEMBER-RAN\n    auto:\n      always: true\n",
    )
    .unwrap();

    let output = fnug(&pkg, &["-c", "../.fnug.yaml", "check", "--no-tui"]);
    assert_success(&output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("PKG-MEMBER-RAN"), "{stdout}");
}

#[test]
fn missing_config_file_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    let output = fnug(dir.path(), &["-c", "missing.yaml", "check", "--no-tui"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(stderr.contains("Config file not found"), "{stderr}");
}

#[test]
fn mcp_install_writes_each_editors_servers_key() {
    let dir = tempfile::tempdir().unwrap();
    for (editor, key) in [
        (Editor::ClaudeCode, "mcpServers"),
        (Editor::VsCode, "servers"),
        (Editor::Cursor, "mcpServers"),
    ] {
        editor.install(dir.path()).unwrap();
        let written = read_json(&editor.config_path(dir.path()));
        assert_eq!(
            written,
            json!({key: {"fnug": {"type": "stdio", "command": "fnug", "args": ["mcp"]}}}),
            "{editor}"
        );
    }
}

// fnug 0.1.0-alpha.11..13 wrote Cursor's entry under VS Code's `servers` key.
#[test]
fn cursor_install_migrates_legacy_servers_entry() {
    let dir = tempfile::tempdir().unwrap();
    let path = Editor::Cursor.config_path(dir.path());
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        r#"{"servers": {"fnug": {"type": "stdio", "command": "fnug", "args": ["mcp"]}}}"#,
    )
    .unwrap();

    assert!(!Editor::Cursor.is_installed(dir.path()));
    Editor::Cursor.install(dir.path()).unwrap();

    let written = read_json(&path);
    assert!(written.get("servers").is_none(), "{written}");
    assert_eq!(written["mcpServers"]["fnug"]["command"], "fnug");
}

#[test]
fn cursor_remove_drops_legacy_servers_entry() {
    let dir = tempfile::tempdir().unwrap();
    let path = Editor::Cursor.config_path(dir.path());
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        r#"{
            "servers": {"fnug": {"command": "fnug"}},
            "mcpServers": {"fnug": {"command": "fnug"}, "other": {"command": "other"}}
        }"#,
    )
    .unwrap();

    Editor::Cursor.remove(dir.path()).unwrap();

    assert_eq!(
        read_json(&path),
        json!({"mcpServers": {"other": {"command": "other"}}})
    );
}
