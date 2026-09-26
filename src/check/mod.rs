//! Headless check mode: run the selected commands without the TUI and report the results.

mod modified;
mod printer;
pub mod stash;

use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::time::Duration;

use git2::{Repository, RepositoryOpenFlags};
use log::warn;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::commands::group::CommandGroup;
use crate::runner::{
    self, CancelCause, CaptureLimits, ExecOptions, NoHook, OutputMode, Plan, PlanError,
    PlanOptions, RunEvent, RunReport, Selection,
};
use crate::selectors::{GitScope, SelectOptions};

use modified::ModificationGuard;
use printer::Printer;
use stash::StashError;

#[derive(Error, Debug)]
pub enum CheckError {
    #[error(transparent)]
    Plan(#[from] PlanError),
    #[error(transparent)]
    Stash(#[from] StashError),
    #[error("--base needs a git repository, but {} is not in one: {message}", path.display())]
    BaseOutsideRepo { path: PathBuf, message: String },
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
    /// index holds; see [`stash::stash`]. Only in the git work tree of [`repo_dir`]. Output is
    /// then captured, so each command runs in a process group of its own that is stopped as a
    /// whole.
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
/// With one job and `mute_success` and `stash` off, commands use the terminal directly and
/// their output streams through. Otherwise it is captured and printed after each command ends.
/// Cancelling `cancel` stops the running commands, and the summary covers what finished; while
/// git selection still runs, it returns at once with nothing run.
///
/// With `opts.stash`, unstaged changes are set aside only when something is selected, and put
/// back once every command has exited.
///
/// # Errors
///
/// Returns `CheckError::Plan` if a target names no single command or git selection fails as a
/// whole, `CheckError::BaseOutsideRepo` if it selects by changes since a base and neither
/// [`repo_dir`] nor a git-enabled command's path is in a git work tree (see
/// [`check_base_repo`]), and `CheckError::Stash` if unstaged changes can't be set aside or put
/// back.
pub async fn run(
    config: &CommandGroup,
    cwd: &Path,
    opts: &CheckOptions,
    cancel: CancellationToken,
) -> Result<CheckResult, CheckError> {
    let repo = repo_dir(cwd);
    if let Selection::Auto { options, .. } = &opts.selection {
        check_base_repo(config, &options.scope, &repo)?;
    }
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

    // A command sharing fnug's process group is signalled alone, so what it started could write
    // to the work tree after the unstaged changes are back
    let output = if opts.jobs.get() == 1 && !opts.mute_success && !opts.stash {
        OutputMode::Inherit
    } else {
        OutputMode::Capture(CaptureLimits::DEFAULT)
    };
    let mut printer = Printer::new(&plan, output, opts.jobs.get() == 1, opts.mute_success);
    if !opts.stash
        && let Some(lock) = stash::pending(&repo)
    {
        printer.stash_pending(&lock);
    }
    if plan.is_empty() {
        if opts.stash
            && let Some(note) = stash::recover(&repo)?
        {
            printer.stash_recovered(&note);
        }
        // A pre-commit hook selects by what is staged, and can't take other flags
        let staged = matches!(
            &opts.selection,
            Selection::Auto { options, .. } if options.scope == GitScope::Staged
        );
        printer.nothing_selected(config.all_commands().len(), !staged);
        return Ok(CheckResult {
            exit_code: 0,
            report: RunReport::default(),
        });
    }

    let stashed = if opts.stash {
        let guard = stash::stash(&repo)?;
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

/// The directory whose git work tree `--stash` and the `--base` guard use: the process's working
/// directory, as for `--staged`, since git runs hooks at the top of the work tree it commits
/// in. `fallback`, such as the config's directory, if that is unknown.
#[must_use]
pub fn repo_dir(fallback: &Path) -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| fallback.to_path_buf())
}

/// Under the since-base scope, fail unless `dir` or an `auto.path` of one of `config`'s
/// git-enabled commands is in a git work tree. Selection alone only warns about paths outside
/// one, and would run just the `always` commands. When `dir` isn't in one, as at the root of a
/// workspace whose packages are repos of their own, the issues of each package's repo decide.
///
/// # Errors
///
/// Returns `CheckError::BaseOutsideRepo` if neither `dir` nor any such path is in a git work
/// tree.
pub fn check_base_repo(
    config: &CommandGroup,
    scope: &GitScope,
    dir: &Path,
) -> Result<(), CheckError> {
    if !matches!(scope, GitScope::Since(_)) {
        return Ok(());
    }
    let Err(message) = work_tree_of(dir) else {
        return Ok(());
    };
    let commands = config.all_commands();
    let mut paths = commands
        .iter()
        .filter(|cmd| cmd.auto.git == Some(true))
        .flat_map(|cmd| cmd.auto.paths());
    if paths.any(|path| work_tree_of(path).is_ok()) {
        return Ok(());
    }
    Err(CheckError::BaseOutsideRepo {
        path: dir.to_path_buf(),
        message,
    })
}

/// Fails with the reason unless `path`, or its nearest existing ancestor, is in a git work tree.
fn work_tree_of(path: &Path) -> Result<(), String> {
    let start = path.ancestors().find(|p| p.is_dir()).unwrap_or(path);
    match Repository::open_ext(start, RepositoryOpenFlags::CROSS_FS, &[] as &[&Path]) {
        Ok(repo) if repo.workdir().is_some() => Ok(()),
        Ok(_) => Err("it is in a bare repository".to_string()),
        Err(e) => Err(e.message().to_string()),
    }
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
