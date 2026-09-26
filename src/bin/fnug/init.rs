use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Args;
use inquire::MultiSelect;

use fnug::init::{InitError, InitOptions, Proposal};

#[derive(Args, Debug)]
pub struct InitArgs {
    /// Directory to create the config in [default: the working directory]
    dir: Option<PathBuf>,

    /// Replace the directory's config if it has one
    #[arg(long)]
    force: bool,

    /// Include everything detected without asking; the default when stdin isn't a terminal
    #[arg(short, long)]
    yes: bool,
}

/// Create a config for the project in `args.dir`, asking which detected groups to include.
///
/// # Errors
///
/// Returns an error if the directory already has a config (without `--force`), the prompt
/// fails, or the config can't be written.
pub fn run(args: &InitArgs) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let opts = InitOptions {
        dir: args.dir.clone().unwrap_or_else(|| PathBuf::from(".")),
        force: args.force,
        yes: args.yes || !std::io::stdin().is_terminal(),
    };
    let path = fnug::init::run(&opts, choose)?;
    println!("Created {}", path.display());
    println!(
        "Run `fnug` to open the TUI, `fnug check` to check your changes, or `fnug setup` to add a pre-commit hook and editor integration."
    );
    Ok(ExitCode::SUCCESS)
}

fn choose(proposals: &[Proposal]) -> Result<Vec<usize>, InitError> {
    let chosen = MultiSelect::new(
        "Which commands should the config include?",
        proposals.iter().collect(),
    )
    .with_all_selected_by_default()
    .raw_prompt()?;
    Ok(chosen.into_iter().map(|option| option.index).collect())
}
