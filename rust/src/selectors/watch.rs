use crate::commands::command::Command;
use crate::commands::group::CommandGroup;
use async_std::channel::{unbounded, Receiver, Sender};
use futures::StreamExt;
use log::{error, info};
use notify::{RecommendedWatcher, RecursiveMode};
use notify_debouncer_full::{new_debouncer, DebounceEventResult, Debouncer, RecommendedCache};
use pyo3::exceptions::PyStopAsyncIteration;
use pyo3::prelude::*;
use pyo3_async_runtimes;
use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::time::Duration;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum WatchError {
    #[error("Error running watch command: {0}")]
    Watch(#[from] notify::Error),
    #[error("IO error: {0}")]
    Io(#[from] io::Error),
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
                        .any(|re| re.is_match(path.to_str().unwrap()))
                    {
                        commands.push(cmd);
                    }
                }
            }
        }
    }
    commands
}

fn path_lookup_table(group: CommandGroup) -> HashMap<PathBuf, Vec<Command>> {
    group
        .all_commands()
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

#[cfg_attr(feature = "stub_gen", pyo3_stub_gen::derive::gen_stub_pyclass)]
#[pyclass(frozen)]
pub struct WatcherIterator {
    receiver: Receiver<Vec<PathBuf>>,
    #[allow(dead_code)]
    watcher: Debouncer<RecommendedWatcher, RecommendedCache>,
    commands: HashMap<PathBuf, Vec<Command>>,
}

#[cfg_attr(feature = "stub_gen", pyo3_stub_gen::derive::gen_stub_pymethods)]
#[pymethods]
impl WatcherIterator {
    fn __aiter__(slf: PyRef<Self>) -> PyRef<Self> {
        slf
    }

    fn __anext__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let mut receiver = self.receiver.clone();
        let commands = self.commands.clone();

        let promise = async move {
            loop {
                let changed_files = receiver
                    .next()
                    .await
                    .ok_or(PyStopAsyncIteration::new_err("The iterator is exhausted"))?;

                let commands = commands_for_paths(&changed_files, &commands);
                if !commands.is_empty() {
                    return Ok(commands.into_iter().cloned().collect::<Vec<_>>());
                }
            }
        };

        pyo3_async_runtimes::async_std::future_into_py(py, promise)
    }
}

fn run_notify_watcher(
    paths: Vec<&PathBuf>,
    sender: Sender<Vec<PathBuf>>,
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

                if !files.is_empty() {
                    if let Err(e) = sender.send_blocking(files) {
                        error!("Failed to send file changes: {}", e);
                    }
                }
            }
            Err(e) => error!("Watch error: {:?}", e),
        },
    )
    .map_err(WatchError::Watch)?;

    for path in paths {
        info!("Watching path: {:?}", path);
        notify_watcher
            .watch(path, RecursiveMode::Recursive)
            .map_err(|e| {
                error!("Failed to watch path {:?}: {}", path, e);
                WatchError::Watch(e)
            })?;
    }

    Ok(notify_watcher)
}

pub fn watch(group: CommandGroup) -> PyResult<WatcherIterator> {
    let (tx, rx) = unbounded();
    let commands = path_lookup_table(group);

    if commands.is_empty() {
        return Err(PyErr::new::<pyo3::exceptions::PyValueError, _>(
            "No watchable commands found in group",
        ));
    }

    let paths = commands.keys().collect::<Vec<&PathBuf>>();
    let watcher = run_notify_watcher(paths, tx).map_err(|e| {
        PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!("Failed to start watcher: {}", e))
    })?;

    Ok(WatcherIterator {
        watcher,
        receiver: rx,
        commands,
    })
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
            interactive: false,
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
