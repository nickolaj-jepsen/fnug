use crate::commands::command::Command;
use crate::selectors::ignore::IgnoreFilter;
use crate::selectors::matching::command_matches;
use log::{debug, error, info};
use notify::{RecommendedWatcher, RecursiveMode};
use notify_debouncer_full::{DebounceEventResult, Debouncer, RecommendedCache, new_debouncer};
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

/// A command selected by file changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchMatch {
    pub id: String,
    /// Changed files matching the command's `auto` rules that still exist: absolute, sorted and
    /// deduplicated. Empty if only removals or directories matched.
    pub files: Vec<PathBuf>,
}

/// A running file watcher. Watching stops when it is dropped.
pub struct WatchHandle {
    /// Commands selected by file changes, a batch at a time, in config order.
    pub events: mpsc::Receiver<Vec<WatchMatch>>,
    pub report: WatchReport,
    _debouncer: Debouncer<RecommendedWatcher, RecommendedCache>,
}

/// A watch path and the commands watching it.
struct WatchKey {
    path: PathBuf,
    /// Indices into [`Matcher::commands`].
    commands: Vec<usize>,
    /// Git ignores the path itself. Ignore rules don't filter changes under it then, as
    /// watching it was asked for explicitly.
    ignored: bool,
}

/// Matches changed paths against the `auto` rules of the commands with `auto.watch`.
struct Matcher {
    commands: Vec<Command>,
    /// Sorted by path.
    keys: Vec<WatchKey>,
    ignore: IgnoreFilter,
}

impl Matcher {
    fn new(commands: Vec<Command>) -> Self {
        let commands: Vec<Command> = commands
            .into_iter()
            .filter(|cmd| cmd.auto.watch == Some(true))
            .collect();
        let mut keys: Vec<WatchKey> = Vec::new();
        for (index, cmd) in commands.iter().enumerate() {
            for path in cmd.auto.paths() {
                match keys.iter_mut().find(|key| &key.path == path) {
                    Some(key) if key.commands.contains(&index) => {}
                    Some(key) => key.commands.push(index),
                    None => keys.push(WatchKey {
                        path: path.clone(),
                        commands: vec![index],
                        ignored: false,
                    }),
                }
            }
        }
        keys.sort_by(|a, b| a.path.cmp(&b.path));
        let mut ignore = IgnoreFilter::new(keys.iter().map(|key| key.path.as_path()));
        for key in &mut keys {
            key.ignored = ignore.is_ignored(&key.path, key.path.is_dir());
        }
        Matcher {
            commands,
            keys,
            ignore,
        }
    }

    /// The commands that `changed` paths select, in config order. Changes inside a `.git`
    /// directory never count, nor do changes git ignores unless under an ignored watch path.
    fn matches(&mut self, changed: &[PathBuf]) -> Vec<WatchMatch> {
        if changed
            .iter()
            .any(|path| path.file_name().is_some_and(|name| name == ".gitignore"))
        {
            self.ignore.clear_cache();
        }
        let mut selected: Vec<Option<Vec<PathBuf>>> = vec![None; self.commands.len()];
        for path in changed {
            // Looked up once needed: whether the path is a directory (`None` once removed),
            // and whether git ignores it.
            let mut is_dir: Option<Option<bool>> = None;
            let mut ignored = None;
            for key in self.keys.iter().filter(|key| path.starts_with(&key.path)) {
                if in_git_dir(path, &key.path) {
                    continue;
                }
                let mut hits = key
                    .commands
                    .iter()
                    .filter(|&&index| command_matches(&self.commands[index], &key.path, path))
                    .peekable();
                if hits.peek().is_none() {
                    continue;
                }
                let is_dir = *is_dir
                    .get_or_insert_with(|| path.symlink_metadata().ok().map(|meta| meta.is_dir()));
                if !key.ignored
                    && *ignored
                        .get_or_insert_with(|| self.ignore.is_ignored(path, is_dir == Some(true)))
                {
                    continue;
                }
                for &index in hits {
                    let files = selected[index].get_or_insert_with(Vec::new);
                    if is_dir == Some(false) {
                        files.push(path.clone());
                    }
                }
            }
        }
        self.commands
            .iter()
            .zip(selected)
            .filter_map(|(cmd, files)| {
                let mut files = files?;
                files.sort();
                files.dedup();
                Some(WatchMatch {
                    id: cmd.id.clone(),
                    files,
                })
            })
            .collect()
    }
}

/// Whether `path` is inside a `.git` directory below the watch path `key`.
fn in_git_dir(path: &Path, key: &Path) -> bool {
    path.strip_prefix(key)
        .is_ok_and(|rel| rel.components().any(|part| part.as_os_str() == ".git"))
}

