use crate::commands::command::Command;
use crate::selectors::ignore::IgnoreFilter;
use crate::selectors::matching::command_matches;
use log::{debug, error, info, warn};
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use parking_lot::Mutex;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::RecvTimeoutError;
use std::sync::{Arc, OnceLock, Weak};
use std::time::{Duration, Instant};
use thiserror::Error;
use tokio::sync::mpsc;

#[cfg(target_os = "linux")]
use {notify::EventKind, notify::event::ModifyKind, std::collections::HashSet};

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
    /// Watches registered. On Linux every directory gets its own; elsewhere a recursive watch
    /// counts once, however many directories it covers.
    pub watched_dirs: usize,
    /// Paths that could not be watched, with the reason. Nothing in such a directory is watched.
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

/// Where the event thread finds the watcher, to watch directories created later.
type WatcherSlot = Arc<OnceLock<Weak<Mutex<RecommendedWatcher>>>>;

/// How long events are gathered, from the first, before they are matched together. No change
/// waits longer than this.
const BATCH_WINDOW: Duration = Duration::from_millis(500);

/// A running file watcher. Watching stops when it is dropped.
pub struct WatchHandle {
    /// Commands selected by file changes, a batch at a time, in config order.
    pub events: mpsc::Receiver<Vec<WatchMatch>>,
    pub report: WatchReport,
    _watcher: Arc<Mutex<RecommendedWatcher>>,
}

/// A watch path and the commands watching it.
struct WatchKey {
    path: PathBuf,
    /// Indices into [`Matcher::commands`].
    commands: Vec<usize>,
    is_dir: bool,
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
                        is_dir: path.symlink_metadata().is_ok_and(|meta| meta.is_dir()),
                        ignored: false,
                    }),
                }
            }
        }
        keys.sort_by(|a, b| a.path.cmp(&b.path));
        let mut ignore = IgnoreFilter::new(keys.iter().map(|key| key.path.as_path()));
        for key in &mut keys {
            key.ignored = ignore.is_ignored(&key.path, key.is_dir);
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
            #[cfg(target_os = "linux")]
            info!("A .gitignore changed: restart to watch directories it no longer ignores");
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

    /// Whether ignore rules apply below `dir`, a directory created under a watch path, or
    /// `None` if it isn't to be watched: it's outside every watched directory, in `.git` or
    /// ignored.
    #[cfg(target_os = "linux")]
    fn new_dir_rules(&mut self, dir: &Path) -> Option<bool> {
        let mut apply_ignore = None;
        for key in self.keys.iter().filter(|key| key.is_dir) {
            if !dir.starts_with(&key.path) {
                continue;
            }
            if in_git_dir(dir, &key.path) {
                return None;
            }
            apply_ignore = Some(apply_ignore.unwrap_or(true) && !key.ignored);
        }
        match apply_ignore? {
            true if self.ignore.is_ignored(dir, true) => None,
            apply_ignore => Some(apply_ignore),
        }
    }
}

/// Whether `path` is inside a `.git` directory below the watch path `key`.
fn in_git_dir(path: &Path, key: &Path) -> bool {
    path.strip_prefix(key)
        .is_ok_and(|rel| rel.components().any(|part| part.as_os_str() == ".git"))
}

/// Start a file watcher, with nothing watched yet, whose events select commands through
/// `matcher` and go to `sender` in batches.
// Not notify-debouncer-full: it compares each new watch with all earlier ones, so a watch per
// directory took seconds to register in large trees.
fn start_watcher(
    mut matcher: Matcher,
    sender: mpsc::Sender<Vec<WatchMatch>>,
    slot: WatcherSlot,
) -> Result<RecommendedWatcher, notify::Error> {
    info!("Starting file watcher");
    let (raw_sender, raw) = std::sync::mpsc::channel();
    let watcher = notify::recommended_watcher(raw_sender)?;
    // Ends when the watcher is dropped, which closes the channel.
    std::thread::Builder::new()
        .name("fnug-watch".to_string())
        .spawn(move || {
            while let Some(batch) = next_batch(&raw) {
                handle_batch(&mut matcher, batch, &sender, &slot);
            }
        })
        .map_err(notify::Error::io)?;
    Ok(watcher)
}

/// The next event and those that follow within [`BATCH_WINDOW`] of it, or `None` once the
/// watcher is gone.
fn next_batch(
    raw: &std::sync::mpsc::Receiver<notify::Result<Event>>,
) -> Option<Vec<notify::Result<Event>>> {
    let mut batch = vec![raw.recv().ok()?];
    let deadline = Instant::now() + BATCH_WINDOW;
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        match raw.recv_timeout(left) {
            Ok(event) => batch.push(event),
            Err(RecvTimeoutError::Timeout) => break,
            Err(RecvTimeoutError::Disconnected) => return None,
        }
    }
    Some(batch)
}

