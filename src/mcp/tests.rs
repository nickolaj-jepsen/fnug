use std::time::Duration;

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use tokio_util::sync::CancellationToken;

use super::FnugMcp;
use super::params::{AutoType, ListLintsParams, RunLintParams, RunParams};
use crate::LoadOptions;
use crate::runner::CancelCause;

fn server(yaml: &str) -> (FnugMcp, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(".fnug.yaml");
    std::fs::write(&path, yaml).unwrap();
    let load = LoadOptions {
        config: Some(path),
        no_workspace: true,
        ..LoadOptions::default()
    };
    (FnugMcp::new(load, CancelCause::default()), dir)
}

fn json(result: &CallToolResult) -> serde_json::Value {
    let text = &result.content[0].as_text().unwrap().text;
    serde_json::from_str(text).unwrap()
}

fn fail_fast(fail_fast: bool) -> Parameters<RunParams> {
    Parameters(RunParams {
        fail_fast: Some(fail_fast),
        verbose: None,
    })
}

fn run_lint(command: &str) -> Parameters<RunLintParams> {
    Parameters(RunLintParams {
        command: command.to_string(),
        verbose: None,
    })
}

fn list_all() -> Parameters<ListLintsParams> {
    Parameters(ListLintsParams {
        group: None,
        auto_type: None,
        name: None,
    })
}

/// The ids `list_lints` returns.
async fn listed_ids(server: &FnugMcp) -> Vec<String> {
    let result = server.list_lints(list_all()).await.unwrap();
    json(&result)
        .as_array()
        .unwrap()
        .iter()
        .map(|lint| lint["id"].as_str().unwrap().to_string())
        .collect()
}

fn text(result: &CallToolResult) -> &str {
    &result.content[0].as_text().unwrap().text
}

