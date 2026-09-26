mod check;
mod mcp;
mod setup;
mod signals;
mod tui;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, Subcommand};
use log::LevelFilter;

use fnug::LoadOptions;
use fnug::logger::{LoggerConfig, LoggerHandle};

#[derive(Parser, Debug)]
#[command(
    name = "fnug",
    version,
    about = "TUI command runner based on git changes",
    // Keeps the global flags apart from a subcommand's own flags in its --help
    next_help_heading = "Global options"
)]
struct Cli {
    /// Path to config file (auto-detected if not specified)
    #[arg(short, long, global = true)]
    config: Option<String>,

    /// Also write logs to this file
    #[arg(long, global = true)]
    log_file: Option<String>,

    /// Log level [default: info, and warn for stderr]
    #[arg(long, global = true, value_parser = parse_level_filter)]
    log_level: Option<LevelFilter>,

    /// Disable workspace resolution (don't search for a parent workspace root)
    #[arg(long, global = true)]
    no_workspace: bool,

    /// Resolve the config's paths and workspace against DIR instead of the config's directory
    #[arg(long, global = true, value_name = "DIR")]
    root: Option<PathBuf>,

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
    let cli = Cli::parse();
    // Before anything else, so config loading's warnings reach stderr and the log file
    let logger = match fnug::logger::init(LoggerConfig {
        level: cli.log_level,
        file: cli.log_file.as_deref().map(PathBuf::from),
        stderr: true,
    }) {
        Ok(logger) => logger,
        Err(e) => {
            eprintln!("Error: {e}");
            return ExitCode::FAILURE;
        }
    };

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("Error: failed to start the async runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    let result = runtime.block_on(run(cli, logger));
    // Don't wait for blocking tasks that can't be cancelled, such as an in-flight git scan
    runtime.shutdown_timeout(Duration::from_millis(500));
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("Error: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli, logger: LoggerHandle) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let load_opts = LoadOptions {
        config: cli.config.as_deref().map(PathBuf::from),
        no_workspace: cli.no_workspace,
        root_dir: cli.root.clone(),
        trust: fnug::trust::TrustPolicy::from_env(),
        ..LoadOptions::default()
    };

    let (loaded, check_result) = match cli.command {
        // Commands that run without a config
        Some(Commands::Schema) => {
            print!("{}", fnug::schema::config_schema_json());
            return Ok(ExitCode::SUCCESS);
        }
        Some(Commands::Setup(ref args)) => return setup::run(args, &load_opts),
        // Commands that need one
        Some(Commands::Mcp) => {
            let loaded = fnug::load(&load_opts)?;
            return mcp::run(loaded.root, loaded.cwd).await;
        }
        Some(Commands::Check(ref args)) => {
            let loaded = fnug::load(&load_opts)?;
            match check::run(args, &loaded.root, &loaded.cwd).await? {
                check::CheckOutcome::Done(code) => return Ok(code),
                check::CheckOutcome::OpenTui(result) => (loaded, Some(result)),
            }
        }
        None => (fnug::load(&load_opts)?, None),
    };

    let check_failed = check_result.is_some();
    let tui_code = tui::run(loaded.root, loaded.cwd, logger, check_result).await?;
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
    fn installed_hook_args_parse() {
        use std::os::unix::fs::PermissionsExt;

        let pinned = hooks::InstallOptions {
            no_workspace: true,
            config_file: Some("ci.yaml".into()),
            root_dir: Some("..".into()),
            ..hooks::InstallOptions::default()
        };
        for opts in [hooks::InstallOptions::default(), pinned] {
            let dir = tempfile::tempdir().unwrap();
            let repo = git2::Repository::init(dir.path()).unwrap();
            // Overrides a global core.hooksPath, which would put the hook elsewhere
            repo.config()
                .unwrap()
                .set_str("core.hooksPath", ".git/hooks")
                .unwrap();
            hooks::install_with(&hooks::resolve(dir.path()).unwrap(), &opts).unwrap();

            // Run the hook with a `fnug` first on PATH that prints its arguments
            let shim = dir.path().join("bin/fnug");
            std::fs::create_dir(shim.parent().unwrap()).unwrap();
            std::fs::write(&shim, "#!/bin/sh\nprintf '%s\\n' \"$@\"\n").unwrap();
            std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
            let path = std::env::var("PATH").unwrap_or_default();
            let output = std::process::Command::new("sh")
                .arg(".git/hooks/pre-commit")
                .current_dir(dir.path())
                .env(
                    "PATH",
                    format!("{}:{path}", dir.path().join("bin").display()),
                )
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");

            let args = String::from_utf8(output.stdout).unwrap();
            let cli = Cli::try_parse_from(std::iter::once("fnug").chain(args.lines())).unwrap();
            assert_eq!(cli.no_workspace, opts.no_workspace);
            assert_eq!(cli.config.map(PathBuf::from), opts.config_file);
            assert_eq!(cli.root, opts.root_dir);
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
