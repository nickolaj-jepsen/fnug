//! Holding back what file changes set off while a `fnug check --stash` has checked the index
//! out over the work tree.

use log::info;

use crate::check::stash;
use crate::selectors::watch::WatchMatch;

use super::app::App;
use super::status::StatusLevel;

/// What file changes set off while a `fnug check --stash` ran, to do once it has ended
#[derive(Debug, Default)]
pub(super) struct HeldBack {
    reload: bool,
    changes: Vec<WatchMatch>,
}

/// `matches` with one entry per command, in the order they came, naming each file once
fn merge(matches: impl IntoIterator<Item = WatchMatch>) -> Vec<WatchMatch> {
    let mut merged: Vec<WatchMatch> = Vec::new();
    for m in matches {
        let Some(entry) = merged.iter_mut().find(|e| e.id == m.id) else {
            merged.push(m);
            continue;
        };
        for file in m.files {
            if !entry.files.contains(&file) {
                entry.files.push(file);
            }
        }
    }
    merged
}

impl App {
    /// Whether a `fnug check --stash` holds the work tree.
    ///
    /// It checks the index out over the work tree and puts the unstaged changes back
    /// afterwards, and neither is an edit of the user's: running commands on the index's
    /// content could fail the check, and reloading it could stop commands.
    pub(super) fn stash_running(&mut self) -> bool {
        // Locating runs git, which is too slow for every event and tick
        if self
            .stash_lock
            .as_ref()
            .is_none_or(|(cwd, _)| *cwd != self.cwd)
        {
            self.stash_lock = Some((self.cwd.clone(), stash::lock_path(&self.cwd)));
        }
        self.stash_lock
            .as_ref()
            .and_then(|(_, lock)| lock.as_deref())
            .is_some_and(stash::held)
    }

    /// Hold back a config reload, if `reload`, and `changes` from the file watcher until the
    /// `fnug check --stash` running now has ended.
    pub(super) fn hold_back(&mut self, reload: bool, changes: Vec<WatchMatch>) {
        if self.held_back.is_none() {
            info!("A `fnug check --stash` is running; holding back auto-runs and reloads");
            self.set_status(
                "Waiting for fnug check --stash to finish",
                StatusLevel::Info,
            );
        }
        let held = self.held_back.get_or_insert_default();
        held.reload |= reload;
        held.changes.extend(changes);
    }

    /// Reload the config, if `reload`, and select `changes` from the file watcher, running
    /// the commands with `auto.run_on_change`, together with what was held back. While a
    /// `fnug check --stash` runs, hold them back too.
    pub(super) fn handle_file_change(&mut self, reload: bool, changes: Vec<WatchMatch>) {
        if self.stash_running() {
            self.hold_back(reload, changes);
            return;
        }
        let held = self.held_back.take().unwrap_or_default();
        if reload || held.reload {
            self.reload_config();
        }
        let changes = merge(held.changes.into_iter().chain(changes));
        if !changes.is_empty() {
            self.select_watch_matches(changes);
        }
    }

    /// Do what was held back, once the `fnug check --stash` it waited for has ended. Returns
    /// whether it did.
    pub(super) fn resume_after_stash(&mut self) -> bool {
        if self.held_back.is_none() {
            return false;
        }
        self.handle_file_change(false, Vec::new());
        self.held_back.is_none()
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command};
    use std::time::{Duration, Instant};

    use crate::LoadOptions;
    use crate::check::stash;
    use crate::process::ExitInfo;
    use crate::pty::test_util::pty_available;
    use crate::selectors::watch::WatchMatch;
    use crate::tui::app::{App, AppEvent};
    use crate::tui::test_util::{AREA, shell_app};

    /// `fmt` runs on file changes, `server` runs until stopped
    const CONFIG: &str = "name: root\ncommands:\n  \
        - name: fmt\n    cmd: \"true\"\n    auto:\n      watch: true\n      run_on_change: true\n  \
        - name: server\n    cmd: exec sleep 30\n";
    /// [`CONFIG`] as committed, without `server`
    const COMMITTED: &str = "name: root\ncommands:\n  \
        - name: fmt\n    cmd: \"true\"\n    auto:\n      watch: true\n      run_on_change: true\n";

    /// A new git repo, or `None` where git would look elsewhere, as in a hook of a linked
    /// worktree
    fn repo() -> Option<tempfile::TempDir> {
        if !pty_available() || std::env::var_os("GIT_DIR").is_some() {
            return None;
        }
        let dir = tempfile::tempdir().unwrap();
        git2::Repository::init(dir.path()).unwrap();
        Some(dir)
    }

    /// A live process holding the `--stash` lock in `dir`'s repo, as `fnug check --stash` does
    struct StashRun {
        process: Child,
        lock: PathBuf,
    }

    impl StashRun {
        fn start(dir: &Path) -> Self {
            let process = Command::new("sleep").arg("60").spawn().unwrap();
            let lock = stash::write_test_lock(&dir.join(".git"), process.id());
            Self { process, lock }
        }

