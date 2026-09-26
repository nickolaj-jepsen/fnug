//! Headless check mode: run the selected commands without the TUI and report the results.

mod modified;
mod printer;
pub mod stash;

use std::num::NonZeroUsize;
use std::path::Path;
use std::time::Duration;

use log::warn;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::commands::group::CommandGroup;
use crate::runner::{
    self, CancelCause, CaptureLimits, ExecOptions, NoHook, OutputMode, Plan, PlanError,
    PlanOptions, RunEvent, RunReport, Selection,
};
use crate::selectors::SelectOptions;

use modified::ModificationGuard;
use printer::Printer;
use stash::StashError;

#[derive(Error, Debug)]
pub enum CheckError {
    #[error(transparent)]
    Plan(#[from] PlanError),
    #[error(transparent)]
    Stash(#[from] StashError),
}

/// How [`run`] selects and runs commands.
#[derive(Debug, Clone)]
#[allow(clippy::struct_excessive_bools)]
pub struct CheckOptions {
    pub selection: Selection,
    /// After the first failure, start nothing new and stop running commands.
    pub fail_fast: bool,
    /// Print output only for commands that don't pass.
    pub mute_success: bool,
    /// How many commands may run at once. Above 1, each command's output is captured and
    /// printed when it ends.
    pub jobs: NonZeroUsize,
    /// Kill commands that run longer than this, unless they have their own `timeout`.
    pub timeout: Option<Duration>,
    /// The signal fnug received that cancels [`run`]'s token, if any, which decides what
    /// running commands get.
    pub cancel_cause: CancelCause,
    /// Fail a command that passes but changes tracked files in its git work tree, as a
    /// formatter does when it finds something to fix.
    pub detect_modifications: bool,
    /// Set unstaged changes to tracked files aside while commands run, so they see what the
    /// index holds; see [`stash::stash`]. Only in the git work tree containing `cwd`.
    pub stash: bool,
}

impl Default for CheckOptions {
    /// Commands selected by their `auto` rules, one at a time, with live output, failing ones
    /// that change tracked files.
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
            cancel_cause: CancelCause::default(),
            detect_modifications: true,
            stash: false,
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
/// Cancelling `cancel` stops the running commands, and the summary covers what finished; while
/// git selection still runs, it returns at once with nothing run.
///
/// With `opts.stash`, unstaged changes are set aside only when something is selected, and put
/// back once every command has exited.
///
/// # Errors
///
/// Returns `CheckError::Plan` if a target names no single command or git selection fails as a
/// whole, and `CheckError::Stash` if unstaged changes can't be set aside or put back.
pub async fn run(
    config: &CommandGroup,
    cwd: &Path,
    opts: &CheckOptions,
    cancel: CancellationToken,
) -> Result<CheckResult, CheckError> {
    let Some(plan) = plan_unless_cancelled(config, &opts.selection, &cancel).await? else {
        printer::interrupted();
        return Ok(CheckResult {
            exit_code: 1,
            report: RunReport {
                cancelled: true,
                ..RunReport::default()
            },
        });
    };
    for warning in &plan.warnings {
        warn!("{warning}");
    }

    let output = if opts.jobs.get() == 1 && !opts.mute_success {
        OutputMode::Inherit
    } else {
        OutputMode::Capture(CaptureLimits::DEFAULT)
    };
    let mut printer = Printer::new(&plan, output, opts.jobs.get() == 1, opts.mute_success);
    if plan.is_empty() {
        if opts.stash
            && let Some(note) = stash::recover(cwd)?
        {
            printer.stash_recovered(&note);
        }
        printer.nothing_selected(config.all_commands().len());
        return Ok(CheckResult {
            exit_code: 0,
            report: RunReport::default(),
        });
    }

    let stashed = if opts.stash {
        let guard = stash::stash(cwd)?;
        if let Some(note) = guard.recovered() {
            printer.stash_recovered(note);
        }
        Some(guard)
    } else {
        None
    };

    let exec = ExecOptions {
        jobs: opts.jobs,
        fail_fast: opts.fail_fast,
        output,
        default_timeout: opts.timeout,
        cancel,
        cancel_cause: opts.cancel_cause.clone(),
        kill_grace: runner::KILL_GRACE,
    };
    let mut on_event = |event: RunEvent<'_>| printer.event(&event);
    let report = if opts.detect_modifications {
        let hook = ModificationGuard::new(cwd);
        runner::execute(&plan, cwd, &exec, &hook, &mut on_event).await
    } else {
        runner::execute(&plan, cwd, &exec, &NoHook, &mut on_event).await
    };
    printer.summary(&report);
    if let Some(guard) = stashed {
        if report.cancelled {
            // Signals only cancel by now, so fnug won't exit before this is done
            printer.restoring_stash();
        }
        printer.stash_restored(&guard.restore()?);
    }
    Ok(CheckResult {
        exit_code: i32::from(!report.success()),
        report,
    })
}

/// Plan the run on a blocking thread, since git selection can take a while in a big repo.
/// Returns `None` if `cancel` fires first; the scan then finishes unobserved.
async fn plan_unless_cancelled(
    config: &CommandGroup,
    selection: &Selection,
    cancel: &CancellationToken,
) -> Result<Option<Plan>, PlanError> {
    let config = config.clone();
    let selection = selection.clone();
    let scan = tokio::task::spawn_blocking(move || {
        runner::plan(&config, &selection, &PlanOptions::default())
    });
    tokio::select! {
        biased;
        () = cancel.cancelled() => Ok(None),
        plan = scan => match plan {
            Ok(plan) => plan.map(Some),
            Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
            // The runtime is shutting down
            Err(_) => Ok(None),
        },
    }
}
