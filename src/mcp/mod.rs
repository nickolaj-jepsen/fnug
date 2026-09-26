//! The MCP server: `list_lints`, `run_lints`, `run_lint` and `run_all` over stdio.

mod params;
mod response;
mod text;

use std::fmt::Write as _;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::{Duration, Instant};

use log::warn;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, Content, Implementation, ServerCapabilities, ServerInfo};
use rmcp::{ServerHandler, ServiceExt, tool, tool_handler, tool_router, transport::stdio};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::commands::command::Command;
use crate::commands::group::CommandGroup;
use crate::runner::{
    self, CancelCause, CaptureLimits, ExecOptions, NoHook, OutputMode, PlanError, PlanOptions,
    Selection, commands_with_group_path,
};
use crate::selectors::{self, GitScope, SelectOptions};
use crate::{LoadOptions, LoadedConfig};

use params::{AutoType, ListLintsParams, RunAllParams, RunLintParams, RunLintsParams};
use response::{LintInfo, Run, RunScope};

/// How long shutdown waits for running commands to stop.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(10);

/// How many commands an error for an unknown command lists.
const MAX_LISTED: usize = 50;

/// Time limit for commands without a `timeout` when a run leaves out `timeout_secs`.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// Output kept of each command: more than a result shows, so cleaning can shrink it first.
const CAPTURE: CaptureLimits = CaptureLimits {
    head: 64 * 1024,
    tail: 256 * 1024,
};

// ---------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------

/// How a run tool runs its commands and reports them.
#[derive(Debug, Clone, Copy)]
struct RunOptions {
    fail_fast: bool,
    verbose: bool,
    /// For commands without a `timeout` of their own.
    timeout: Option<Duration>,
    jobs: NonZeroUsize,
}

/// The `jobs` parameter: one per CPU when left out or 0.
fn jobs(jobs: Option<usize>) -> NonZeroUsize {
    jobs.and_then(NonZeroUsize::new)
        .unwrap_or_else(|| std::thread::available_parallelism().unwrap_or(NonZeroUsize::MIN))
}

/// The `timeout_secs` parameter: [`DEFAULT_TIMEOUT`] when left out, and no limit for 0.
fn timeout(secs: Option<u64>) -> Duration {
    secs.map_or(DEFAULT_TIMEOUT, Duration::from_secs)
}

#[derive(Debug, Clone)]
pub struct FnugMcp {
    /// How to load the config, which every tool call does afresh.
    load: LoadOptions,
    /// The signal that shut the server down, which running commands then get too.
    cancel_cause: CancelCause,
    /// Held by each run from planning to its last command, so overlapping calls take turns;
    /// shutdown takes it to wait for the running one.
    run_lock: Arc<Mutex<()>>,
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

/// A tool error for a target that names no single command, listing the candidates or the
/// available commands, or for git selection that failed as a whole.
fn plan_error(e: &PlanError, config: &CommandGroup) -> CallToolResult {
    let mut message = e.to_string();
    match e {
        PlanError::NotFound { .. } => {
            let commands = commands_with_group_path(config);
            message.push_str("\nAvailable commands (id: name (group)):");
            for (cmd, group) in commands.iter().take(MAX_LISTED) {
                let _ = write!(message, "\n- {}: {} ({group})", cmd.id, cmd.name);
            }
            if commands.len() > MAX_LISTED {
                let more = commands.len() - MAX_LISTED;
                let _ = write!(message, "\n… and {more} more; list_lints shows them all");
            }
        }
        PlanError::Ambiguous { .. } => {}
        PlanError::Selection(_) => message.insert_str(0, "Git selection failed: "),
    }
    tool_error(message)
}

/// Git selection that compares with `base`, or else looks at uncommitted changes.
fn select_options(base: Option<&str>) -> SelectOptions {
    SelectOptions {
        scope: base.map_or(GitScope::WorkingTree, |base| GitScope::Since(base.into())),
        index_override: None,
    }
}

/// The plan selection for what a run tool asked for.
fn selection(scope: &RunScope) -> Selection {
    match scope {
        RunScope::Changes {
            base,
            include_manual,
        } => Selection::Auto {
            options: select_options(base.as_deref()),
            include_manual: *include_manual,
        },
        RunScope::All { include_manual } => Selection::All {
            include_manual: *include_manual,
        },
        RunScope::Named(command) => Selection::Targets(vec![command.clone()]),
    }
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
    let auto = &cmd.auto;
    match params.auto_type {
        None => true,
        Some(AutoType::Git) => auto.git == Some(true),
        Some(AutoType::Watch) => auto.watch == Some(true),
        Some(AutoType::Always) => auto.always == Some(true),
        Some(AutoType::None) => ![auto.git, auto.watch, auto.always].contains(&Some(true)),
    }
}

#[tool_router]
impl FnugMcp {
    fn new(load: LoadOptions, cancel_cause: CancelCause) -> Self {
        Self {
            load,
            cancel_cause,
            run_lock: Arc::default(),
            tool_router: Self::tool_router(),
        }
    }

