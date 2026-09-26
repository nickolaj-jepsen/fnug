use std::collections::HashSet;
use std::io::{IsTerminal, Write};
use std::path::Path;
use std::process::Command as ProcessCommand;
use std::time::Instant;

use log::warn;
use thiserror::Error;

use crate::commands::command::Command;
use crate::commands::group::CommandGroup;
use crate::runner::{self, PlanError, PlanOptions, Selection};
use crate::selectors::SelectOptions;

#[derive(Error, Debug)]
pub enum CheckError {
    #[error(transparent)]
    Plan(#[from] PlanError),
}

/// Result of executing a single command with captured output.
pub(crate) struct CommandResult {
    pub success: bool,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub duration: std::time::Duration,
}

/// Execute a single command, capturing stdout and stderr.
pub(crate) fn execute_command(cmd: &Command, cwd: &Path) -> CommandResult {
    let cmd_cwd = cmd.effective_cwd(cwd);

    let start = Instant::now();
    let output = ProcessCommand::new("sh")
        .arg("-c")
        .arg(&cmd.cmd)
        .current_dir(cmd_cwd)
        .envs(&cmd.env)
        .output();
    let duration = start.elapsed();

    match output {
        Ok(o) => CommandResult {
            success: o.status.success(),
            exit_code: o.status.code(),
            stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
            duration,
        },
        Err(e) => CommandResult {
            success: false,
            exit_code: None,
            stdout: String::new(),
            stderr: e.to_string(),
            duration,
        },
    }
}

/// Result of a headless check run, carrying state for TUI handoff.
pub struct CheckResult {
    pub exit_code: i32,
    /// All command IDs that were selected (including expanded deps).
    pub selected_ids: HashSet<String>,
    /// Command IDs that failed or were skipped due to a dependency failure.
    pub failed_ids: HashSet<String>,
}

/// ANSI color helpers — only emit escape codes when stderr is a terminal.
struct Style {
    color: bool,
}

impl Style {
    fn new() -> Self {
        Self {
            color: std::io::stderr().is_terminal(),
        }
    }

