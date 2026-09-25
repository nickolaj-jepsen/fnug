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