        fn end(mut self) {
            self.process.kill().unwrap();
            self.process.wait().unwrap();
            std::fs::remove_file(&self.lock).unwrap();
        }
    }

    /// An app for [`CONFIG`] in `dir`, reloading from it, with `server` running
    fn app_in(dir: &Path) -> App {
        let path = dir.join(".fnug.yaml");
        std::fs::write(&path, CONFIG).unwrap();
        let opts = LoadOptions {
            config: Some(path),
            ..LoadOptions::default()
        };
        let loaded = crate::load(&opts).unwrap();
        let mut app = App::new(loaded.root, loaded.cwd, crate::logger::LogBuffer::new());
        app.reload = Some(opts);
        app.run_command("server", AREA);
        app
    }

    fn changed(dir: &Path, id: &str) -> AppEvent {
        AppEvent::WatcherTriggered(vec![WatchMatch {
            id: id.into(),
            files: vec![dir.join("a.txt")],
        }])
    }

    /// Handle the app's events until a config reload is handled
    async fn handle_until_reloaded(app: &mut App) {
        loop {
            let next = tokio::time::timeout(Duration::from_secs(5), app.event_rx.recv()).await;
            let event = next.expect("no reload").unwrap();
            let reloaded = matches!(event, AppEvent::ConfigReloaded { .. });
            app.handle_app_event(event);
            if reloaded {
                return;
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn live_stash_lock_holds_back_auto_runs_and_reloads() {
        let Some(dir) = repo() else { return };
        let mut app = app_in(dir.path());
        let check = StashRun::start(dir.path());
        let reloads = app.reload_generation;

        // The stash checks the index out, which the watchers see as changes
        std::fs::write(dir.path().join(".fnug.yaml"), COMMITTED).unwrap();
        app.handle_app_event(changed(dir.path(), "fmt"));
        app.handle_app_event(AppEvent::ConfigFileChanged);
        app.handle_app_event(changed(dir.path(), "fmt"));

        assert!(!app.processes.contains_key("fmt"), "auto-ran mid-stash");
        assert_eq!(app.reload_generation, reloads, "reloaded mid-stash");
        assert!(app.processes["server"].terminal.is_running());
        assert!(app.wants_tick());

        // It puts the unstaged changes back and lets go
        std::fs::write(dir.path().join(".fnug.yaml"), CONFIG).unwrap();
        check.end();
        assert!(app.tick(Instant::now()));

        assert_eq!(app.reload_generation, reloads + 1);
        let fmt = app
            .processes
            .get("fmt")
            .expect("no auto-run after the stash");
        let generation = fmt.generation;
        handle_until_reloaded(&mut app).await;
        assert!(app.processes["server"].terminal.is_running());
        assert_eq!(
            app.processes["fmt"].generation, generation,
            "auto-ran twice"
        );
        assert!(app.held_back.is_none());
        app.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reload_finishing_mid_stash_waits_for_it() {
        let Some(dir) = repo() else { return };
        let mut app = app_in(dir.path());
        std::fs::write(dir.path().join(".fnug.yaml"), COMMITTED).unwrap();
        app.reload_config();
        let check = StashRun::start(dir.path());

        handle_until_reloaded(&mut app).await;
        assert!(app.find_command("server").is_some(), "applied mid-stash");

        std::fs::write(dir.path().join(".fnug.yaml"), CONFIG).unwrap();
        check.end();
        app.tick(Instant::now());
        handle_until_reloaded(&mut app).await;
        assert!(app.processes["server"].terminal.is_running());
        app.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stash_lock_is_located_once() {
        let Some(dir) = repo() else { return };
        let mut app = shell_app(dir.path(), &[("a", "true")]);
        assert!(!app.stash_running());

        // git no longer finds a repo there, yet the lock where it was still counts
        std::fs::remove_file(dir.path().join(".git/HEAD")).unwrap();
        let check = StashRun::start(dir.path());
        assert!(app.stash_running());
        check.end();
        assert!(!app.stash_running());
        app.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rerun_due_mid_stash_waits_for_it() {
        let Some(dir) = repo() else { return };
        let mut app = shell_app(dir.path(), &[("auto", "exec sleep 30")]);
        app.config.commands[0].auto.run_on_change = Some(true);
        app.handle_app_event(changed(dir.path(), "auto"));
        let first = app.processes["auto"].generation;
        app.handle_app_event(changed(dir.path(), "auto"));

        let check = StashRun::start(dir.path());
        app.handle_app_event(AppEvent::ProcessExited {
            id: "auto".into(),
            generation: first,
            exit: ExitInfo {
                code: Some(0),
                signal: None,
                stop_requested: false,
            },
        });
        assert_eq!(app.processes["auto"].generation, first, "reran mid-stash");

        check.end();
        app.tick(Instant::now());
        assert_eq!(app.processes["auto"].generation, first + 1);
        app.shutdown().await;
    }
}
