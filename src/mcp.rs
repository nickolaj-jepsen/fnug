use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use log::warn;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, Content, ServerCapabilities, ServerInfo};
use rmcp::{ServerHandler, ServiceExt, tool, tool_handler, tool_router, transport::stdio};
use serde::Serialize;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use crate::commands::command::Command;
use crate::commands::group::CommandGroup;
use crate::runner::{
    self, CaptureLimits, CommandReport, ExecOptions, Failure, NoHook, Outcome, OutputMode,
    PlanError, PlanOptions, RunReport, Selection, commands_with_group_path,
};
use crate::selectors::{self, SelectOptions};

/// How long shutdown waits for running commands to stop.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(10);

// ---------------------------------------------------------------------------
// Parameter structs
// ---------------------------------------------------------------------------

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct ListLintsParams {
    /// Filter by group name (case-insensitive substring match). Groups organize
    /// commands hierarchically, e.g. "tests", "lints".
    #[schemars(default)]
    group: Option<String>,
    /// Filter by auto-selection type: "git" (selected by changed files), "watch"
    /// (selected by file watcher), "always" (always runs), or "none" (manual only).
    #[schemars(default)]
    auto_type: Option<String>,
    /// Filter by command name or id (case-insensitive substring match).
    #[schemars(default)]
    name: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct FailFastParams {
    /// Stop on first failure instead of running all commands. Useful for quick
    /// feedback when you expect failures.
    #[schemars(default)]
    fail_fast: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct RunLintParams {
    /// The command name or id to run. Use `list_lints` to discover available
    /// commands. Matches by exact id or case-insensitive name.
    command: String,
}

// ---------------------------------------------------------------------------
// Response structs
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct LintInfo {
    id: String,
    name: String,
    cmd: String,
    cwd: String,
    auto_rules: AutoRules,
    depends_on: Vec<String>,
    group: String,
    selected: bool,
}

#[derive(Serialize)]
struct AutoRules {
    git: Option<bool>,
    watch: Option<bool>,
    always: Option<bool>,
    check: Option<bool>,
}

#[derive(Serialize)]
struct RunResult {
    total: usize,
    passed: usize,
    failed: usize,
    timed_out: usize,
    skipped: usize,
    cancelled: usize,
    not_run: usize,
    duration_ms: u128,
    commands: Vec<CommandRunResult>,
}

#[derive(Serialize)]
struct CommandRunResult {
    name: String,
    id: String,
    /// `passed`, `failed`, `timeout`, `skipped`, `cancelled` or `not_run`.
    status: &'static str,
    exit_code: Option<i32>,
    duration_ms: u128,
    /// stdout and stderr, merged in the order they were written.
    output: String,
}

impl From<&RunReport> for RunResult {
    fn from(report: &RunReport) -> Self {
        let counts = report.counts();
        Self {
            total: counts.total,
            passed: counts.passed,
            failed: counts.failed,
            timed_out: counts.timed_out,
            skipped: counts.skipped,
            cancelled: counts.cancelled,
            not_run: counts.not_run,
            duration_ms: report.duration.as_millis(),
            commands: report.commands.iter().map(CommandRunResult::from).collect(),
        }
    }
}

impl From<&CommandReport> for CommandRunResult {
    fn from(report: &CommandReport) -> Self {
        let captured = report
            .output
            .as_ref()
            .map(runner::CapturedOutput::text)
            .unwrap_or_default();
        let (status, exit_code, output) = match &report.outcome {
            Outcome::Passed => ("passed", Some(0), captured),
            Outcome::Failed(Failure::Exit(code)) => ("failed", Some(*code), captured),
            Outcome::Failed(Failure::Spawn(message)) => ("failed", None, message.clone()),
            Outcome::Failed(_) => ("failed", None, captured),
            Outcome::TimedOut(_) => ("timeout", None, captured),
            Outcome::Skipped { cause } => (
                "skipped",
                None,
                format!("Skipped: dependency '{cause}' failed"),
            ),
            Outcome::Cancelled => ("cancelled", None, captured),
            Outcome::NotRun => ("not_run", None, captured),
        };
        Self {
            name: report.name.clone(),
            id: report.id.clone(),
            status,
            exit_code,
            duration_ms: report.duration.unwrap_or_default().as_millis(),
            output,
        }
    }
}

// ---------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct FnugMcp {
    config: CommandGroup,
    cwd: PathBuf,
    /// Held for reading by every run; shutdown takes it for writing to wait for them.
    runs: Arc<RwLock<()>>,
    tool_router: ToolRouter<Self>,
}

/// Convert any `Display` error into an MCP internal error.
fn mcp_err(e: impl std::fmt::Display) -> rmcp::ErrorData {
    rmcp::ErrorData::internal_error(e.to_string(), None)
}

/// Report an unknown or ambiguous command, or unusable selection, as invalid parameters.
fn plan_err(e: &PlanError) -> rmcp::ErrorData {
    rmcp::ErrorData::invalid_params(e.to_string(), None)
}

/// Check whether a command matches the `list_lints` filter parameters.
fn matches_lint_filters(cmd: &Command, group_path: &str, params: &ListLintsParams) -> bool {
    if let Some(ref g) = params.group
        && !group_path.to_lowercase().contains(&g.to_lowercase())
    {
        return false;
    }
    if let Some(ref n) = params.name {
        let n_lower = n.to_lowercase();
        if !cmd.name.to_lowercase().contains(&n_lower) && !cmd.id.to_lowercase().contains(&n_lower)
        {
            return false;
        }
    }
    if let Some(ref at) = params.auto_type {
        match at.to_lowercase().as_str() {
            "git" if cmd.auto.git != Some(true) => return false,
            "watch" if cmd.auto.watch != Some(true) => return false,
            "always" if cmd.auto.always != Some(true) => return false,
            "none"
                if cmd.auto.git == Some(true)
                    || cmd.auto.watch == Some(true)
                    || cmd.auto.always == Some(true) =>
            {
                return false;
            }
            _ => {}
        }
    }
    true
}

#[tool_router]
impl FnugMcp {
    fn new(config: CommandGroup, cwd: PathBuf) -> Self {
        Self {
            config,
            cwd,
            runs: Arc::default(),
            tool_router: Self::tool_router(),
        }
    }

    #[tool(
        description = "List all configured lint/test commands in this project. Shows which \
        commands are currently auto-selected based on git changes. Call this first to \
        understand what checks are available before running them. Each result includes the \
        command's id, name, shell command, working directory, auto-selection rules, \
        dependencies, group, and whether it is currently selected by git changes. \
        Use filters to narrow results."
    )]
    async fn list_lints(
        &self,
        Parameters(params): Parameters<ListLintsParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let config = self.config.clone();
        let result = tokio::task::spawn_blocking(move || {
            let flat = commands_with_group_path(&config);
            let commands: Vec<&Command> = flat.iter().map(|(cmd, _)| *cmd).collect();
            let selection = selectors::select(&commands, &SelectOptions::default());
            for issue in &selection.issues {
                warn!("{issue}");
            }
            let selected_ids: HashSet<&str> = selection.ids().collect();

            let infos: Vec<LintInfo> = flat
                .into_iter()
                .filter(|(cmd, group_path)| matches_lint_filters(cmd, group_path, &params))
                .map(|(cmd, group_path)| LintInfo {
                    selected: selected_ids.contains(cmd.id.as_str()),
                    id: cmd.id.clone(),
                    name: cmd.name.clone(),
                    cmd: cmd.cmd.clone(),
                    cwd: cmd.cwd.display().to_string(),
                    auto_rules: AutoRules {
                        git: cmd.auto.git,
                        watch: cmd.auto.watch,
                        always: cmd.auto.always,
                        check: cmd.auto.check,
                    },
                    depends_on: cmd.depends_on.clone(),
                    group: group_path,
                })
                .collect();

            serde_json::to_string_pretty(&infos).map_err(mcp_err)
        })
        .await
        .map_err(mcp_err)??;

        Ok(CallToolResult::success(vec![Content::text(result)]))
    }

    #[tool(
        description = "Run all lint/test commands that are relevant to the current git \
        changes. This is the primary tool for verifying code correctness — call it after \
        making edits, before committing, or to validate a fix. Commands are auto-selected \
        based on which files were modified in git. Dependencies between commands are \
        resolved automatically (e.g. build before test). Returns per-command results with \
        status, exit code, output (stdout and stderr merged), and timing."
    )]
    async fn run_lints(
        &self,
        Parameters(params): Parameters<FailFastParams>,
        ct: CancellationToken,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let selection = Selection::Auto {
            options: SelectOptions::default(),
            include_manual: false,
        };
        let fail_fast = params.fail_fast.unwrap_or(false);
        self.run_and_serialize(selection, fail_fast, ct).await
    }

    #[tool(
        description = "Run a single lint/test command by name or id. Use this to re-run a \
        specific failing check after fixing it, or to run a check that wasn't auto-selected. \
        Use list_lints to discover available command names and ids. Dependencies are resolved \
        and run first automatically. Returns per-command results with status, exit code, \
        output (stdout and stderr merged), and timing."
    )]
    async fn run_lint(
        &self,
        Parameters(params): Parameters<RunLintParams>,
        ct: CancellationToken,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let selection = Selection::Targets(vec![params.command]);
        self.run_and_serialize(selection, false, ct).await
    }

    #[tool(
        description = "Run every configured lint/test command regardless of git changes, \
        except those marked `auto.check: false` (run those by name with run_lint). Use \
        this for a full sweep before creating a pull request, after large refactors, or when \
        you want to ensure nothing is broken across the entire project. Dependencies are \
        resolved automatically. Returns per-command results with status, exit code, output \
        (stdout and stderr merged), and timing."
    )]
    async fn run_all(
        &self,
        Parameters(params): Parameters<FailFastParams>,
        ct: CancellationToken,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let selection = Selection::All {
            include_manual: false,
        };
        let fail_fast = params.fail_fast.unwrap_or(false);
        self.run_and_serialize(selection, fail_fast, ct).await
    }
}

