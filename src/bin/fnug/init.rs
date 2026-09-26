use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Args;

use fnug::LoadOptions;
use fnug::init::{InitError, InitOptions};

#[derive(Args, Debug)]
pub struct InitArgs {
    /// Directory to create the config in [default: --root, -c's directory, or the working
    /// directory]
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
/// Returns an error if DIR, `--root` and `-c` contradict each other, the config already exists
/// (without `--force`), the prompt fails, or the config can't be written.
pub fn run(
    args: &InitArgs,
    load_opts: &LoadOptions,
) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let opts = options(args, load_opts)?;
    let path = fnug::init::run(&opts, fnug::init::prompt).map_err(|e| match e {
        InitError::Exists(_) => format!(
            "{e}; run `{}` to replace it",
            force_command(args, load_opts)
        )
        .into(),
        e => Box::<dyn std::error::Error>::from(e),
    })?;
    println!("Created {}", path.display());
    println!(
        "Run `fnug` to open the TUI, `fnug check` to check your changes, or `fnug setup` to add a pre-commit hook and editor integration."
    );
    Ok(ExitCode::SUCCESS)
}

/// The project directory is DIR, or else [`crate::new_config_dir`] with `--root`, or else the
/// directory of the file `-c` names, which is the file to write. Relative paths resolve against
/// [`crate::start_dir`].
fn options(
    args: &InitArgs,
    load_opts: &LoadOptions,
) -> Result<InitOptions, Box<dyn std::error::Error>> {
    let start = crate::start_dir(load_opts)?;
    let file = load_opts.config.as_ref().map(|file| start.join(file));
    let dir = match (&args.dir, &load_opts.root_dir) {
        (Some(dir), Some(_)) => {
            let (dir, root) = (start.join(dir), crate::new_config_dir(load_opts)?);
            if !same_dir(&dir, &root) {
                return Err(format!(
                    "DIR ({}) and --root ({}) are different directories; pass one of them",
                    dir.display(),
                    root.display()
                )
                .into());
            }
            dir
        }
        (Some(dir), None) => start.join(dir),
        (None, Some(_)) => crate::new_config_dir(load_opts)?,
        (None, None) => file
            .as_deref()
            .and_then(Path::parent)
            .map_or(start, Path::to_path_buf),
    };
    // Loaded without --root, the file's paths resolve against its own directory
    if let Some(file) = &file
        && load_opts.root_dir.is_none()
        && !file.parent().is_some_and(|parent| same_dir(parent, &dir))
    {
        return Err(format!(
            "{} would run its commands in its own directory, not {}; name a file in {}, or \
             pass --root {} to run them there",
            file.display(),
            dir.display(),
            dir.display(),
            dir.display()
        )
        .into());
    }
    Ok(InitOptions {
        dir,
        file,
        force: args.force,
        yes: args.yes || !std::io::stdin().is_terminal(),
    })
}

/// The `fnug init` command line that was run, with `--force`.
fn force_command(args: &InitArgs, load_opts: &LoadOptions) -> String {
    let mut words = vec!["fnug".to_string()];
    for (flag, value) in [("-c", &load_opts.config), ("--root", &load_opts.root_dir)] {
        if let Some(value) = value {
            words.extend([flag.to_string(), shell_word(value)]);
        }
    }
    words.extend(["init", "--force"].map(String::from));
    if args.yes {
        words.push("--yes".into());
    }
    words.extend(args.dir.as_deref().map(shell_word));
    words.join(" ")
}

/// `path` as a shell word: as it is when that's safe, else single-quoted.
fn shell_word(path: &Path) -> String {
    let path = path.to_string_lossy();
    let plain = |c: char| c.is_ascii_alphanumeric() || "_-./+:@%,".contains(c);
    if !path.is_empty() && path.chars().all(plain) {
        path.into_owned()
    } else {
        format!("'{}'", path.replace('\'', r"'\''"))
    }
}

/// Whether `a` and `b` are the same directory, going by their canonical paths when they exist.
fn same_dir(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    fn init_args(args: &[&str]) -> InitArgs {
        let cli = crate::Cli::try_parse_from(std::iter::once("fnug").chain(args.iter().copied()))
            .unwrap();
        match cli.command {
            Some(crate::Commands::Init(args)) => args,
            _ => panic!("not init"),
        }
    }

    #[test]
    fn init_and_setup_create_in_the_same_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        for root in [None, Some("sub")] {
            let load = LoadOptions {
                start_dir: Some(dir.path().to_path_buf()),
                root_dir: root.map(PathBuf::from),
                ..LoadOptions::default()
            };
            let opts = options(&init_args(&["init"]), &load).unwrap();
            let (setup_dir, _) = crate::setup::setup_context(&load).unwrap();
            assert_eq!(opts.dir, setup_dir, "{root:?}");
        }
    }

    #[test]
    fn force_command_repeats_the_invocation() {
        let load = LoadOptions {
            config: Some("my ci.yaml".into()),
            root_dir: Some("app".into()),
            ..LoadOptions::default()
        };
        let args = init_args(&["init", "-y", "app"]);
        assert_eq!(
            force_command(&args, &load),
            "fnug -c 'my ci.yaml' --root app init --force --yes app"
        );
        let plain = force_command(&init_args(&["init"]), &LoadOptions::default());
        assert_eq!(plain, "fnug init --force");
        assert_eq!(shell_word(Path::new("it's")), r"'it'\''s'");
    }

    #[test]
    fn config_file_is_written_where_it_loads_from() {
        let dir = tempfile::tempdir().unwrap();
        let load = LoadOptions {
            start_dir: Some(dir.path().to_path_buf()),
            config: Some("ci.yaml".into()),
            ..LoadOptions::default()
        };
        let opts = options(&init_args(&["init"]), &load).unwrap();
        assert_eq!(opts.dir, dir.path());
        assert_eq!(opts.file, Some(dir.path().join("ci.yaml")));
    }
}
