//! Headless check mode: run the selected commands without the TUI and report the results.

mod printer;

use std::num::NonZeroUsize;
use std::path::Path;
use std::process::Command as ProcessCommand;
use std::time::{Duration, Instant};

use log::warn;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::commands::command::Command;
use crate::commands::group::CommandGroup;
use crate::runner::{
    self, CaptureLimits, ExecOptions, NoHook, OutputMode, PlanError, PlanOptions, RunReport,
    Selection,
};
use crate::selectors::SelectOptions;

use printer::Printer;

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

/// How [`run`] selects and runs commands.
#[derive(Debug, Clone)]
pub struct CheckOptions {
    pub selection: Selection,
    /// After the first failure, start nothing new and stop running commands.
    pub fail_fast: bool,
    /// Print output only for commands that don't pass.
    pub mute_success: bool,
    /// How many commands may run at once. Above 1, each command's output is captured and
    /// printed when it ends.
    pub jobs: NonZeroUsize,
    /// Kill commands that run longer than this.
    pub timeout: Option<Duration>,
}

impl Default for CheckOptions {
    /// Commands selected by their `auto` rules, one at a time, with live output.
    fn default() -> Self {
        Self {
            selection: Selection::Auto {
                options: SelectOptions::default(),
                include_manual: false,
            },
            fail_fast: false,
            mute_success: false,
            jobs: NonZeroUsize::MIN,
            timeout: None,
        }
    }
}

/// The result of a check run.
#[derive(Debug, Clone)]
pub struct CheckResult {
    /// 0 if every planned command passed or none was selected, otherwise 1.
    pub exit_code: i32,
    pub report: RunReport,
}

/// Run the commands `opts.selection` picks, after their dependencies, printing each result and
/// a summary to stderr.
///
/// With one job and `mute_success` off, commands use the terminal directly and their output
/// streams through. Otherwise it is captured and printed after each command ends.
/// Cancelling `cancel` stops the running commands, and the summary covers what finished.
///
/// # Errors
///
/// Returns `CheckError::Plan` if a target names no single command or git selection fails as a
/// whole.
pub async fn run(
    config: &CommandGroup,
    cwd: &Path,
    opts: &CheckOptions,
    cancel: CancellationToken,
) -> Result<CheckResult, CheckError> {
    let plan = runner::plan(config, &opts.selection, &PlanOptions::default())?;
    for warning in &plan.warnings {
        warn!("{warning}");
    }

    let output = if opts.jobs.get() == 1 && !opts.mute_success {
        OutputMode::Inherit
    } else {
        OutputMode::Capture(CaptureLimits::DEFAULT)
    };
    let mut printer = Printer::new(&plan, output, opts.mute_success);
    if plan.is_empty() {
        printer.nothing_selected();
        return Ok(CheckResult {
            exit_code: 0,
            report: RunReport::default(),
        });
    }

    let exec = ExecOptions {
        jobs: opts.jobs,
        fail_fast: opts.fail_fast,
        output,
        default_timeout: opts.timeout,
        cancel,
        kill_grace: runner::KILL_GRACE,
    };
    let report = runner::execute(&plan, cwd, &exec, &NoHook, &mut |event| {
        printer.event(&event);
    })
    .await;
    printer.summary(&report);
    Ok(CheckResult {
        exit_code: i32::from(!report.success()),
        report,
    })
}