    #[tool(
        description = "List all configured lint/test commands in this project. Shows which \
        commands are currently auto-selected based on git changes. Call this first to \
        understand what checks are available before running them. Each result includes the \
        command's id, name, shell command, working directory, auto-selection rules, \
        dependencies, group, whether it is currently selected by git changes (and why: \
        reason and matched_files), and runs_in_check, which is false for commands that \
        run_lints and run_all skip because of auto.check: false. Use filters to narrow \
        results, and base to select by the changes since a branch point.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn list_lints(
        &self,
        Parameters(params): Parameters<ListLintsParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let result = self.with_config(move |loaded| {
            let flat = commands_with_group_path(&loaded.root);
            let commands: Vec<&Command> = flat.iter().map(|(cmd, _)| *cmd).collect();
            let selection = selectors::select(&commands, &select_options(params.base.as_deref()));
            let (fatal, issues): (Vec<_>, Vec<_>) =
                selection.issues.iter().partition(|i| i.is_fatal());
            if !fatal.is_empty() {
                let error = PlanError::Selection(fatal.into_iter().cloned().collect());
                return Err(plan_error(&error, &loaded.root));
            }
            for issue in issues {
                warn!("{issue}");
            }

            let infos: Vec<LintInfo> = flat
                .into_iter()
                .filter(|(cmd, group_path)| matches_lint_filters(cmd, group_path, &params))
                .map(|(cmd, group_path)| {
                    let selected = selection.get(&cmd.id);
                    response::lint_info(cmd, group_path, selected, &loaded.cwd)
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
        resolved automatically (e.g. build before test). Commands with auto.check: false \
        are skipped unless include_manual is set, and base (such as \"origin/main\") selects \
        by everything changed since the merge base with that revision, commits included, \
        instead of uncommitted changes only. An empty result's message says why nothing was \
        selected. Returns a compact JSON summary that lists failures first, then a text \
        block with the output of each command that failed or timed out: stdout and stderr \
        merged, terminal escapes removed, and cut to about 20 KiB, keeping its start and \
        end. Output of commands that passed is left out unless verbose is set.",
        annotations(open_world_hint = false)
    )]
    async fn run_lints(
        &self,
        Parameters(params): Parameters<RunLintsParams>,
        ct: CancellationToken,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let scope = RunScope::Changes {
            base: params.base,
            include_manual: params.include_manual.unwrap_or(false),
        };
        let run = RunOptions {
            fail_fast: params.fail_fast.unwrap_or(false),
            verbose: params.verbose.unwrap_or(false),
            timeout: Some(timeout(params.timeout_secs)),
            jobs: jobs(params.jobs),
        };
        self.run_and_serialize(scope, run, ct).await
    }

    #[tool(
        description = "Run a single lint/test command by name or id. Use this to re-run a \
        specific failing check after fixing it, or to run a check that wasn't auto-selected. \
        Use list_lints to discover available command names and ids; a name that matches no \
        command or several is an error listing the candidates. Dependencies are resolved \
        and run first automatically. Returns a compact JSON summary that lists failures \
        first, then a text block with the output of each command that failed or timed out: \
        stdout and stderr merged, terminal escapes removed, and cut to about 20 KiB, keeping \
        its start and end. Output of commands that passed is left out unless verbose is set.",
        annotations(open_world_hint = false)
    )]
    async fn run_lint(
        &self,
        Parameters(params): Parameters<RunLintParams>,
        ct: CancellationToken,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let run = RunOptions {
            fail_fast: false,
            verbose: params.verbose.unwrap_or(false),
            timeout: Some(timeout(params.timeout_secs)),
            jobs: NonZeroUsize::MIN,
        };
        self.run_and_serialize(RunScope::Named(params.command), run, ct)
            .await
    }

    #[tool(
        description = "Run every configured lint/test command regardless of git changes, \
        except those marked auto.check: false unless include_manual is set (or run one by \
        name with run_lint). Use this for a full sweep before creating a pull request, after \
        large refactors, or when you want to ensure nothing is broken across the entire \
        project. Dependencies are resolved automatically. Returns a compact JSON summary \
        that lists failures first, then a text block with the output of each command that \
        failed or timed out: stdout and stderr merged, terminal escapes removed, and cut to \
        about 20 KiB, keeping its start and end. Output of commands that passed is left out \
        unless verbose is set.",
        annotations(open_world_hint = false)
    )]
    async fn run_all(
        &self,
        Parameters(params): Parameters<RunAllParams>,
        ct: CancellationToken,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let scope = RunScope::All {
            include_manual: params.include_manual.unwrap_or(false),
        };
        let run = RunOptions {
            fail_fast: params.fail_fast.unwrap_or(false),
            verbose: params.verbose.unwrap_or(false),
            timeout: Some(timeout(params.timeout_secs)),
            jobs: jobs(params.jobs),
        };
        self.run_and_serialize(scope, run, ct).await
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

    /// Plan and run what `scope` asks for, with captured output, once no other run is in
    /// progress. Cancelling `ct` stops waiting, or kills the running commands' process groups.
    async fn run_and_serialize(
        &self,
        scope: RunScope,
        run: RunOptions,
        ct: CancellationToken,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let (_turn, queued) = if let Ok(turn) = self.run_lock.try_lock() {
            (turn, None)
        } else {
            let since = Instant::now();
            tokio::select! {
                biased;
                () = ct.cancelled() => {
                    return Ok(tool_error(
                        "Cancelled while waiting for another run to finish; nothing ran.".into(),
                    ));
                }
                turn = self.run_lock.lock() => (turn, Some(since.elapsed())),
            }
        };
        let selection = selection(&scope);
        let planned = self.with_config(move |loaded| {
            let plan = runner::plan(&loaded.root, &selection, &PlanOptions::default())
                .map_err(|e| plan_error(&e, &loaded.root))?;
            let selectable = commands_with_group_path(&loaded.root)
                .iter()
                .any(|(cmd, _)| cmd.auto.git == Some(true) || cmd.auto.always == Some(true));
            Ok((plan, loaded.cwd, selectable))
        });
        let (plan, cwd, selectable) = match planned.await {
            Ok(planned) => planned,
            Err(result) => return Ok(result),
        };
        for warning in &plan.warnings {
            warn!("{warning}");
        }

        let opts = ExecOptions {
            jobs: run.jobs,
            fail_fast: run.fail_fast,
            output: OutputMode::Capture(CAPTURE),
            default_timeout: run.timeout,
            cancel: ct,
            cancel_cause: self.cancel_cause.clone(),
            ..ExecOptions::default()
        };
        let report = runner::execute(&plan, &cwd, &opts, &NoHook, &mut |_| {}).await;
        response::run_result(&Run {
            scope: &scope,
            plan: &plan,
            report: &report,
            root: &cwd,
            verbose: run.verbose,
            queued,
            selectable,
        })
        .map_err(mcp_err)
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
                right checks for the files you changed and handle dependency ordering. Each \
                run returns a JSON summary with failures first, then the output of each \
                command that failed; read the summary's message before the output. Runs take \
                turns, so a call made while another run is in progress waits for it. The \
                config is reloaded on every call."
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
    let run_lock = server.run_lock.clone();
    let service = server.serve(stdio()).await?;
    // Dropping the service, whichever branch wins, cancels every request's token
    tokio::select! {
        quit = service.waiting() => {
            quit?;
        }
        () = shutdown.cancelled() => {}
    }
    if tokio::time::timeout(SHUTDOWN_GRACE, run_lock.lock())
        .await
        .is_err()
    {
        warn!("Commands still running after {SHUTDOWN_GRACE:?}; exiting anyway");
    }
    Ok(())
}

#[cfg(test)]
mod tests;
