use std::path::PathBuf;
use std::process::ExitCode;

use clap::Args;

#[derive(Args, Debug)]
pub struct InitHooksArgs {
    /// Overwrite existing hook
    #[arg(long)]
    force: bool,
}

/// Install git pre-commit hooks.
///
/// # Errors
///
/// Returns an error if hook installation fails.
pub fn run(args: &InitHooksArgs, cwd: &PathBuf) -> Result<ExitCode, Box<dyn std::error::Error>> {
    fnug::init_hooks::run(cwd, args.force)?;
    Ok(ExitCode::SUCCESS)
}