    fn style(&self, code: &str, s: &str) -> String {
        if self.color {
            format!("\x1b[{code}m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }

    fn bold(&self, s: &str) -> String {
        self.style("1", s)
    }

    fn green(&self, s: &str) -> String {
        self.style("32", s)
    }

    fn red(&self, s: &str) -> String {
        self.style("31", s)
    }

    fn yellow(&self, s: &str) -> String {
        self.style("33", s)
    }

    fn dim(&self, s: &str) -> String {
        self.style("2", s)
    }
}

fn format_duration(d: std::time::Duration) -> String {
    let total_secs = d.as_secs();
    let millis = d.subsec_millis();
    if total_secs < 60 {
        let tenths = millis / 100;
        format!("{total_secs}.{tenths}s")
    } else {
        let mins = total_secs / 60;
        let secs = total_secs % 60;
        let tenths = millis / 100;
        format!("{mins}m {secs}.{tenths}s")
    }
}

/// Execute a command with captured output, printing only failures.
fn execute_muted(cmd: &Command, cwd: &Path, sty: &Style) -> bool {
    let result = execute_command(cmd, cwd);
    if result.success {
        eprintln!(
            "{} {}",
            sty.green("PASS"),
            sty.dim(&format_duration(result.duration))
        );
    } else {
        eprintln!(
            "{} {}",
            sty.red("FAIL"),
            sty.dim(&format_duration(result.duration))
        );
        let _ = std::io::stderr().write_all(result.stdout.as_bytes());
        let _ = std::io::stderr().write_all(result.stderr.as_bytes());
    }
    result.success
}

/// Execute a command with inherited stdio (output streams directly to terminal).
fn execute_streaming(cmd: &Command, cwd: &Path, sty: &Style) -> bool {
    let cmd_cwd = cmd.effective_cwd(cwd);

    let start = Instant::now();
    let status = ProcessCommand::new("sh")
        .arg("-c")
        .arg(&cmd.cmd)
        .current_dir(cmd_cwd)
        .envs(&cmd.env)
        .status();
    let elapsed = start.elapsed();

    match status {
        Ok(s) if s.success() => {
            eprintln!(
                "{} {}",
                sty.green("PASS"),
                sty.dim(&format_duration(elapsed))
            );
            true
        }
        _ => {
            eprintln!("{} {}", sty.red("FAIL"), sty.dim(&format_duration(elapsed)));
            false
        }
    }
}

/// Run all selected commands headlessly and report results.
///
/// # Errors
///
/// Returns `CheckError::Plan` if git selection fails as a whole.
pub fn run(
    config: &CommandGroup,
    cwd: &Path,
    fail_fast: bool,
    mute_success: bool,
    all: bool,
) -> Result<CheckResult, CheckError> {
    let sty = Style::new();
    let selection = Selection::Auto {
        options: SelectOptions::default(),
        include_manual: all,
    };
    let plan = runner::plan(config, &selection, &PlanOptions::default())?;
    for warning in &plan.warnings {
        warn!("{warning}");
    }

    if plan.is_empty() {
        eprintln!("{}", sty.dim("No commands selected."));
        return Ok(CheckResult {
            exit_code: 0,
            selected_ids: HashSet::new(),
            failed_ids: HashSet::new(),
        });
    }

    let selected_ids: HashSet<String> = plan.ids().map(str::to_string).collect();
    let ordered: Vec<&Command> = plan.commands.iter().map(|c| &c.command).collect();

    // Execute sequentially
    let total = ordered.len();
    let total_start = Instant::now();
    let mut passed = 0usize;
    let mut skipped = 0usize;
    let mut failed_ids: HashSet<String> = HashSet::new();
    let counter_width = total.to_string().len();

    for (i, cmd) in ordered.iter().enumerate() {
        let idx = i + 1;
        let prefix = format!("[{idx:>counter_width$}/{total}]");

        // Skip if a dependency failed
        let dep_failed = cmd
            .depends_on
            .iter()
            .any(|dep: &String| failed_ids.contains(dep.as_str()));
        if dep_failed {
            eprintln!(
                "{} {} {}",
                sty.dim(&prefix),
                cmd.name,
                sty.yellow("SKIP (dependency failed)")
            );
            failed_ids.insert(cmd.id.clone());
            skipped += 1;
            continue;
        }

        eprint!("{} {} ", sty.bold(&prefix), cmd.name);
        let _ = std::io::stderr().flush();

        let success = if mute_success {
            execute_muted(cmd, cwd, &sty)
        } else {
            execute_streaming(cmd, cwd, &sty)
        };

        if success {
            passed += 1;
        } else {
            failed_ids.insert(cmd.id.clone());
            if fail_fast {
                eprintln!();
                print_summary(
                    &sty,
                    passed,
                    failed_ids.len(),
                    skipped,
                    total,
                    total_start.elapsed(),
                );
                return Ok(CheckResult {
                    exit_code: 1,
                    selected_ids,
                    failed_ids,
                });
            }
        }
    }

    eprintln!();
    print_summary(
        &sty,
        passed,
        failed_ids.len(),
        skipped,
        total,
        total_start.elapsed(),
    );
    let exit_code = i32::from(!failed_ids.is_empty());
    Ok(CheckResult {
        exit_code,
        selected_ids,
        failed_ids,
    })
}

fn print_summary(
    sty: &Style,
    passed: usize,
    failed: usize,
    skipped: usize,
    total: usize,
    elapsed: std::time::Duration,
) {
    let mut parts = Vec::new();
    if passed > 0 {
        parts.push(sty.green(&format!("{passed} passed")));
    }
    if failed > 0 {
        parts.push(sty.red(&format!("{failed} failed")));
    }
    if skipped > 0 {
        parts.push(sty.yellow(&format!("{skipped} skipped")));
    }

    eprintln!(
        "{} {} {}",
        sty.bold(&format!("{total} commands:")),
        parts.join(&sty.dim(", ")),
        sty.dim(&format!("({})", format_duration(elapsed)))
    );
}
