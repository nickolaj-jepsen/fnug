use crate::commands::command::Command;
use log::{debug, error, info};
use notify::{RecommendedWatcher, RecursiveMode};
use notify_debouncer_full::{DebounceEventResult, Debouncer, RecommendedCache, new_debouncer};
use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;
use thiserror::Error;
use tokio::sync::mpsc;

#[derive(Error, Debug)]
pub enum WatchError {
    #[error("Error running watch command: {0}")]
    Watch(#[from] notify::Error),
    #[error("IO error: {0}")]
    Io(#[from] io::Error),
    #[error("No watchable commands found")]
    NoWatchableCommands,
}

fn commands_for_paths<'a>(
    paths: &[PathBuf],
    path_map: &'a HashMap<PathBuf, Vec<Command>>,
) -> Vec<&'a Command> {
    let mut commands = Vec::new();
    for path in paths {
        for (key, value) in path_map {
            if path.starts_with(key) {
                for cmd in value {
                    if cmd
                        .auto
                        .regex
                        .iter()
                        .any(|re| re.is_match(&path.to_string_lossy()))
                    {
                        commands.push(cmd);
                    }
                }
            }
        }
    }
    commands
}

fn path_lookup_table(commands: Vec<Command>) -> HashMap<PathBuf, Vec<Command>> {
    commands
        .into_iter()
        .filter(|cmd| cmd.auto.watch.unwrap_or(false))
        .flat_map(|cmd| {
            cmd.auto
                .path
                .clone()
                .into_iter()
                .map(move |p| (p, cmd.clone()))
        })
        .fold(HashMap::new(), |mut acc, (path, cmd)| {
            acc.entry(path).or_default().push(cmd);
            acc
        })
}

fn run_notify_watcher(
    paths: Vec<&Path>,
    sender: mpsc::Sender<Vec<PathBuf>>,
) -> Result<Debouncer<RecommendedWatcher, RecommendedCache>, WatchError> {
    info!("Starting file watcher");
    let mut notify_watcher = new_debouncer(
        Duration::from_secs(5),
        Some(Duration::from_millis(500)),
        move |res: DebounceEventResult| match res {
            Ok(events) => {
                let files: Vec<PathBuf> = events
                    .iter()
                    .filter(|event| {
                        event.event.kind.is_create()
                            || event.event.kind.is_modify()
                            || event.event.kind.is_remove()
                    })
                    .flat_map(|event| event.paths.clone())
                    .collect();

                if !files.is_empty()
                    && let Err(e) = sender.blocking_send(files)
                {
                    error!("Failed to send watch event: {e}");
                }
            }
            Err(e) => error!("Watch error: {e:?}"),
        },
    )
    .map_err(WatchError::Watch)?;

    for path in paths {
        info!("Watching path: {}", path.display());
        notify_watcher
            .watch(path, RecursiveMode::Recursive)
            .map_err(|e| {
                error!("Failed to watch path {}: {e}", path.display());
                WatchError::Watch(e)
            })?;
    }

    Ok(notify_watcher)
}

type WatchHandle = (
    mpsc::Receiver<Vec<Command>>,
    Debouncer<RecommendedWatcher, RecommendedCache>,
);

/// Start watching for file changes, returning a receiver of matched commands and the debouncer (must be kept alive).
///
/// # Errors
///
/// Returns `WatchError::NoWatchableCommands` if no commands have watch enabled,
/// or `WatchError::Watch` if the file watcher fails to start.
pub fn watch_commands(commands: Vec<Command>) -> Result<WatchHandle, WatchError> {
    let (path_tx, mut path_rx) = mpsc::channel(100);
    let lookup_table = path_lookup_table(commands);

    if lookup_table.is_empty() {
        return Err(WatchError::NoWatchableCommands);
    }

    let paths = lookup_table
        .keys()
        .map(PathBuf::as_path)
        .collect::<Vec<&Path>>();
    let watcher = run_notify_watcher(paths, path_tx)?;

    let (cmd_tx, cmd_rx) = mpsc::channel(100);
    let lookup = lookup_table;

    tokio::spawn(async move {
        while let Some(changed_files) = path_rx.recv().await {
            let matched = commands_for_paths(&changed_files, &lookup);
            if !matched.is_empty() {
                let names: Vec<&str> = matched.iter().map(|c| c.name.as_str()).collect();
                debug!("Watcher matched commands: {}", names.join(", "));
                let cmds: Vec<Command> = matched.into_iter().cloned().collect();
                if cmd_tx.send(cmds).await.is_err() {
                    break;
                }
            }
        }
    });

    Ok((cmd_rx, watcher))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::auto::Auto;
    use regex_cache::LazyRegex;

    fn create_test_command(name: &str, paths: Vec<&str>, patterns: Vec<&str>) -> Command {
        Command {
            id: name.to_string(),
            name: name.to_string(),
            cmd: "test".to_string(),
            cwd: PathBuf::new(),

            auto: Auto {
                watch: Some(true),
                path: paths.into_iter().map(PathBuf::from).collect(),
                regex: patterns
                    .into_iter()
                    .map(|p| LazyRegex::new(p).unwrap())
                    .collect(),
                git: None,
                always: None,
            },
            ..Default::default()
        }
    }

    fn create_path_map(commands: Vec<Command>) -> HashMap<PathBuf, Vec<Command>> {
        let mut map: HashMap<PathBuf, Vec<Command>> = HashMap::new();
        for cmd in commands {
            for path in &cmd.auto.path {
                map.entry(path.clone()).or_default().push(cmd.clone());
            }
        }
        map
    }

    #[test]
    fn test_commands_for_paths_basic_match() {
        let cmd = create_test_command("test1", vec!["src"], vec![r".*\.rs$"]);
        let path_map = create_path_map(vec![cmd]);

        let changed_paths = vec![PathBuf::from("src/main.rs")];
        let matching_commands = commands_for_paths(&changed_paths, &path_map);

        assert_eq!(matching_commands.len(), 1);
        assert_eq!(matching_commands[0].name, "test1");
    }

    #[test]
    fn test_commands_for_paths_no_match() {
        let cmd = create_test_command("test1", vec!["src"], vec![r".*\.rs$"]);
        let path_map = create_path_map(vec![cmd]);

        let changed_paths = vec![PathBuf::from("src/main.txt")];
        let matching_commands = commands_for_paths(&changed_paths, &path_map);

        assert_eq!(matching_commands.len(), 0);
    }

    #[test]
    fn test_commands_for_paths_multiple_commands() {
        let cmd1 = create_test_command("test1", vec!["src"], vec![r".*\.rs$"]);
        let cmd2 = create_test_command("test2", vec!["src"], vec![r".*\.rs$"]);
        let path_map = create_path_map(vec![cmd1, cmd2]);

        let changed_paths = vec![PathBuf::from("src/main.rs")];
        let matching_commands = commands_for_paths(&changed_paths, &path_map);

        assert_eq!(matching_commands.len(), 2);
    }

    #[test]
    fn test_commands_for_paths_multiple_patterns() {
        let cmd = create_test_command("test1", vec!["src"], vec![r".*\.rs$", r".*\.toml$"]);
        let path_map = create_path_map(vec![cmd]);

        let changed_paths = vec![
            PathBuf::from("src/main.rs"),
            PathBuf::from("src/Cargo.toml"),
            PathBuf::from("src/README.md"),
        ];
        let matching_commands = commands_for_paths(&changed_paths, &path_map);

        assert_eq!(matching_commands.len(), 2);
    }
}
