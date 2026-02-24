use std::collections::HashSet;
use std::path::PathBuf;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, Content, ServerCapabilities, ServerInfo};
use rmcp::{ServerHandler, ServiceExt, tool, tool_handler, tool_router, transport::stdio};
use serde::Serialize;

use crate::check::{execute_command, expand_dependencies, topo_sort};
use crate::commands::command::Command;
use crate::commands::group::CommandGroup;
use crate::selectors;

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
}

#[derive(Serialize)]
struct RunResult {
    total: usize,
    passed: usize,
    failed: usize,
    skipped: usize,
    duration_ms: u128,
    commands: Vec<CommandRunResult>,
}

#[derive(Serialize)]
struct CommandRunResult {
    name: String,
    id: String,
    status: String,
    exit_code: Option<i32>,
    duration_ms: u128,
    stdout: String,
    stderr: String,
}

// ---------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct FnugMcp {
    config: CommandGroup,
    cwd: PathBuf,
    tool_router: ToolRouter<Self>,
}

/// Convert any `Display` error into an MCP internal error.
fn mcp_err(e: impl std::fmt::Display) -> rmcp::ErrorData {
    rmcp::ErrorData::internal_error(e.to_string(), None)
}

/// Walk the command tree, collecting (command, `group_path`) pairs.
fn flatten_commands<'a>(group: &'a CommandGroup, path: &str) -> Vec<(&'a Command, String)> {
    let mut result = Vec::new();
    for cmd in &group.commands {
        result.push((cmd, path.to_string()));
    }
    for child in &group.children {
        let child_path = if path.is_empty() {
            child.name.clone()
        } else {
            format!("{path} > {}", child.name)
        };
        result.extend(flatten_commands(child, &child_path));
    }
    result
}

#[tool_router]
impl FnugMcp {
    fn new(config: CommandGroup, cwd: PathBuf) -> Self {
        Self {
            config,
            cwd,
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
            let all_commands: Vec<Command> = config.all_commands().into_iter().cloned().collect();
            let selected = selectors::get_selected_commands(all_commands).map_err(mcp_err)?;
            let selected_ids: HashSet<&str> = selected.iter().map(|c| c.id.as_str()).collect();

            let flat = flatten_commands(&config, &config.name);

            let infos: Vec<LintInfo> = flat
                .into_iter()
                .filter(|(cmd, group_path): &(&Command, String)| {
                    if let Some(ref g) = params.group
                        && !group_path.to_lowercase().contains(&g.to_lowercase())
                    {
                        return false;
                    }
                    if let Some(ref n) = params.name {
                        let n_lower = n.to_lowercase();
                        if !cmd.name.to_lowercase().contains(&n_lower)
                            && !cmd.id.to_lowercase().contains(&n_lower)
                        {
                            return false;
                        }
                    }
                    if let Some(ref at) = params.auto_type {
                        match at.to_lowercase().as_str() {
                            "git" => {
                                if cmd.auto.git != Some(true) {
                                    return false;
                                }
                            }
                            "watch" => {
                                if cmd.auto.watch != Some(true) {
                                    return false;
                                }
                            }
                            "always" => {
                                if cmd.auto.always != Some(true) {
                                    return false;
                                }
                            }
                            "none" => {
                                if cmd.auto.git == Some(true)
                                    || cmd.auto.watch == Some(true)
                                    || cmd.auto.always == Some(true)
                                {
                                    return false;
                                }
                            }
                            _ => {}
                        }
                    }
                    true
                })
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
        pass/fail status, exit codes, stdout, stderr, and timing."
    )]
    async fn run_lints(
        &self,
        Parameters(params): Parameters<FailFastParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let fail_fast = params.fail_fast.unwrap_or(false);
        self.run_and_serialize(CommandSelection::GitSelected, fail_fast)
            .await
    }

    #[tool(
        description = "Run a single lint/test command by name or id. Use this to re-run a \
        specific failing check after fixing it, or to run a check that wasn't auto-selected. \
        Use list_lints to discover available command names and ids. Dependencies are resolved \
        and run first automatically. Returns per-command results with pass/fail status, exit \
        codes, stdout, stderr, and timing."
    )]
    async fn run_lint(
        &self,
        Parameters(params): Parameters<RunLintParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        self.run_and_serialize(CommandSelection::Single(params.command), false)
            .await
    }

    #[tool(
        description = "Run every configured lint/test command regardless of git changes. Use \
        this for a full sweep before creating a pull request, after large refactors, or when \
        you want to ensure nothing is broken across the entire project. Dependencies are \
        resolved automatically. Returns per-command results with pass/fail status, exit \
        codes, stdout, stderr, and timing."
    )]
    async fn run_all(
        &self,
        Parameters(params): Parameters<FailFastParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let fail_fast = params.fail_fast.unwrap_or(false);
        self.run_and_serialize(CommandSelection::All, fail_fast)
            .await
    }
}

