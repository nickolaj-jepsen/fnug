use crate::commands::command::Command;
use crate::selectors::matching::command_matches;
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
#[non_exhaustive]
pub enum WatchError {
    #[error("the file watcher could not start: {0}")]
    Watch(#[from] notify::Error),
    #[error("No watchable commands found")]
    NoWatchableCommands,
    /// Every watch path was missing or failed to register.
    #[error("none of the watch paths could be watched")]
    NothingWatched(WatchReport),
}

/// What [`watch_commands`] managed to watch.
#[derive(Debug, Default)]
pub struct WatchReport {
    /// Watch paths that are being watched.
    pub roots: Vec<PathBuf>,
    /// Watches registered; a recursive watch counts once, however many directories it covers.
    pub watched_dirs: usize,
    /// Paths that could not be watched, or only partly, with the reason.
    pub failed: Vec<(PathBuf, String)>,
    /// Watch paths that don't exist, so they are not watched.
    pub missing: Vec<PathBuf>,
    /// The system's limit on file watches ran out, leaving some directories unwatched.
    pub limit_reached: bool,
}

/// A running file watcher. Watching stops when it is dropped.
pub struct WatchHandle {
    /// Batches of commands selected by file changes.
    pub events: mpsc::Receiver<Vec<Command>>,
    pub report: WatchReport,
    _debouncer: Debouncer<RecommendedWatcher, RecommendedCache>,
}

fn commands_for_paths<'a>(
    paths: &[PathBuf],
    path_map: &'a HashMap<PathBuf, Vec<Command>>,
) -> Vec<&'a Command> {
    let mut seen = std::collections::HashSet::new();
    paths
        .iter()
        .flat_map(|path| {
            path_map.iter().flat_map(move |(key, cmds)| {
                cmds.iter()
                    .filter(move |cmd| command_matches(cmd, key, path))
            })
        })
        .filter(|cmd| seen.insert(cmd.id.clone()))
        .collect()
}

fn path_lookup_table(commands: Vec<Command>) -> HashMap<PathBuf, Vec<Command>> {
    commands
        .into_iter()
        .filter(|cmd| cmd.auto.watch.unwrap_or(false))
        .flat_map(|cmd| {
            cmd.auto
                .paths()
                .to_vec()
                .into_iter()
                .map(move |p| (p, cmd.clone()))
        })
        .fold(HashMap::new(), |mut acc, (path, cmd)| {
            acc.entry(path).or_default().push(cmd);
            acc
        })
}

