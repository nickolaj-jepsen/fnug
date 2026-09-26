//! The MCP server: `list_lints`, `run_lints`, `run_lint` and `run_all` over stdio.

mod params;
mod response;

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use log::warn;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, Content, Implementation, ServerCapabilities, ServerInfo};
use rmcp::{ServerHandler, ServiceExt, tool, tool_handler, tool_router, transport::stdio};
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use crate::commands::command::Command;
use crate::runner::{
    self, CancelCause, CaptureLimits, ExecOptions, NoHook, OutputMode, PlanError, PlanOptions,
    Selection, commands_with_group_path,
};
use crate::selectors::{self, SelectOptions};
use crate::{LoadOptions, LoadedConfig};

use params::{FailFastParams, ListLintsParams, RunLintParams};
use response::{AutoRules, LintInfo, RunResult};

/// How long shutdown waits for running commands to stop.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(10);

// ---------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct FnugMcp {
    /// How to load the config, which every tool call does afresh.
    load: LoadOptions,
    /// The signal that shut the server down, which running commands then get too.
    cancel_cause: CancelCause,
    /// Held for reading by every run; shutdown takes it for writing to wait for them.
    runs: Arc<RwLock<()>>,
    tool_router: ToolRouter<Self>,
}

/// Convert any `Display` error into an MCP internal error.
fn mcp_err(e: impl std::fmt::Display) -> rmcp::ErrorData {
    rmcp::ErrorData::internal_error(e.to_string(), None)
}

/// A tool result that reports a failure to the caller, as opposed to a protocol error.
fn tool_error(message: String) -> CallToolResult {
    CallToolResult::error(vec![Content::text(message)])
}

fn load_error(e: &crate::config_file::ConfigError) -> CallToolResult {
    tool_error(format!(
        "Failed to load the fnug config: {e}\nFix it and call the tool again; every call \
         reloads the config."
    ))
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
    fn new(load: LoadOptions, cancel_cause: CancelCause) -> Self {
        Self {
            load,
            cancel_cause,
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
        Use filters to narrow results.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn list_lints(
        &self,
        Parameters(params): Parameters<ListLintsParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let result = self.with_config(move |loaded| {
            let flat = commands_with_group_path(&loaded.root);
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

            Ok(serde_json::to_string_pretty(&infos).map_err(mcp_err))
        });
        match result.await {
            Ok(json) => Ok(CallToolResult::success(vec![Content::text(json?)])),
            Err(result) => Ok(result),
        }
    }

    #[tool(
        description = "Run all lint/test commands that are relevant to the current git \
        changes. This is the primary tool for verifying code correctness — call it after \
        making edits, before committing, or to validate a fix. Commands are auto-selected \
        based on which files were modified in git. Dependencies between commands are \
        resolved automatically (e.g. build before test). Returns per-command results with \
        status, exit code, output (stdout and stderr merged), and timing.",
        annotations(open_world_hint = false)
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
        output (stdout and stderr merged), and timing.",
        annotations(open_world_hint = false)
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
        (stdout and stderr merged), and timing.",
        annotations(open_world_hint = false)
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
    /// Load the config and pass it to `f`, on a blocking thread. A config that fails to load
    /// becomes the tool's error result.
    async fn with_config<T: Send + 'static>(
        &self,
        f: impl FnOnce(LoadedConfig) -> Result<T, CallToolResult> + Send + 'static,
    ) -> Result<T, CallToolResult> {
        let opts = self.load.clone();
        tokio::task::spawn_blocking(move || f(crate::load(&opts).map_err(|e| load_error(&e))?))
            .await
            .unwrap_or_else(|e| Err(tool_error(format!("fnug failed: {e}"))))
    }

    /// Plan and run `selection`, one command at a time with captured output. Cancelling `ct`
    /// kills the running command's process group.
    async fn run_and_serialize(
        &self,
        selection: Selection,
        fail_fast: bool,
        ct: CancellationToken,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let _running = self.runs.read().await;
        let planned = self.with_config(move |loaded| {
            let plan = runner::plan(&loaded.root, &selection, &PlanOptions::default());
            Ok((plan, loaded.cwd))
        });
        let (plan, cwd) = match planned.await {
            Ok(planned) => planned,
            Err(result) => return Ok(result),
        };
        let plan = plan.map_err(|e| plan_err(&e))?;
        for warning in &plan.warnings {
            warn!("{warning}");
        }

        let opts = ExecOptions {
            fail_fast,
            output: OutputMode::Capture(CaptureLimits::DEFAULT),
            cancel: ct,
            cancel_cause: self.cancel_cause.clone(),
            ..ExecOptions::default()
        };
        let report = runner::execute(&plan, &cwd, &opts, &NoHook, &mut |_| {}).await;
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
                commands before creating a PR or after large refactors. Always prefer these \
                tools over running shell commands directly — they automatically select the \
                right checks for the files you changed and handle dependency ordering."
                    .into(),
            ),
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            server_info: Implementation {
                name: "fnug".into(),
                title: Some("Fnug".into()),
                version: env!("CARGO_PKG_VERSION").into(),
                website_url: Some(env!("CARGO_PKG_HOMEPAGE").into()),
                ..Implementation::default()
            },
            ..ServerInfo::default()
        }
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Serve MCP over stdio until the client closes stdin or `shutdown` is cancelled.
///
/// Every tool call loads the config with `load`, so edits apply without a restart, and a config
/// that fails to load is the call's error result rather than a reason not to start. On
/// shutdown, running tool calls are cancelled, which stops their commands, and the server waits
/// up to 10 s for them before returning. Running commands get the signal recorded in
/// `cancel_cause`, as [`CancelCause`] describes, or `SIGTERM` without one.
///
/// # Errors
///
/// Returns an error if the MCP transport fails.
pub async fn run(
    load: LoadOptions,
    shutdown: CancellationToken,
    cancel_cause: CancelCause,
) -> Result<(), Box<dyn std::error::Error>> {
    // Logs the config's warnings, or why it doesn't load, once at startup
    let preflight = load.clone();
    if let Err(e) = tokio::task::spawn_blocking(move || crate::load(&preflight)).await? {
        warn!("{e}; every tool call reports this until the config is fixed");
    }
    let server = FnugMcp::new(load, cancel_cause);
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
mod tests;