/// Send the commands that a batch of events selects.
fn handle_batch(
    matcher: &mut Matcher,
    batch: Vec<notify::Result<Event>>,
    sender: &mpsc::Sender<Vec<WatchMatch>>,
    slot: &WatcherSlot,
) {
    let mut events = Vec::with_capacity(batch.len());
    let mut rescan = false;
    for event in batch {
        match event {
            Ok(event) if event.need_rescan() => rescan = true,
            Ok(event) => events.push(event),
            Err(e) => error!("Watch error: {e}"),
        }
    }
    if rescan {
        warn!("The system dropped file events, so some changes may have gone unnoticed");
    }
    let mut changed: Vec<PathBuf> = events
        .iter()
        .filter(|event| event.kind.is_create() || event.kind.is_modify() || event.kind.is_remove())
        .flat_map(|event| event.paths.iter().cloned())
        .collect();
    changed.extend(watch_new_dirs(matcher, &events, slot));
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

/// Watch the directories that `events` created or moved in under a watched directory, and
/// return the files already in them, which appeared before any watch could report them.
#[cfg(target_os = "linux")]
fn watch_new_dirs(matcher: &mut Matcher, events: &[Event], slot: &WatcherSlot) -> Vec<PathBuf> {
    let dirs: Vec<&PathBuf> = events
        .iter()
        .filter(|event| {
            matches!(
                event.kind,
                EventKind::Create(_) | EventKind::Modify(ModifyKind::Name(_))
            )
        })
        .flat_map(|event| &event.paths)
        .filter(|path| path.symlink_metadata().is_ok_and(|meta| meta.is_dir()))
        .collect();
    if dirs.is_empty() {
        return Vec::new();
    }
    let Some(watcher) = slot.get().and_then(Weak::upgrade) else {
        return Vec::new();
    };
    let mut watcher = watcher.lock();
    let mut watched_dirs = HashSet::new();
    let mut walk = Walk {
        files: Some(Vec::new()),
        ..Walk::default()
    };
    for dir in dirs {
        if let Some(apply_ignore) = matcher.new_dir_rules(dir) {
            let ignore = apply_ignore.then_some(&mut matcher.ignore);
            walk_tree(&mut watcher, dir, ignore, &mut watched_dirs, &mut walk);
        }
    }
    for (path, e) in &walk.failed {
        warn!("Could not watch {}: {e}", path.display());
    }
    walk.files.unwrap_or_default()
}

#[cfg(not(target_os = "linux"))]
fn watch_new_dirs(_: &mut Matcher, _: &[Event], _: &WatcherSlot) -> Vec<PathBuf> {
    Vec::new()
}

/// What watching a directory tree found.
#[cfg(target_os = "linux")]
#[derive(Default)]
struct Walk {
    /// Directories given a watch.
    dirs: usize,
    /// Files in the watched directories, if collected.
    files: Option<Vec<PathBuf>>,
    failed: Vec<(PathBuf, String)>,
    limit_reached: bool,
}

/// Watch `root` and the directories below it one by one, skipping `.git`, those in
/// `watched_dirs` and, with an `ignore` filter, those git ignores. A directory that can't be
/// watched or read is skipped with everything in it. Stops at the system's limit on watches.
#[cfg(target_os = "linux")]
fn walk_tree(
    watcher: &mut RecommendedWatcher,
    root: &Path,
    mut ignore: Option<&mut IgnoreFilter>,
    watched_dirs: &mut HashSet<PathBuf>,
    walk: &mut Walk,
) {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if watched_dirs.contains(&dir) {
            continue;
        }
        // A directory removed in the meantime is no failure.
        match watcher.watch(&dir, RecursiveMode::NonRecursive) {
            Ok(()) => walk.dirs += 1,
            Err(e) if matches!(e.kind, notify::ErrorKind::PathNotFound) => continue,
            Err(e) => {
                let limit_reached = matches!(e.kind, notify::ErrorKind::MaxFilesWatch);
                walk.failed.push((dir, e.set_paths(Vec::new()).to_string()));
                if limit_reached {
                    walk.limit_reached = true;
                    return;
                }
                continue;
            }
        }
        watched_dirs.insert(dir.clone());
        // Listed after the watch is in place, so no file can slip in between unreported.
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => {
                walk.failed.push((dir, e.to_string()));
                continue;
            }
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                if let Some(files) = &mut walk.files {
                    files.push(path);
                }
            } else if entry.file_name() != ".git"
                && !ignore
                    .as_deref_mut()
                    .is_some_and(|ignore| ignore.is_ignored(&path, true))
            {
                stack.push(path);
            }
        }
    }
}

/// The directories watched so far while registering, to tell whether a file's parent is.
#[cfg(target_os = "linux")]
struct Registered {
    ignore: IgnoreFilter,
    /// Each watched directory: on Linux every one is watched on its own.
    watched: HashSet<PathBuf>,
}

#[cfg(target_os = "linux")]
impl Registered {
    fn new(paths: &[(PathBuf, bool)]) -> Self {
        Registered {
            ignore: IgnoreFilter::new(paths.iter().map(|(path, _)| path.as_path())),
            watched: HashSet::new(),
        }
    }