fn start_debouncer(
    sender: mpsc::Sender<Vec<PathBuf>>,
) -> Result<Debouncer<RecommendedWatcher, RecommendedCache>, notify::Error> {
    info!("Starting file watcher");
    // `timeout` is how long every event is held back, not a quiet period.
    new_debouncer(
        Duration::from_millis(500),
        Some(Duration::from_millis(100)),
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
}

/// Watch each of `paths` on its own, so one that is missing or fails leaves the others watched.
fn register(
    debouncer: &mut Debouncer<RecommendedWatcher, RecommendedCache>,
    paths: &[&Path],
) -> WatchReport {
    let mut report = WatchReport::default();
    for &path in paths {
        if matches!(path.symlink_metadata(), Err(e) if e.kind() == io::ErrorKind::NotFound) {
            report.missing.push(path.to_path_buf());
            continue;
        }
        match debouncer.watch(path, RecursiveMode::Recursive) {
            Ok(()) => {
                debug!("Watching path: {}", path.display());
                report.roots.push(path.to_path_buf());
                report.watched_dirs += 1;
            }
            // A recursive watch stops at the first directory that fails, leaving the rest of
            // the tree unwatched, so the path counts as failed even if some of it is watched.
            Err(e) => {
                report.limit_reached |= matches!(e.kind, notify::ErrorKind::MaxFilesWatch);
                report.failed.push((path.to_path_buf(), e.to_string()));
            }
        }
    }
    report
}

/// Start watching the `auto.path` entries of the commands with `auto.watch`. Paths that are
/// missing or can't be watched are left out and listed in the handle's report.
///
/// # Errors
///
/// Returns `WatchError::NoWatchableCommands` if no command has `auto.watch`,
/// `WatchError::NothingWatched` if none of their paths could be watched, or
/// `WatchError::Watch` if the file watcher fails to start.
pub fn watch_commands(commands: Vec<Command>) -> Result<WatchHandle, WatchError> {
    let (path_tx, mut path_rx) = mpsc::channel(100);
    let lookup_table = path_lookup_table(commands);

    if lookup_table.is_empty() {
        return Err(WatchError::NoWatchableCommands);
    }

    let mut paths = lookup_table
        .keys()
        .map(PathBuf::as_path)
        .collect::<Vec<&Path>>();
    paths.sort();
    let mut debouncer = start_debouncer(path_tx)?;
    let report = register(&mut debouncer, &paths);
    if report.roots.is_empty() {
        return Err(WatchError::NothingWatched(report));
    }

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

    Ok(WatchHandle {
        events: cmd_rx,
        report,
        _debouncer: debouncer,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::auto::Auto;

    /// Like a loaded config: absolute cwd and paths, as notify reports absolute event paths.
    const ROOT: &str = "/project";

    fn abs(path: &str) -> PathBuf {
        Path::new(ROOT).join(path)
    }

    fn create_test_command(name: &str, paths: Vec<&str>, patterns: Vec<&str>) -> Command {
        Command {
            id: name.to_string(),
            name: name.to_string(),
            cmd: "test".to_string(),
            cwd: PathBuf::from(ROOT),

            auto: Auto::create(
                Some(true),
                None,
                paths.into_iter().map(abs).collect(),
                patterns.into_iter().map(String::from).collect(),
                None,
                None,
            )
            .unwrap(),
            ..Default::default()
        }
    }

    fn create_path_map(commands: Vec<Command>) -> HashMap<PathBuf, Vec<Command>> {
        let mut map: HashMap<PathBuf, Vec<Command>> = HashMap::new();
        for cmd in commands {
            for path in cmd.auto.paths() {
                map.entry(path.clone()).or_default().push(cmd.clone());
            }
        }
        map
    }

    #[test]
    fn test_commands_for_paths_basic_match() {
        let cmd = create_test_command("test1", vec!["src"], vec![r".*\.rs$"]);
        let path_map = create_path_map(vec![cmd]);

        let changed_paths = vec![abs("src/main.rs")];
        let matching_commands = commands_for_paths(&changed_paths, &path_map);

        assert_eq!(matching_commands.len(), 1);
        assert_eq!(matching_commands[0].name, "test1");
    }

    #[test]
    fn test_commands_for_paths_no_match() {
        let cmd = create_test_command("test1", vec!["src"], vec![r".*\.rs$"]);
        let path_map = create_path_map(vec![cmd]);

        let changed_paths = vec![abs("src/main.txt"), abs("other/main.rs")];
        let matching_commands = commands_for_paths(&changed_paths, &path_map);

        assert_eq!(matching_commands.len(), 0);
    }

    #[test]
    fn test_commands_for_paths_multiple_commands() {
        let cmd1 = create_test_command("test1", vec!["src"], vec![r".*\.rs$"]);
        let cmd2 = create_test_command("test2", vec!["src"], vec![r".*\.rs$"]);
        let path_map = create_path_map(vec![cmd1, cmd2]);

        let changed_paths = vec![abs("src/main.rs")];
        let matching_commands = commands_for_paths(&changed_paths, &path_map);

        assert_eq!(matching_commands.len(), 2);
    }

    #[test]
    fn test_commands_for_paths_multiple_patterns() {
        let cmd = create_test_command("test1", vec!["src"], vec![r".*\.rs$", r".*\.toml$"]);
        let path_map = create_path_map(vec![cmd]);

        let changed_paths = vec![
            abs("src/main.rs"),
            abs("src/Cargo.toml"),
            abs("src/README.md"),
        ];
        let matching_commands = commands_for_paths(&changed_paths, &path_map);

        // Same command matches both .rs and .toml, but should be deduplicated
        assert_eq!(matching_commands.len(), 1);
        assert_eq!(matching_commands[0].name, "test1");
    }

    #[test]
    fn test_commands_for_paths_empty_regex_matches_all() {
        let cmd = create_test_command("test1", vec!["docs"], vec![]);
        let path_map = create_path_map(vec![cmd]);

        let changed_paths = vec![abs("docs/demo.tape")];
        let matching_commands = commands_for_paths(&changed_paths, &path_map);

        assert_eq!(matching_commands.len(), 1);
        assert_eq!(matching_commands[0].name, "test1");
    }

    #[test]
    fn test_commands_for_paths_regex_relative_to_cwd() {
        let anchored = create_test_command("anchored", vec!["."], vec![r"^src/.*\.rs$"]);
        let parent_name = create_test_command("parent", vec!["."], vec!["project"]);
        let path_map = create_path_map(vec![anchored, parent_name]);

        let changed_paths = vec![abs("src/main.rs"), abs("nested/src/lib.rs")];
        let matching_commands = commands_for_paths(&changed_paths, &path_map);

        assert_eq!(matching_commands.len(), 1);
        assert_eq!(matching_commands[0].name, "anchored");
    }
}
