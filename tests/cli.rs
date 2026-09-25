//! End-to-end tests that run the built `fnug` binary.

use std::path::Path;
use std::process::{Command, Output};

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

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "exit {:?}, stderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn no_workspace_after_subcommand_is_accepted() {
    let dir = git_repo_with_config(".fnug.yaml");
    let output = fnug(dir.path(), &["check", "--no-tui", "--no-workspace"]);
    assert_success(&output);
}