    /// Watch the directory tree at `path`, applying ignore rules below it unless it is
    /// `ignored` itself.
    fn watch_dir(
        &mut self,
        watcher: &mut RecommendedWatcher,
        path: &Path,
        ignored: bool,
        report: &mut WatchReport,
    ) {
        let mut walk = Walk::default();
        let ignore = (!ignored).then_some(&mut self.ignore);
        walk_tree(watcher, path, ignore, &mut self.watched, &mut walk);
        report.watched_dirs += walk.dirs;
        report.failed.extend(walk.failed);
        report.limit_reached |= walk.limit_reached;
        if self.watched.contains(path) {
            debug!(
                "Watching {} ({} new directories)",
                path.display(),
                walk.dirs
            );
            report.roots.push(path.to_path_buf());
        }
    }

    fn covers(&self, dir: &Path) -> bool {
        self.watched.contains(dir)
    }

    fn add_parent(&mut self, dir: &Path) {
        self.watched.insert(dir.to_path_buf());
    }
}

/// The directories watched so far while registering, to tell whether a file's parent is.
#[cfg(not(target_os = "linux"))]
struct Registered {
    recursive: Vec<PathBuf>,
    parents: Vec<PathBuf>,
}

#[cfg(not(target_os = "linux"))]
impl Registered {
    fn new(_: &[(PathBuf, bool)]) -> Self {
        Registered {
            recursive: Vec::new(),
            parents: Vec::new(),
        }
    }

    /// Watch the directory tree at `path` recursively. `FSEvents` watches whole trees anyway,
    /// so ignored directories are only filtered out of events.
    fn watch_dir(
        &mut self,
        watcher: &mut RecommendedWatcher,
        path: &Path,
        _ignored: bool,
        report: &mut WatchReport,
    ) {
        if add_watch(watcher, path, path, RecursiveMode::Recursive, report) {
            self.recursive.push(path.to_path_buf());
        }
    }

    fn covers(&self, dir: &Path) -> bool {
        self.recursive.iter().any(|root| dir.starts_with(root))
            || self.parents.iter().any(|p| p == dir)
    }

    fn add_parent(&mut self, dir: &Path) {
        self.parents.push(dir.to_path_buf());
    }
}

/// Watch each of `paths`, given with whether git ignores it, on its own, so one that is
/// missing or fails leaves the others watched. A file is watched through its parent
/// directory, as saving it by renaming a new file over it would end a watch on the file itself.
fn register(watcher: &mut RecommendedWatcher, paths: &[(PathBuf, bool)]) -> WatchReport {
    let mut report = WatchReport::default();
    let mut registered = Registered::new(paths);
    let mut files: Vec<&Path> = Vec::new();
    for (path, ignored) in paths {
        match path.symlink_metadata() {
            Err(e) if e.kind() == io::ErrorKind::NotFound => report.missing.push(path.clone()),
            Ok(meta) if !meta.is_dir() => files.push(path),
            _ => registered.watch_dir(watcher, path, *ignored, &mut report),
        }
    }
    // After the directories: a file in a watched one needs no watch of its own, and a
    // non-recursive watch would turn off notify's recursion on a directory already watched.
    for file in files {
        let parent = file.parent().unwrap_or(file);
        if registered.covers(parent) {
            report.roots.push(file.to_path_buf());
        } else if add_watch(
            watcher,
            file,
            parent,
            RecursiveMode::NonRecursive,
            &mut report,
        ) {
            registered.add_parent(parent);
        }
    }
    report
}

/// Watch `dir` so changes to the watch path `path` are seen, and record the outcome in
/// `report`. Returns whether it worked.
fn add_watch(
    watcher: &mut RecommendedWatcher,
    path: &Path,
    dir: &Path,
    mode: RecursiveMode,
    report: &mut WatchReport,
) -> bool {
    match watcher.watch(dir, mode) {
        Ok(()) => {
            debug!("Watching {} for {}", dir.display(), path.display());
            report.roots.push(path.to_path_buf());
            report.watched_dirs += 1;
            true
        }
        Err(e) => {
            report.limit_reached |= matches!(e.kind, notify::ErrorKind::MaxFilesWatch);
            report.failed.push((path.to_path_buf(), e.to_string()));
            false
        }
    }
}

/// Start watching the `auto.path` entries of the commands with `auto.watch`. Paths that are
/// missing or can't be watched are left out and listed in the handle's report.
///
/// On Linux, directories git ignores are not watched at all, unless the watch path itself is
/// ignored; directories created later are watched as they appear.
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
    let paths: Vec<(PathBuf, bool)> = matcher
        .keys
        .iter()
        .map(|key| (key.path.clone(), key.ignored))
        .collect();

    let (sender, events) = mpsc::channel(100);
    let slot = WatcherSlot::default();
    let watcher = Arc::new(Mutex::new(start_watcher(
        matcher,
        sender,
        Arc::clone(&slot),
    )?));
    let _ = slot.set(Arc::downgrade(&watcher));
    let report = register(&mut watcher.lock(), &paths);
    if report.roots.is_empty() {
        return Err(WatchError::NothingWatched(report));
    }
    Ok(WatchHandle {
        events,
        report,
        _watcher: watcher,
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