impl FnugMcp {
    async fn run_and_serialize(
        &self,
        selection: CommandSelection,
        fail_fast: bool,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let config = self.config.clone();
        let cwd = self.cwd.clone();

        let result =
            tokio::task::spawn_blocking(move || run_commands(&config, &cwd, fail_fast, &selection))
                .await
                .map_err(mcp_err)??;

        let json = serde_json::to_string_pretty(&result).map_err(mcp_err)?;
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
                or understand what would run, (4) use run_all for a full sweep before creating \
                a PR or after large refactors. Always prefer these tools over running shell \
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
// Shared execution logic
// ---------------------------------------------------------------------------

enum CommandSelection {
    /// Select commands based on git changes and always rules.
    GitSelected,
    /// Run a single command by name or id.
    Single(String),
    /// Run every configured command.
    All,
}

fn run_commands(
    config: &CommandGroup,
    cwd: &std::path::Path,
    fail_fast: bool,
    selection: &CommandSelection,
) -> Result<RunResult, rmcp::ErrorData> {
    let all_commands: Vec<Command> = config.all_commands().into_iter().cloned().collect();

    // Hoisted so references in the GitSelected branch live long enough.
    let git_selected;

    let selected: Vec<&Command> = match *selection {
        CommandSelection::Single(ref target) => {
            let found = all_commands
                .iter()
                .find(|c| c.id == *target || c.name.eq_ignore_ascii_case(target))
                .ok_or_else(|| {
                    rmcp::ErrorData::invalid_params(format!("Command not found: {target}"), None)
                })?;
            vec![found]
        }
        CommandSelection::All => all_commands.iter().collect(),
        CommandSelection::GitSelected => {
            git_selected =
                selectors::get_selected_commands(all_commands.clone()).map_err(mcp_err)?;
            if git_selected.is_empty() {
                return Ok(RunResult {
                    total: 0,
                    passed: 0,
                    failed: 0,
                    skipped: 0,
                    duration_ms: 0,
                    commands: vec![],
                });
            }
            git_selected.iter().collect()
        }
    };

    let commands_to_run = expand_dependencies(&selected, &all_commands);
    let ordered = topo_sort(&commands_to_run);

    let total_start = std::time::Instant::now();
    let mut passed = 0usize;
    let mut failed_count = 0usize;
    let mut skipped = 0usize;
    let mut failed_ids: HashSet<String> = HashSet::new();
    let mut cmd_results = Vec::new();

    for cmd in &ordered {
        let dep_failed = cmd
            .depends_on
            .iter()
            .any(|dep| failed_ids.contains(dep.as_str()));

        if dep_failed {
            failed_ids.insert(cmd.id.clone());
            skipped += 1;
            cmd_results.push(CommandRunResult {
                name: cmd.name.clone(),
                id: cmd.id.clone(),
                status: "skipped".to_string(),
                exit_code: None,
                duration_ms: 0,
                stdout: String::new(),
                stderr: "Skipped: dependency failed".to_string(),
            });
            continue;
        }

        let result = execute_command(cmd, cwd);

        let status = if result.success { "passed" } else { "failed" };
        cmd_results.push(CommandRunResult {
            name: cmd.name.clone(),
            id: cmd.id.clone(),
            status: status.to_string(),
            exit_code: result.exit_code,
            duration_ms: result.duration.as_millis(),
            stdout: result.stdout,
            stderr: result.stderr,
        });

        if result.success {
            passed += 1;
        } else {
            failed_ids.insert(cmd.id.clone());
            failed_count += 1;
            if fail_fast {
                break;
            }
        }
    }

    Ok(RunResult {
        total: ordered.len(),
        passed,
        failed: failed_count,
        skipped,
        duration_ms: total_start.elapsed().as_millis(),
        commands: cmd_results,
    })
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Start the MCP server over stdio.
///
/// # Errors
///
/// Returns an error if the MCP transport fails.
pub async fn run(config: CommandGroup, cwd: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let server = FnugMcp::new(config, cwd);
    let service = server.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
