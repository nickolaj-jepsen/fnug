use std::io::{self, IsTerminal, Write};
use std::num::NonZeroUsize;
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use clap::Args;
use tokio_util::sync::CancellationToken;

use fnug::check::{CheckOptions, CheckResult};
use fnug::commands::group::CommandGroup;
use fnug::config_file::parse_duration;
use fnug::runner::Selection;
use fnug::selectors::SelectOptions;

use crate::signals;

#[derive(Args, Debug)]
#[allow(clippy::struct_excessive_bools)]
pub struct CheckArgs {
    /// Stop on first failure
    #[arg(long)]
    fail_fast: bool,

    /// Never prompt to open the TUI on failure
    #[arg(long)]
    no_tui: bool,

    /// Suppress stdout/stderr for commands that pass
    #[arg(long)]
    mute_success: bool,

    /// Include commands with `auto.check: false`
    #[arg(long)]
    all: bool,

    /// Kill commands that run longer than DURATION (seconds, or e.g. 90s, 5m), unless their
    /// config sets `timeout`
    #[arg(long, value_name = "DURATION", value_parser = parse_duration)]
    timeout: Option<Duration>,

    /// Run up to N commands at once, each after its dependencies (0: one per CPU). Above 1,
    /// output is captured and printed as each command finishes
    #[arg(short, long, value_name = "N", default_value_t = 1)]
    jobs: usize,
}

impl CheckArgs {
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
        selection: Selection::Auto {
            options: SelectOptions::default(),
            include_manual: args.all,
        },
        fail_fast: args.fail_fast,
        mute_success: args.mute_success,
        jobs: args.jobs(),
        timeout: args.timeout.filter(|t| !t.is_zero()),
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
            Some(true) => {
                signals::restore_default();
                return Ok(CheckOutcome::OpenTui(result));
            }
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
