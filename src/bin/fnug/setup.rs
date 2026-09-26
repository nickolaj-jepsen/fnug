use std::path::PathBuf;
use std::process::ExitCode;

use clap::Args;
use log::warn;

use fnug::config_file::ConfigError;
use fnug::{LoadOptions, LoadedConfig};

#[derive(Args, Debug)]
pub struct SetupArgs {}

/// Run the interactive setup wizard, with the config if one loads.
///
/// # Errors
///
/// Returns an error if `--root` names a missing directory or setup fails.
pub fn run(
    _args: &SetupArgs,
    load_opts: &LoadOptions,
) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let (cwd, loaded) = setup_context(load_opts)?;
    fnug::setup::run(&cwd, loaded.as_ref(), load_opts)?;
    Ok(ExitCode::SUCCESS)
}

/// The directory to set up and the config, if one loads. Without a config it is `--root`, or the
/// working directory.
fn setup_context(
    load_opts: &LoadOptions,
) -> Result<(PathBuf, Option<LoadedConfig>), Box<dyn std::error::Error>> {
    match fnug::load(load_opts) {
        Ok(loaded) => return Ok((loaded.cwd.clone(), Some(loaded))),
        Err(e @ ConfigError::RootDirMissing { .. }) => return Err(e.into()),
        Err(ConfigError::ConfigNotFound(_)) => {}
        Err(e) => warn!("{e}; continuing without a config"),
    }
    let start = match &load_opts.start_dir {
        Some(dir) => dir.clone(),
        None => std::env::current_dir()?,
    };
    let dir = load_opts
        .root_dir
        .as_ref()
        .map_or_else(|| start.clone(), |root| start.join(root));
    Ok((dir, None))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn without_a_config_setup_uses_root() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        let opts = LoadOptions {
            start_dir: Some(dir.path().to_path_buf()),
            root_dir: Some("sub".into()),
            ..LoadOptions::default()
        };

        let (cwd, loaded) = setup_context(&opts).unwrap();
        assert!(loaded.is_none());
        assert_eq!(cwd, dir.path().join("sub"));
    }

    #[test]
    fn mcp_entry_args_parse() {
        use clap::Parser;

        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().canonicalize().unwrap();
        std::fs::create_dir_all(base.join("cfg")).unwrap();
        std::fs::create_dir_all(base.join("proj")).unwrap();
        std::fs::write(base.join("cfg/ci.yaml"), "name: ci\ncommands: []\n").unwrap();
        let pinned = LoadOptions {
            start_dir: Some(base.join("proj")),
            config: Some("../cfg/ci.yaml".into()),
            root_dir: Some(".".into()),
            no_workspace: true,
            ..LoadOptions::default()
        };
        let loaded = fnug::load(&pinned).unwrap();

        let args = fnug::setup::mcp_server_args(Some(&loaded), &pinned, &loaded.cwd);
        let cli =
            crate::Cli::try_parse_from(std::iter::once("fnug".to_string()).chain(args)).unwrap();
        assert!(matches!(cli.command, Some(crate::Commands::Mcp)));
        assert!(cli.no_workspace);

        // The editor starts it from the directory the entry is in
        let again = fnug::load(&LoadOptions {
            start_dir: Some(loaded.cwd.clone()),
            config: cli.config.map(PathBuf::from),
            root_dir: cli.root,
            no_workspace: cli.no_workspace,
            ..LoadOptions::default()
        })
        .unwrap();
        assert_eq!(
            (again.config_path, again.cwd),
            (loaded.config_path, loaded.cwd)
        );
    }

    #[test]
    fn missing_root_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let opts = LoadOptions {
            start_dir: Some(dir.path().to_path_buf()),
            root_dir: Some("missing".into()),
            ..LoadOptions::default()
        };
        assert!(setup_context(&opts).is_err());
    }
}