fn start_debouncer(
    mut matcher: Matcher,
    sender: mpsc::Sender<Vec<WatchMatch>>,
) -> Result<Debouncer<RecommendedWatcher, RecommendedCache>, notify::Error> {
    info!("Starting file watcher");
    // `timeout` is how long every event is held back, not a quiet period.
    new_debouncer(
        Duration::from_millis(500),
        Some(Duration::from_millis(100)),
        move |res: DebounceEventResult| match res {
            Ok(events) => {
                let mut changed: Vec<PathBuf> = events
                    .iter()
                    .filter(|event| {
                        event.event.kind.is_create()
                            || event.event.kind.is_modify()
                            || event.event.kind.is_remove()
                    })
                    .flat_map(|event| event.paths.iter().cloned())
                    .collect();
                changed.sort();
                changed.dedup();

                let selected = matcher.matches(&changed);
                if selected.is_empty() {
                    return;
                }
                let ids: Vec<&str> = selected.iter().map(|m| m.id.as_str()).collect();
                debug!("Watcher matched commands: {}", ids.join(", "));
                if sender.blocking_send(selected).is_err() {
                    debug!("Dropping watch events: nothing receives them");
                }
            }
            Err(errors) => {
                for e in errors {
                    error!("Watch error: {e}");
                }
            }
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
    let matcher = Matcher::new(commands);
    if matcher.keys.is_empty() {
        return Err(WatchError::NoWatchableCommands);
    }
    let paths: Vec<PathBuf> = matcher.keys.iter().map(|key| key.path.clone()).collect();
    let paths: Vec<&Path> = paths.iter().map(PathBuf::as_path).collect();

    let (sender, events) = mpsc::channel(100);
    let mut debouncer = start_debouncer(matcher, sender)?;
    let report = register(&mut debouncer, &paths);
    if report.roots.is_empty() {
        return Err(WatchError::NothingWatched(report));
    }
    Ok(WatchHandle {
        events,
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

    /// Ids of the `commands` that the `changed` paths (relative to [`ROOT`]) select.
    fn matched_ids(commands: Vec<Command>, changed: &[&str]) -> Vec<String> {
        let changed: Vec<PathBuf> = changed.iter().map(|path| abs(path)).collect();
        Matcher::new(commands)
            .matches(&changed)
            .into_iter()
            .map(|m| m.id)
            .collect()
    }

    #[test]
    fn matches_basic_match() {
        let cmd = create_test_command("test1", vec!["src"], vec![r".*\.rs$"]);
        assert_eq!(matched_ids(vec![cmd], &["src/main.rs"]), ["test1"]);
    }

    #[test]
    fn matches_no_match() {
        let cmd = create_test_command("test1", vec!["src"], vec![r".*\.rs$"]);
        assert!(matched_ids(vec![cmd], &["src/main.txt", "other/main.rs"]).is_empty());
    }

    #[test]
    fn matches_multiple_commands() {
        let cmd1 = create_test_command("test1", vec!["src"], vec![r".*\.rs$"]);
        let cmd2 = create_test_command("test2", vec!["src"], vec![r".*\.rs$"]);
        assert_eq!(
            matched_ids(vec![cmd1, cmd2], &["src/main.rs"]),
            ["test1", "test2"]
        );
    }

    #[test]
    fn matches_multiple_patterns() {
        let cmd = create_test_command("test1", vec!["src"], vec![r".*\.rs$", r".*\.toml$"]);
        let changed = ["src/main.rs", "src/Cargo.toml", "src/README.md"];
        assert_eq!(matched_ids(vec![cmd], &changed), ["test1"]);
    }

    #[test]
    fn matches_empty_regex_matches_all() {
        let cmd = create_test_command("test1", vec!["docs"], vec![]);
        assert_eq!(matched_ids(vec![cmd], &["docs/demo.tape"]), ["test1"]);
    }

    #[test]
    fn matches_regex_relative_to_cwd() {
        let anchored = create_test_command("anchored", vec!["."], vec![r"^src/.*\.rs$"]);
        let parent_name = create_test_command("parent", vec!["."], vec!["project"]);
        let changed = ["src/main.rs", "nested/src/lib.rs"];
        assert_eq!(
            matched_ids(vec![anchored, parent_name], &changed),
            ["anchored"]
        );
    }

    #[test]
    fn matches_follow_config_order_and_skip_unwatched() {
        let mut unwatched = create_test_command("unwatched", vec!["src"], vec![]);
        unwatched.auto.watch = Some(false);
        let second = create_test_command("second", vec!["src/b"], vec![]);
        let first = create_test_command("first", vec!["src", "src/b"], vec![]);
        assert_eq!(
            matched_ids(vec![unwatched, first, second], &["src/b/x.rs"]),
            ["first", "second"]
        );
    }
}
