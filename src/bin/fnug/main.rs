mod check;
mod init_hooks;
mod mcp;
mod tui;

use std::process::ExitCode;

use clap::{Parser, Subcommand};

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

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Run selected commands headlessly (useful for pre-commit hooks)
    Check(check::CheckArgs),
    /// Install a git pre-commit hook that runs `fnug check`
    InitHooks(init_hooks::InitHooksArgs),
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

    let (config, cwd, config_path) = load_config(cli.config.as_deref())?;

    // Dispatch subcommands
    let check_result = match cli.command {
        Some(Commands::Check(ref args)) => match check::run(args, &config, &cwd)? {
            check::CheckOutcome::Done(code) => return Ok(code),
            check::CheckOutcome::OpenTui(result) => Some(result),
        },
        Some(Commands::InitHooks(ref args)) => return init_hooks::run(args, &cwd),
        Some(Commands::Mcp) => return mcp::run(config, cwd).await,
        None => None,
    };

    tui::run(config, cwd, config_path, cli.log_file, check_result).await
}
