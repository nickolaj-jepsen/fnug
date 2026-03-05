mod check;
mod mcp;
mod setup;
mod tui;

use std::process::ExitCode;

use clap::{Parser, Subcommand};
use log::LevelFilter;

use fnug::load_config;

#[derive(Parser, Debug)]
#[command(name = "fnug", about = "TUI command runner based on git changes")]
struct Cli {
    /// Path to config file (auto-detected if not specified)
    #[arg(short, long)]
    config: Option<String>,

    /// Log file path (enables file logging in addition to TUI log panel)
    #[arg(long)]
    log_file: Option<String>,

    /// Log level [default: info]
    #[arg(long, value_parser = parse_level_filter)]
    log_level: Option<LevelFilter>,

    /// Disable workspace resolution (don't search for a parent workspace root)
    #[arg(long)]
    no_workspace: bool,

    #[command(subcommand)]
    command: Option<Commands>,
}

fn parse_level_filter(s: &str) -> Result<LevelFilter, String> {
    s.parse().map_err(|_| {
        format!("invalid log level '{s}', expected one of: off, error, warn, info, debug, trace")
    })
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Run selected commands headlessly (useful for pre-commit hooks)
    Check(check::CheckArgs),
    /// Interactive setup wizard for git hooks and MCP server configuration
    Setup(setup::SetupArgs),
    /// Start an MCP server over stdio
    Mcp,
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("Error: {e}");
            ExitCode::FAILURE
        }
    }
}

#[tokio::main]
async fn run() -> Result<ExitCode, Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    // Setup can work without a config file
    if let Some(Commands::Setup(ref args)) = cli.command {
        let config_result = load_config(cli.config.as_deref(), cli.no_workspace);
        let (config, cwd) = match config_result {
            Ok((config, cwd)) => (Some(config), cwd),
            Err(_) => (None, std::env::current_dir()?),
        };
        return setup::run(args, &cwd, config.as_ref());
    }

    let (config, cwd) = load_config(cli.config.as_deref(), cli.no_workspace)?;

    // Dispatch subcommands
    let check_result = match cli.command {
        Some(Commands::Check(ref args)) => match check::run(args, &config, &cwd)? {
            check::CheckOutcome::Done(code) => return Ok(code),
            check::CheckOutcome::OpenTui(result) => Some(result),
        },
        Some(Commands::Mcp) => return mcp::run(config, cwd).await,
        Some(Commands::Setup(_)) => unreachable!(),
        None => None,
    };

    tui::run(config, cwd, cli.log_file, cli.log_level, check_result).await
}
