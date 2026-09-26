use std::path::PathBuf;
use std::process::ExitCode;

use fnug::commands::group::CommandGroup;

use crate::signals;

/// Start the MCP server over stdio. A termination signal stops running commands and exits with
/// 128 plus its number.
///
/// # Errors
///
/// Returns an error if the MCP transport fails.
pub async fn run(
    config: CommandGroup,
    cwd: PathBuf,
) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let signals = signals::install()?;
    fnug::mcp::run(config, cwd, signals.cancel.clone()).await?;
    Ok(signals.exit_code().unwrap_or(ExitCode::SUCCESS))
}
