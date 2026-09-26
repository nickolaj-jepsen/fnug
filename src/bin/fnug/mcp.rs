use std::process::ExitCode;

use fnug::LoadOptions;

use crate::signals;

/// Start the MCP server over stdio, loading the config with `load` on every tool call. SIGINT,
/// SIGTERM or SIGHUP stops running commands with the same signal and exits with 128 plus its
/// number.
///
/// # Errors
///
/// Returns an error if the MCP transport fails.
pub async fn run(load: LoadOptions) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let signals = signals::install()?;
    fnug::mcp::run(load, signals.cancel.clone(), signals.cause.clone()).await?;
    Ok(signals.exit_code().unwrap_or(ExitCode::SUCCESS))
}
