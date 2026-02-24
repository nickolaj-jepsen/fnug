use std::path::PathBuf;
use std::process::ExitCode;

use fnug::commands::group::CommandGroup;

/// Start the MCP server over stdio.
///
/// # Errors
///
/// Returns an error if the MCP transport fails.
pub async fn run(
    config: CommandGroup,
    cwd: PathBuf,
) -> Result<ExitCode, Box<dyn std::error::Error>> {
    fnug::mcp::run(config, cwd).await?;
    Ok(ExitCode::SUCCESS)
}
