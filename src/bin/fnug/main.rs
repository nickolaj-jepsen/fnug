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
    #[arg(short, long, global = true)]
    config: Option<String>,

    /// Log file path (enables file logging in addition to TUI log panel)
    #[arg(long, global = true)]
    log_file: Option<String>,

    /// Log level [default: info]
    #[arg(long, global = true, value_parser = parse_level_filter)]
    log_level: Option<LevelFilter>,

    /// Disable workspace resolution (don't search for a parent workspace root)
    #[arg(long, global = true)]
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
    /// Print the config file's JSON Schema
    Schema,
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

    if let Some(Commands::Schema) = cli.command {
        print!("{}", fnug::schema::config_schema_json());
        return Ok(ExitCode::SUCCESS);
    }

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
        Some(Commands::Setup(_) | Commands::Schema) => unreachable!(),
        None => None,
    };

    let check_failed = check_result.is_some();
    let tui_code = tui::run(config, cwd, cli.log_file, cli.log_level, check_result).await?;
    Ok(handoff_exit_code(check_failed, tui_code))
}

fn handoff_exit_code(check_failed: bool, tui_code: ExitCode) -> ExitCode {
    // Keep the check's failure after the TUI closes, so `fnug check && git push` stops.
    if check_failed {
        ExitCode::FAILURE
    } else {
        tui_code
    }
}

#[cfg(test)]
mod tests {
    use fnug::setup::hooks;

    use super::*;

    #[test]
    fn hook_args_parse() {
        for no_workspace in [false, true] {
            let cli =
                Cli::try_parse_from(std::iter::once("fnug").chain(hooks::hook_args(no_workspace)))
                    .unwrap();
            assert_eq!(cli.no_workspace, no_workspace);
            assert!(matches!(cli.command, Some(Commands::Check(_))));
        }
    }

    #[test]
    fn installed_hook_line_parses() {
        for no_workspace in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            git2::Repository::init(dir.path()).unwrap();
            hooks::install(dir.path(), no_workspace).unwrap();
            let hook = std::fs::read_to_string(dir.path().join(".git/hooks/pre-commit")).unwrap();
            let line = hook.lines().last().unwrap();
            let cli = Cli::try_parse_from(line.split_whitespace()).unwrap();
            assert_eq!(cli.no_workspace, no_workspace);
            assert!(matches!(cli.command, Some(Commands::Check(_))));
        }
    }

    #[test]
    fn global_flags_parse_after_subcommand() {
        let cli = Cli::try_parse_from([
            "fnug",
            "check",
            "--no-workspace",
            "-c",
            "x.yaml",
            "--log-file",
            "fnug.log",
            "--log-level",
            "debug",
        ])
        .unwrap();
        assert!(cli.no_workspace);
        assert_eq!(cli.config.as_deref(), Some("x.yaml"));
        assert_eq!(cli.log_file.as_deref(), Some("fnug.log"));
        assert_eq!(cli.log_level, Some(LevelFilter::Debug));
    }

    #[test]
    fn handoff_keeps_check_failure() {
        assert_eq!(
            handoff_exit_code(true, ExitCode::SUCCESS),
            ExitCode::FAILURE
        );
        assert_eq!(
            handoff_exit_code(false, ExitCode::SUCCESS),
            ExitCode::SUCCESS
        );
        assert_eq!(
            handoff_exit_code(false, ExitCode::FAILURE),
            ExitCode::FAILURE
        );
    }

    // Hooks installed by 0.1.0-alpha.11..13 put the flag after the subcommand.
    #[test]
    fn legacy_hook_line_parses() {
        let cli = Cli::try_parse_from(
            "fnug check --fail-fast --mute-success --no-workspace".split_whitespace(),
        )
        .unwrap();
        assert!(cli.no_workspace);
        assert!(matches!(cli.command, Some(Commands::Check(_))));
    }
}
