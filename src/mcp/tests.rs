use std::time::Duration;

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use tokio_util::sync::CancellationToken;

use super::FnugMcp;
use super::params::{FailFastParams, RunLintParams};

fn server(yaml: &str) -> (FnugMcp, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(".fnug.yaml");
    std::fs::write(&path, yaml).unwrap();
    let (config, cwd) = crate::load_config(path.to_str(), true).unwrap();
    (FnugMcp::new(config, cwd), dir)
}

fn json(result: &CallToolResult) -> serde_json::Value {
    let text = &result.content[0].as_text().unwrap().text;
    serde_json::from_str(text).unwrap()
}

fn fail_fast(fail_fast: bool) -> Parameters<FailFastParams> {
    Parameters(FailFastParams {
        fail_fast: Some(fail_fast),
    })
}

fn run_lint(command: &str) -> Parameters<RunLintParams> {
    Parameters(RunLintParams {
        command: command.to_string(),
    })
}

#[tokio::test]
async fn run_all_skips_check_false_commands() {
    let (server, _dir) = server(
        r"
name: root
commands:
  - name: lint
    cmd: 'true'
  - name: demo
    cmd: exit 1
    auto:
      check: false
",
    );
    let result = server
        .run_all(fail_fast(false), CancellationToken::new())
        .await
        .unwrap();
    let result = json(&result);
    assert_eq!(result["commands"].as_array().unwrap().len(), 1);
    assert_eq!(result["commands"][0]["name"], "lint");
    assert_eq!(result["failed"], 0);
}

#[tokio::test]
async fn run_lint_ambiguous_name_is_error() {
    let (server, _dir) = server(
        r"
name: root
children:
  - name: backend
    commands:
      - name: test
        cmd: 'true'
  - name: frontend
    commands:
      - name: test
        cmd: 'true'
",
    );
    let Err(err) = server
        .run_lint(run_lint("test"), CancellationToken::new())
        .await
    else {
        panic!("an ambiguous name ran a command");
    };
    assert!(err.message.contains("backend/test"), "{err:?}");
    assert!(err.message.contains("frontend/test"), "{err:?}");
}

#[tokio::test]
async fn run_lint_reports_merged_output_and_exit_code() {
    let (server, _dir) = server(
        r"
name: root
commands:
  - name: mixed
    cmd: 'echo o1; echo e1 >&2; echo o2; exit 3'
  - name: after
    cmd: 'true'
    depends_on: [mixed]
",
    );
    let result = server
        .run_lint(run_lint("after"), CancellationToken::new())
        .await
        .unwrap();
    let result = json(&result);
    let mixed = &result["commands"][0];
    assert_eq!(mixed["status"], "failed");
    assert_eq!(mixed["exit_code"], 3);
    assert_eq!(mixed["output"], "o1\ne1\no2\n");
    assert_eq!(result["commands"][1]["status"], "skipped");
    assert_eq!(
        (&result["failed"], &result["skipped"]),
        (&1.into(), &1.into())
    );
}

#[tokio::test]
async fn fail_fast_lists_not_run_commands() {
    let (server, _dir) = server(
        r"
name: root
commands:
  - name: first
    cmd: 'exit 1'
  - name: second
    cmd: 'true'
",
    );
    let result = server
        .run_all(fail_fast(true), CancellationToken::new())
        .await
        .unwrap();
    let result = json(&result);
    assert_eq!(result["total"], 2);
    assert_eq!(result["not_run"], 1);
    assert_eq!(result["commands"][1]["status"], "not_run");
}

#[tokio::test]
async fn cancelled_run_reports_cancelled() {
    let (server, dir) = server(
        r"
name: root
commands:
  - name: hang
    cmd: 'touch started; exec sleep 30'
",
    );
    let ct = CancellationToken::new();
    let started = dir.path().join("started");
    let cancel = ct.clone();
    tokio::task::spawn_blocking(move || {
        assert!(crate::pty::test_util::wait_until(
            Duration::from_secs(10),
            || { started.exists() }
        ));
        cancel.cancel();
    });
    let result = server.run_lint(run_lint("hang"), ct).await.unwrap();
    let result = json(&result);
    assert_eq!(result["cancelled"], 1);
    assert_eq!(result["commands"][0]["status"], "cancelled");
}

#[test]
fn server_info_is_fnug() {
    use rmcp::ServerHandler;

    let (server, _dir) = server("name: root\n");
    let info = server.get_info().server_info;
    assert_eq!(info.name, "fnug");
    assert_eq!(info.version, env!("CARGO_PKG_VERSION"));
    assert_eq!(info.title.as_deref(), Some("Fnug"));
}

#[test]
fn list_lints_read_only_annotation() {
    let annotations = FnugMcp::list_lints_tool_attr().annotations.unwrap();
    assert_eq!(annotations.read_only_hint, Some(true));
    assert_eq!(annotations.open_world_hint, Some(false));
    for tool in [
        FnugMcp::run_lints_tool_attr(),
        FnugMcp::run_lint_tool_attr(),
        FnugMcp::run_all_tool_attr(),
    ] {
        let annotations = tool.annotations.unwrap();
        assert_ne!(annotations.read_only_hint, Some(true), "{}", tool.name);
        assert_eq!(annotations.open_world_hint, Some(false), "{}", tool.name);
    }
}