impl FnugMcp {
    /// Plan and run `selection`, one command at a time with captured output. Cancelling `ct`
    /// kills the running command's process group.
    async fn run_and_serialize(
        &self,
        selection: Selection,
        fail_fast: bool,
        ct: CancellationToken,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let _running = self.runs.read().await;
        let config = self.config.clone();
        let plan = tokio::task::spawn_blocking(move || {
            runner::plan(&config, &selection, &PlanOptions::default())
        })
        .await
        .map_err(mcp_err)?
        .map_err(|e| plan_err(&e))?;
        for warning in &plan.warnings {
            warn!("{warning}");
        }

        let opts = ExecOptions {
            fail_fast,
            output: OutputMode::Capture(CaptureLimits::DEFAULT),
            cancel: ct,
            ..ExecOptions::default()
        };
        let report = runner::execute(&plan, &self.cwd, &opts, &NoHook, &mut |_| {}).await;
        let json = serde_json::to_string_pretty(&RunResult::from(&report)).map_err(mcp_err)?;
        Ok(CallToolResult::success(vec![Content::text(json)]))
    }
}

#[tool_handler]
impl ServerHandler for FnugMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            instructions: Some(
                "Fnug is a command runner that knows which lints, tests, and checks to run \
                based on git changes. Use this server to verify code correctness after edits. \
                Recommended workflow: (1) call run_lints after making code changes to check \
                everything relevant, (2) if a specific check fails, fix the issue and re-run \
                just that check with run_lint, (3) use list_lints to explore available checks \
                or understand what would run, (4) use run_all for a full sweep of all check \
                commands before creating a PR or after large refactors. Always prefer these tools over running shell \
                commands directly — they automatically select the right checks for the files \
                you changed and handle dependency ordering."
                    .into(),
            ),
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            ..Default::default()
        }
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Serve MCP over stdio until the client closes stdin or `shutdown` is cancelled.
///
/// Either way, running tool calls are cancelled, which stops their commands, and the server
/// waits up to 10 s for them before returning.
///
/// # Errors
///
/// Returns an error if the MCP transport fails.
pub async fn run(
    config: CommandGroup,
    cwd: PathBuf,
    shutdown: CancellationToken,
) -> Result<(), Box<dyn std::error::Error>> {
    let server = FnugMcp::new(config, cwd);
    let runs = server.runs.clone();
    let service = server.serve(stdio()).await?;
    // Dropping the service, whichever branch wins, cancels every request's token
    tokio::select! {
        quit = service.waiting() => {
            quit?;
        }
        () = shutdown.cancelled() => {}
    }
    if tokio::time::timeout(SHUTDOWN_GRACE, runs.write())
        .await
        .is_err()
    {
        warn!("Commands still running after {SHUTDOWN_GRACE:?}; exiting anyway");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
