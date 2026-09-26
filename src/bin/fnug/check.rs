use std::io::{self, IsTerminal, Write};
use std::num::NonZeroUsize;
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use clap::{ArgGroup, Args};
use tokio_util::sync::CancellationToken;

use fnug::check::{CheckOptions, CheckResult};
use fnug::commands::group::CommandGroup;
use fnug::config_file::parse_duration;
use fnug::runner::Selection;
use fnug::selectors::{GitScope, SelectOptions};

use crate::signals;

#[derive(Args, Debug)]
#[allow(clippy::struct_excessive_bools)]
#[command(group(ArgGroup::new("source").multiple(false)))]
pub struct CheckArgs {
    /// Run these commands, by id or name, after their dependencies, instead of the ones changes
    /// select. They run even with `auto.check: false`
    #[arg(value_name = "TARGET", group = "source")]
    targets: Vec<String>,

    /// Run every command instead of the ones changes select
    #[arg(long, group = "source")]
    all: bool,

    /// Also run commands with `auto.check: false`
    #[arg(long, conflicts_with = "targets")]
    include_manual: bool,

    /// Select by the changes since the merge base of HEAD and REF, such as origin/main:
    /// commits since then plus uncommitted changes
    #[arg(long, value_name = "REF", group = "source")]
    base: Option<String>,

    /// Select by the changes staged for the next commit; unstaged and untracked changes don't
    /// count. In a pre-commit hook, the index git is committing
    #[arg(long, group = "source")]
    pub(crate) staged: bool,

    /// With --staged: set unstaged changes to tracked files aside while commands run, so they
    /// check exactly what is staged, and put them back afterwards
    // clap drops `requires` when the required arg conflicts with one given, as the other
    // sources do with --staged, so they are ruled out here too
    #[arg(long, requires = "staged", conflicts_with_all = ["targets", "all", "base"])]
    pub(crate) stash: bool,

    /// Stop on first failure
    #[arg(long)]
    pub(crate) fail_fast: bool,

    /// Never prompt to open the TUI on failure
    #[arg(long)]
    no_tui: bool,

    /// Suppress stdout/stderr for commands that pass
    #[arg(long)]
    pub(crate) mute_success: bool,

    /// Kill commands that run longer than DURATION (seconds, or e.g. 90s, 5m), unless their
    /// config sets `timeout`
    #[arg(long, value_name = "DURATION", value_parser = parse_duration)]
    timeout: Option<Duration>,

    /// Run up to N commands at once, each after its dependencies (0: one per CPU). Above 1,
    /// output is captured and printed as each command finishes
    #[arg(short, long, value_name = "N", default_value_t = 1)]
    jobs: usize,

    /// Let commands change tracked files, such as a formatter that fixes what it finds, without
    /// failing
    #[arg(long)]
    allow_modifications: bool,
}

impl CheckArgs {
    /// The commands the flags ask for.
    fn selection(&self) -> Selection {
        let include_manual = self.include_manual;
        if !self.targets.is_empty() {
            Selection::Targets(self.targets.clone())
        } else if self.all {
            Selection::All { include_manual }
        } else {
            let options = match &self.base {
                Some(base) => SelectOptions {
                    scope: GitScope::Since(base.clone()),
                    index_override: None,
                },
                None if self.staged => SelectOptions::from_env(GitScope::Staged),
                None => SelectOptions::default(),
            };
            Selection::Auto {
                options,
                include_manual,
            }
        }
    }

    fn jobs(&self) -> NonZeroUsize {
        NonZeroUsize::new(self.jobs)
            .unwrap_or_else(|| std::thread::available_parallelism().unwrap_or(NonZeroUsize::MIN))
    }
}

/// Outcome of the check subcommand.
pub enum CheckOutcome {
    /// Check completed, return this exit code.
    Done(ExitCode),
    /// Check failed interactively — user wants the TUI with these results.
    OpenTui(CheckResult),
}

/// Run the check subcommand. A termination signal stops the commands and exits with 128 plus
/// its number.
///
/// # Errors
///
/// Returns an error if the check runner or IO fails.
pub async fn run(
    args: &CheckArgs,
    config: &CommandGroup,
    cwd: &Path,
) -> Result<CheckOutcome, Box<dyn std::error::Error>> {
    let signals = signals::install()?;
    let opts = CheckOptions {
        selection: args.selection(),
        fail_fast: args.fail_fast,
        mute_success: args.mute_success,
        jobs: args.jobs(),
        timeout: args.timeout.filter(|t| !t.is_zero()),
        cancel_cause: signals.cause.clone(),
        detect_modifications: !args.allow_modifications,
        stash: args.stash,
    };
    let result = fnug::check::run(config, cwd, &opts, signals.cancel.clone()).await?;
    if let Some(code) = signals.exit_code() {
        return Ok(CheckOutcome::Done(code));
    }
    if result.exit_code == 0 {
        return Ok(CheckOutcome::Done(ExitCode::SUCCESS));
    }

    // On failure in an interactive terminal, offer to open the TUI
    if !args.no_tui && std::io::stdin().is_terminal() && std::io::stderr().is_terminal() {
        match prompt_open_tui(&signals.cancel).await? {
            Some(true) => return Ok(CheckOutcome::OpenTui(result)),
            Some(false) => {}
            None => {
                let code = signals.exit_code().unwrap_or(ExitCode::FAILURE);
                return Ok(CheckOutcome::Done(code));
            }
        }
    }

    Ok(CheckOutcome::Done(ExitCode::FAILURE))
}

/// Ask whether to open the TUI. Returns `None` if a signal cancelled the prompt.
async fn prompt_open_tui(cancel: &CancellationToken) -> io::Result<Option<bool>> {
    eprint!("Open TUI to investigate? [y/N] ");
    let _ = std::io::stderr().flush();
    // Read on a blocking thread, so a signal can still end the prompt
    let read = tokio::task::spawn_blocking(|| {
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer).map(|_| answer)
    });
    tokio::select! {
        answer = read => {
            let answer = answer.map_err(io::Error::other)??;
            Ok(Some(answer.trim().eq_ignore_ascii_case("y")))
        }
        () = cancel.cancelled() => {
            eprintln!();
            Ok(None)
        }
    }
}