#[tokio::test]
async fn config_reloaded_between_calls() {
    let (server, dir) = server("name: root\ncommands:\n  - name: a\n    cmd: 'true'\n");
    assert_eq!(listed_ids(&server).await, ["a"]);

    std::fs::write(
        dir.path().join(".fnug.yaml"),
        "name: root\ncommands:\n  - name: a\n    cmd: 'true'\n  - name: b\n    cmd: 'echo from-b; exit 4'\n",
    )
    .unwrap();
    assert_eq!(listed_ids(&server).await, ["a", "b"]);
    let result = server
        .run_lint(run_lint("b"), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(json(&result)["commands"][0]["exit_code"], 4);
}

#[tokio::test]
async fn invalid_config_reported_as_tool_error() {
    let (server, dir) = server("name: root\ncommands:\n  - name: a\n    cmd: 'true'\n");
    std::fs::write(
        dir.path().join(".fnug.yaml"),
        "name: root\ncommands:\n  - name: a\n    cmd: 'true'\n    colour: red\n",
    )
    .unwrap();

    let listed = server.list_lints(list_all()).await.unwrap();
    let ran = server
        .run_lint(run_lint("a"), CancellationToken::new())
        .await
        .unwrap();
    for result in [listed, ran] {
        assert_eq!(result.is_error, Some(true), "{result:?}");
        assert!(text(&result).contains("colour"), "{}", text(&result));
    }
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

const SAME_NAMES: &str = r"
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
      - name: lint
        cmd: 'true'
";

#[tokio::test]
async fn ambiguous_name_is_tool_error() {
    let (server, _dir) = server(SAME_NAMES);
    let result = server
        .run_lint(run_lint("test"), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(result.is_error, Some(true), "{result:?}");
    let text = text(&result);
    assert!(text.contains("backend/test (root > backend)"), "{text}");
    assert!(text.contains("frontend/test (root > frontend)"), "{text}");
}

#[tokio::test]
async fn not_found_is_tool_error_listing_ids() {
    let (server, _dir) = server(SAME_NAMES);
    let result = server
        .run_lint(run_lint("lnit"), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(result.is_error, Some(true), "{result:?}");
    let text = text(&result);
    assert!(text.contains("did you mean 'lint'"), "{text}");
    for listed in [
        "backend/test: test (root > backend)",
        "frontend/test: test (root > frontend)",
        "lint: lint (root > frontend)",
    ] {
        assert!(text.contains(listed), "{text}");
    }
}

#[test]
fn auto_type_typo_rejected() {
    let parse = |value| serde_json::from_value::<ListLintsParams>(value);
    assert!(parse(serde_json::json!({"auto_type": "git"})).is_ok());
    assert!(parse(serde_json::json!({"auto_type": "gti"})).is_err());
    assert!(parse(serde_json::json!({"nmae": "test"})).is_err());
    assert!(serde_json::from_value::<RunLintParams>(serde_json::json!({"cmd": "x"})).is_err());

    let schema = serde_json::to_string(&*FnugMcp::list_lints_tool_attr().input_schema).unwrap();
    assert!(
        schema.contains(r#"["git","watch","always","none"]"#),
        "{schema}"
    );
}

#[tokio::test]
async fn auto_type_filters_by_rule() {
    let (server, _dir) = server(
        r"
name: root
commands:
  - name: on-change
    cmd: 'true'
    auto: {git: true, path: [.]}
  - name: every-time
    cmd: 'true'
    auto: {always: true}
  - name: by-hand
    cmd: 'true'
",
    );
    for (auto_type, expected) in [
        (AutoType::Git, "on-change"),
        (AutoType::Always, "every-time"),
        (AutoType::None, "by-hand"),
    ] {
        let params = ListLintsParams {
            auto_type: Some(auto_type),
            ..list_all().0
        };
        let result = server.list_lints(Parameters(params)).await.unwrap();
        let ids: Vec<_> = json(&result)
            .as_array()
            .unwrap()
            .iter()
            .map(|lint| lint["id"].clone())
            .collect();
        assert_eq!(ids, [expected], "{auto_type:?}");
    }
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
    assert_eq!(
        blocks(&result),
        ["### mixed: failed with exit code 3\no1\ne1\no2\n"]
    );
    let result = json(&result);
    let mixed = &result["commands"][0];
    assert_eq!(mixed["status"], "failed");
    assert_eq!(mixed["exit_code"], 3);
    assert_eq!(mixed["output_bytes"], 9);
    assert_eq!(result["commands"][1]["status"], "skipped");
    assert_eq!(
        result["commands"][1]["detail"],
        "not run because mixed failed"
    );
    assert_eq!(
        (&result["failed"], &result["skipped"]),
        (&1.into(), &1.into())
    );
}

/// The text blocks after the JSON summary.
fn blocks(result: &CallToolResult) -> Vec<String> {
    result.content[1..]
        .iter()
        .map(|content| {
            let text = &content.as_text().unwrap().text;
            // The header ends with the duration, which varies
            let (header, body) = text.split_once('\n').unwrap_or((text, ""));
            let header = header.rsplit_once(" (").map_or(header, |(h, _)| h);
            format!("{header}\n{body}")
        })
        .collect()
}

#[tokio::test]
async fn passing_output_omitted_by_default() {
    let (server, _dir) = server(
        r"
name: root
commands:
  - name: noisy
    cmd: 'seq 1 5000'
  - name: failing
    cmd: 'echo boom; exit 1'
",
    );
    let result = server
        .run_all(fail_fast(false), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        blocks(&result),
        ["### failing: failed with exit code 1\nboom\n"]
    );
    let summary = json(&result);
    assert_eq!(summary["ok"], false);
    // Failures first
    assert_eq!(summary["commands"][0]["id"], "failing");
    assert_eq!(summary["commands"][1]["id"], "noisy");
    assert_eq!(summary["commands"][1]["status"], "passed");
    assert_eq!(summary["commands"][1]["output_bytes"], 23_893);
    assert!(summary["commands"][1].get("output").is_none());
    assert!(text(&result).len() < 2_000, "{}", text(&result));
}

#[tokio::test]
async fn failure_output_capped_utf8_safe_and_ansi_stripped() {
    let (server, _dir) = server(
        r#"
name: root
commands:
  - name: loud
    cmd: 'awk ''BEGIN { for (i = 0; i < 3000; i++) printf "\033[31merror\033[0m 日本語 line %d\r\n", i; exit 1 }'''
"#,
    );
    let result = server
        .run_lint(run_lint("loud"), CancellationToken::new())
        .await
        .unwrap();
    let block = &result.content[1].as_text().unwrap().text;
    assert!(block.len() < 21 * 1024, "{} bytes", block.len());
    assert!(!block.contains('\x1b') && !block.contains('\r'), "{block}");
    assert!(!block.contains('\u{fffd}'), "{block}");
    assert!(block.contains("\nerror 日本語 line 0\n"), "{block}");
    assert!(block.ends_with("\nerror 日本語 line 2999\n"), "{block}");

    let summary = json(&result);
    let loud = &summary["commands"][0];
    let written =
        3000 * "\x1b[31merror\x1b[0m 日本語 line \r\n".len() + 10 + 90 * 2 + 900 * 3 + 2000 * 4;
    assert_eq!(loud["output_bytes"], written);
    let truncated = loud["truncated_bytes"].as_u64().unwrap();
    assert!(truncated > 0);
    assert!(
        block.contains(&format!("\n… {truncated} bytes omitted …\n")),
        "{block}"
    );
}

#[tokio::test]
async fn verbose_includes_passing_output() {
    let (server, _dir) = server(
        r"
name: root
commands:
  - name: noisy
    cmd: 'seq 1 5000'
  - name: quiet
    cmd: 'echo hi'
",
    );
    let params = RunParams {
        verbose: Some(true),
        ..fail_fast(false).0
    };
    let result = server
        .run_all(Parameters(params), CancellationToken::new())
        .await
        .unwrap();
    let blocks = blocks(&result);
    assert_eq!(blocks.len(), 2, "{blocks:?}");
    assert!(
        blocks[0].starts_with("### noisy: passed\n1\n2\n"),
        "{}",
        blocks[0]
    );
    assert!(blocks[0].ends_with("\n5000\n") && blocks[0].len() < 21 * 1024);
    assert_eq!(blocks[1], "### quiet: passed\nhi\n");
    assert!(json(&result)["commands"][0]["truncated_bytes"].as_u64() > Some(0));
}

#[tokio::test]
async fn failures_share_the_output_budget() {
    let (server, _dir) = server(
        r"
name: root
auto:
  always: true
commands:
  - {name: a, cmd: 'seq 1 30000; exit 1'}
  - {name: b, cmd: 'seq 1 30000; exit 1'}
  - {name: c, cmd: 'seq 1 30000; exit 1'}
  - {name: d, cmd: 'seq 1 30000; exit 1'}
  - {name: e, cmd: 'echo short; exit 1'}
",
    );
    let result = server
        .run_all(fail_fast(false), CancellationToken::new())
        .await
        .unwrap();
    let blocks = blocks(&result);
    assert_eq!(blocks.len(), 5);
    let total: usize = blocks.iter().map(String::len).sum();
    assert!(total < 62 * 1024, "{total} bytes");
    for block in &blocks[..4] {
        assert!(block.len() > 10 * 1024, "{} bytes", block.len());
        assert!(block.ends_with("\n30000\n"), "{block}");
    }
    assert_eq!(blocks[4], "### e: failed with exit code 1\nshort\n");
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
