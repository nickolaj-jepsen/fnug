use std::io::{IsTerminal, Write};
use std::path::Path;
use std::process::ExitCode;

use clap::Args;

use fnug::check::CheckResult;
use fnug::commands::group::CommandGroup;

#[derive(Args, Debug)]
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
}

/// Outcome of the check subcommand.
pub enum CheckOutcome {
    /// Check completed, return this exit code.
    Done(ExitCode),
    /// Check failed interactively — user wants the TUI with these results.
    OpenTui(CheckResult),
}

/// Run the check subcommand.
///
/// # Errors
///
/// Returns an error if the check runner or IO fails.
pub fn run(
    args: &CheckArgs,
    config: &CommandGroup,
    cwd: &Path,
) -> Result<CheckOutcome, Box<dyn std::error::Error>> {
    let result = fnug::check::run(config, cwd, args.fail_fast, args.mute_success)?;
    if result.exit_code == 0 {
        return Ok(CheckOutcome::Done(ExitCode::SUCCESS));
    }

    // On failure in an interactive terminal, offer to open the TUI
    if !args.no_tui && std::io::stdin().is_terminal() && std::io::stderr().is_terminal() {
        eprint!("Open TUI to investigate? [y/N] ");
        let _ = std::io::stderr().flush();
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        if answer.trim().eq_ignore_ascii_case("y") {
            return Ok(CheckOutcome::OpenTui(result));
        }
    }

    Ok(CheckOutcome::Done(ExitCode::FAILURE))
}
