use std::path::Path;
use std::process::ExitCode;

use clap::Args;

use fnug::commands::group::CommandGroup;

#[derive(Args, Debug)]
pub struct SetupArgs {}

/// Run the interactive setup wizard.
///
/// # Errors
///
/// Returns an error if setup fails.
pub fn run(
    _args: &SetupArgs,
    cwd: &Path,
    config: Option<&CommandGroup>,
) -> Result<ExitCode, Box<dyn std::error::Error>> {
    fnug::setup::run(cwd, config)?;
    Ok(ExitCode::SUCCESS)
}
