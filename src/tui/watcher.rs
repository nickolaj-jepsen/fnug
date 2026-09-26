//! Background watchers the TUI owns: the `auto.watch` file watcher, and one on the config
//! files that asks for a reload when they change.

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use log::{debug, info, warn};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use parking_lot::Mutex;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::commands::command::Command;
use crate::selectors::watch::{WatchError, WatchReport, watch_commands};

use super::app::AppEvent;
use super::status::{StatusLevel, watch_problems};

/// How long config changes are gathered before they cause one reload: editors often save a
/// file in several writes
const CONFIG_DEBOUNCE: Duration = Duration::from_millis(300);

/// Start watching the `auto.watch` paths of `commands`, forwarding matches as
/// [`AppEvent::WatcherTriggered`]. Aborting the task stops the watching.
///
/// Registering the watches runs on a blocking thread, as it takes long for big trees. A
/// warning about the watches is posted as a status unless it is `last_warning`, the previous
/// watcher's, so a config reload doesn't repeat it over the reload's own message.
pub(super) fn start_file_watcher(
    commands: Vec<Command>,
    event_tx: mpsc::Sender<AppEvent>,
    last_warning: Arc<Mutex<Option<String>>>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let result = tokio::task::spawn_blocking(move || watch_commands(commands)).await;

        let post_warning = async |warning: Option<String>| {
            if let Some(text) = new_warning(&last_warning, warning) {
                let level = StatusLevel::Warn;
                let _ = event_tx.send(AppEvent::Status { text, level }).await;
            }
        };
        let mut handle = match result {
            Ok(Ok(handle)) => {
                log_watch_report(&handle.report);
                post_warning(watch_problems(&handle.report)).await;
                handle
            }
            Ok(Err(WatchError::NoWatchableCommands)) => {
                debug!("File watcher not started: no command uses auto.watch");
                post_warning(None).await;
                return;
            }
            Ok(Err(e)) => {
                if let WatchError::NothingWatched(report) = &e {
                    log_watch_report(report);
                }
                warn!("File watcher not started: {e}");
                post_warning(Some(format!("File watcher not started: {e}"))).await;
                return;
            }
            Err(e) => {
                warn!("File watcher task failed: {e}");
                return;
            }
        };

        // The handle keeps watching while this task lives
        while let Some(matches) = handle.events.recv().await {
            if event_tx
                .send(AppEvent::WatcherTriggered(matches))
                .await
                .is_err()
            {
                break;
            }
        }
    })
}

/// Record `warning` as the watcher's current one in `last`. Returns it unless it was
/// already the current one.
fn new_warning(last: &Mutex<Option<String>>, warning: Option<String>) -> Option<String> {
    let mut last = last.lock();
    if *last == warning {
        return None;
    }
    last.clone_from(&warning);
    warning
}

fn log_watch_report(report: &WatchReport) {
    for path in &report.missing {
        warn!("Not watching {}: it does not exist", path.display());
    }
    for (path, error) in &report.failed {
        warn!("Could not watch {}: {error}", path.display());
    }
    if report.limit_reached {
        warn!(
            "Ran out of file watches, so some directories are not watched; \
             on Linux, raise fs.inotify.max_user_watches"
        );
    }
    if !report.roots.is_empty() {
        info!(
            "File watcher started: {} paths, {} directories",
            report.roots.len(),
            report.watched_dirs
        );
    }
}

/// Watches the config files until dropped.
pub(super) struct ConfigWatcher {
    _watcher: RecommendedWatcher,
    task: JoinHandle<()>,
}

impl Drop for ConfigWatcher {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// `file` as the watcher reports it: its directory resolved, as event paths are, and its name
fn watched_path(file: &Path) -> Option<(PathBuf, PathBuf)> {
    let dir = file.parent()?.canonicalize().ok()?;
    let path = dir.join(file.file_name()?);
    Some((dir, path))
}

/// Send [`AppEvent::ConfigFileChanged`] once changes to any of `files` settle.
///
/// Watches their directories rather than the files, so a file an editor replaces on save
/// stays watched.
///
/// # Errors
///
/// Returns the watcher's error if it can't start or watch a directory.
pub(super) fn watch_config_files(
    files: &[PathBuf],
    event_tx: mpsc::Sender<AppEvent>,
) -> Result<ConfigWatcher, notify::Error> {
    let (dirs, paths): (BTreeSet<PathBuf>, HashSet<PathBuf>) =
        files.iter().filter_map(|f| watched_path(f)).unzip();
    let (changed_tx, mut changed_rx) = mpsc::unbounded_channel();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let Ok(event) = event else { return };
        if !matches!(event.kind, EventKind::Access(_))
            && event.paths.iter().any(|p| paths.contains(p))
        {
            let _ = changed_tx.send(());
        }
    })?;
    for dir in &dirs {
        watcher.watch(dir, RecursiveMode::NonRecursive)?;
    }
    let task = tokio::spawn(async move {
        while changed_rx.recv().await.is_some() {
            tokio::time::sleep(CONFIG_DEBOUNCE).await;
            while changed_rx.try_recv().is_ok() {}
            if event_tx.send(AppEvent::ConfigFileChanged).await.is_err() {
                break;
            }
        }
    });
    Ok(ConfigWatcher {
        _watcher: watcher,
        task,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn config_change_asks_for_reload() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join(".fnug.yaml");
        std::fs::write(&config, "name: a\n").unwrap();
        let (tx, mut rx) = mpsc::channel(10);
        let _watcher = watch_config_files(std::slice::from_ref(&config), tx).unwrap();

        // Replaced, as editors that save atomically do
        let temp = dir.path().join(".fnug.yaml.tmp");
        std::fs::write(&temp, "name: b\n").unwrap();
        std::fs::rename(&temp, &config).unwrap();

        let event = tokio::time::timeout(Duration::from_secs(5), rx.recv()).await;
        let event = event.expect("no reload asked for").unwrap();
        assert!(matches!(event, AppEvent::ConfigFileChanged));
    }
}
