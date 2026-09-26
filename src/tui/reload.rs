//! Reloading the config while the TUI runs, keeping what it can by command and group id.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use log::{info, warn};

use crate::commands::command::Command;
use crate::commands::group::CommandGroup;
use crate::process::StopSignal;
use crate::{LoadOptions, LoadedConfig};

use super::app::{App, AppEvent};
use super::selection::SelectionReason;
use super::status::StatusLevel;
use super::watcher;

fn group_ids(group: &CommandGroup, ids: &mut HashSet<String>) {
    ids.insert(group.id.clone());
    for child in &group.children {
        group_ids(child, ids);
    }
}

/// Whether a running `old` would behave like `new`, so it needs no restart
fn same_process(old: &Command, new: &Command) -> bool {
    old.cmd == new.cmd && old.cwd == new.cwd && old.env == new.env
}

impl App {
    /// (Re)start the `auto.watch` file watcher for the current config.
    pub fn start_file_watcher(&mut self) {
        if let Some(handle) = self.file_watcher.take() {
            handle.abort();
        }
        let commands = self.config.all_commands().into_iter().cloned().collect();
        self.file_watcher = Some(watcher::start_file_watcher(
            commands,
            self.event_tx.clone(),
            Arc::clone(&self.watcher_warning),
        ));
    }

    /// Reload the config with `opts` on `F5`, and whenever one of `sources` changes.
    pub fn watch_config(&mut self, opts: LoadOptions, sources: &[PathBuf]) {
        self.reload = Some(opts);
        self.watch_config_sources(sources);
    }

    fn watch_config_sources(&mut self, sources: &[PathBuf]) {
        self.config_watcher = None;
        match watcher::watch_config_files(sources, self.event_tx.clone()) {
            Ok(watcher) => {
                self.config_watcher = Some(watcher);
                self.config_sources = sources.to_vec();
            }
            Err(e) => warn!("Not watching the config for changes: {e}"),
        }
    }

    /// Load the config again in the background; [`AppEvent::ConfigReloaded`] brings the result.
    pub fn reload_config(&mut self) {
        let Some(opts) = self.reload.clone() else {
            self.set_status("Can't reload: no config file", StatusLevel::Warn);
            return;
        };
        self.reload_generation += 1;
        let generation = self.reload_generation;
        let event_tx = self.event_tx.clone();
        tokio::task::spawn_blocking(move || {
            let result = crate::load(&opts).map(Box::new).map_err(|e| e.to_string());
            let _ = event_tx.blocking_send(AppEvent::ConfigReloaded { generation, result });
        });
    }

    /// Take the result of the reload of `generation`, unless a later reload is under way.
    pub(super) fn finish_reload(
        &mut self,
        generation: u64,
        result: Result<Box<LoadedConfig>, String>,
    ) {
        if generation != self.reload_generation {
            return;
        }
        // It may have read the index's config
        if self.stash_running() {
            self.hold_back(true, Vec::new());
            return;
        }
        match result {
            Ok(loaded) => self.apply_config(*loaded),
            Err(e) => {
                warn!("Config not reloaded: {e}");
                // The toolbar cuts it at its width; relative paths leave room for the reason
                let short = e.replace(&format!("{}/", self.cwd.display()), "");
                self.config_error = Some(format!("Config not reloaded: {short}"));
                // Such as "Config reloaded" from the save before, which would hide the error
                self.status = None;
            }
        }
    }

    /// Switch to the reloaded config. Output, selection, expansion and the cursor stay with
    /// the commands and groups whose ids remain; commands that are gone are stopped.
    pub fn apply_config(&mut self, loaded: LoadedConfig) {
        let new_ids: HashSet<String> = loaded
            .root
            .all_commands()
            .iter()
            .map(|c| c.id.clone())
            .collect();
        let mut new_groups = HashSet::new();
        group_ids(&loaded.root, &mut new_groups);
        let old_ids: HashSet<String> = self
            .config
            .all_commands()
            .iter()
            .map(|c| c.id.clone())
            .collect();

        let removed: Vec<String> = old_ids.difference(&new_ids).cloned().collect();
        for id in &removed {
            if let Some(proc) = self.processes.remove(id) {
                info!("Stopping '{id}': it is no longer in the config");
                proc.stop_and_abort(id, StopSignal::Interrupt);
            }
            self.dag.stop(id);
        }
        let mut changed: Vec<String> = loaded
            .root
            .all_commands()
            .into_iter()
            .filter(|new| {
                self.processes
                    .get(&new.id)
                    .is_some_and(|p| p.terminal.is_running())
                    && self
                        .find_command(&new.id)
                        .is_some_and(|old| !same_process(&old, new))
            })
            .map(|c| c.name.clone())
            .collect();
        changed.sort();

        self.selected.retain(|id| new_ids.contains(id));
        self.selection_reason.retain(|id, _| new_ids.contains(id));
        self.error_messages.retain(|id, _| new_ids.contains(id));
        self.queued_generation.retain(|id, _| new_ids.contains(id));
        self.auto_run_pending.retain(|id| new_ids.contains(id));
        self.auto_running.retain(|id| new_ids.contains(id));
        self.quiet_until.retain(|id, _| new_ids.contains(id));
        if let Some(batch) = &mut self.batch_run_ids {
            batch.retain(|id| new_ids.contains(id));
        }
        self.expanded.retain(|id, _| new_groups.contains(id));
        self.user_expansion.retain(|id| new_groups.contains(id));
        if self
            .active_terminal_id
            .as_ref()
            .is_some_and(|id| !new_ids.contains(id))
        {
            self.active_terminal_id = None;
        }
        for cmd in loaded.root.all_commands() {
            if !old_ids.contains(&cmd.id) && cmd.auto.always == Some(true) {
                self.selected.insert(cmd.id.clone());
                self.selection_reason
                    .insert(cmd.id.clone(), SelectionReason::Always);
            }
        }

        self.config = loaded.root;
        self.cwd = loaded.cwd;
        self.config_error = None;
        self.rebuild_visible_nodes();
        self.update_active_terminal();
        self.start_file_watcher();
        if loaded.sources != self.config_sources && self.reload.is_some() {
            self.watch_config_sources(&loaded.sources);
        }
        self.check_batch_complete();

        info!("Config reloaded");
        let message = if changed.is_empty() {
            "Config reloaded".to_string()
        } else {
            format!(
                "Config reloaded; restart {} to apply the changes",
                changed.join(", ")
            )
        };
        self.set_status(message, StatusLevel::Info);
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;
    use std::time::Duration;

    use crossterm::event::KeyCode;

    use crate::pty::test_util::pty_available;
    use crate::tui::app::{App, AppEvent};
    use crate::tui::test_util::{AREA, cursor_node, draw, press, shell_app, shell_group};
    use crate::{LoadOptions, LoadedConfig};

    fn loaded(dir: &Path, commands: &[(&str, &str)]) -> LoadedConfig {
        LoadedConfig {
            root: shell_group(dir, commands),
            cwd: dir.to_path_buf(),
            config_path: dir.join(".fnug.yaml"),
            sources: Vec::new(),
        }
    }

    fn status(app: &App) -> &str {
        app.status.as_ref().map_or("", |s| s.text.as_str())
    }

    async fn next_event(app: &mut App) -> AppEvent {
        let event = tokio::time::timeout(Duration::from_secs(5), app.event_rx.recv()).await;
        event.expect("no app event").expect("event channel closed")
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn apply_config_retains_by_id() {
        if !pty_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let commands = [("a", "exec sleep 30"), ("b", "true")];
        let mut app = shell_app(dir.path(), &commands);
        app.run_command("a", AREA);
        let running = Arc::clone(&app.processes["a"].terminal);
        draw(&mut app, 80, 24);
        while cursor_node(&app) != Some("b") {
            press(&mut app, KeyCode::Char('j'));
        }
        press(&mut app, KeyCode::Char(' '));

        let commands = [("a", "exec sleep 30"), ("b", "true"), ("c", "true")];
        app.apply_config(loaded(dir.path(), &commands));

        assert!(Arc::ptr_eq(&app.processes["a"].terminal, &running));
        assert!(running.is_running());
        assert!(app.selected.contains("b"));
        assert_eq!(cursor_node(&app), Some("b"));
        assert!(app.visible_nodes.iter().any(|n| n.id == "c"));
        assert_eq!(status(&app), "Config reloaded");
        app.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn apply_config_stops_removed() {
        if !pty_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let mut app = shell_app(dir.path(), &[("a", "exec sleep 30"), ("b", "true")]);
        app.run_command("a", AREA);
        let removed = Arc::clone(&app.processes["a"].terminal);

        app.apply_config(loaded(dir.path(), &[("b", "true")]));

        assert!(!app.processes.contains_key("a"));
        assert_eq!(app.active_terminal_id, None);
        let exit = tokio::time::timeout(Duration::from_secs(3), removed.wait()).await;
        let exit = exit.expect("removed command kept running").unwrap();
        assert!(exit.stop_requested);
        app.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn changed_running_command_asks_for_restart() {
        if !pty_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let mut app = shell_app(dir.path(), &[("a", "exec sleep 30")]);
        app.run_command("a", AREA);

        app.apply_config(loaded(dir.path(), &[("a", "exec sleep 31")]));

        assert!(app.processes["a"].terminal.is_running());
        assert_eq!(
            status(&app),
            "Config reloaded; restart a to apply the changes"
        );
        app.shutdown().await;
    }

    #[tokio::test]
    async fn apply_config_error_keeps_old() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = shell_app(dir.path(), &[("a", "true")]);
        app.reload_generation = 2;
        app.set_status("Config reloaded", crate::tui::status::StatusLevel::Info);

        let failed = AppEvent::ConfigReloaded {
            generation: 2,
            result: Err("unknown field `cmdd`".into()),
        };
        app.handle_app_event(failed);

        assert!(app.find_command("a").is_some());
        let screen = draw(&mut app, 100, 24).0.join("\n");
        assert!(
            screen.contains("Config not reloaded: unknown field `cmdd`"),
            "{screen}"
        );

        // An older reload's result is ignored, and the latest one's clears the error
        let stale = AppEvent::ConfigReloaded {
            generation: 1,
            result: Ok(Box::new(loaded(dir.path(), &[]))),
        };
        app.handle_app_event(stale);
        assert!(app.find_command("a").is_some());
        let latest = AppEvent::ConfigReloaded {
            generation: 2,
            result: Ok(Box::new(loaded(dir.path(), &[("b", "true")]))),
        };
        app.handle_app_event(latest);
        assert!(app.find_command("b").is_some());
        assert_eq!(app.config_error, None);
    }

    #[tokio::test]
    async fn reload_error_names_files_relative_to_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = shell_app(dir.path(), &[("a", "true")]);
        app.reload_generation = 1;
        let path = dir.path().join("api/.fnug.yaml");

        app.handle_app_event(AppEvent::ConfigReloaded {
            generation: 1,
            result: Err(format!("Unable to parse {}: bad", path.display())),
        });

        assert_eq!(
            app.config_error.as_deref(),
            Some("Config not reloaded: Unable to parse api/.fnug.yaml: bad")
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reload_message_not_replaced_by_known_watch_problems() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir(&src).unwrap();
        let mut root = shell_group(dir.path(), &[("a", "true")]);
        root.commands[0].auto.watch = Some(true);
        root.commands[0].auto.path = Some(vec![src.clone(), dir.path().join("missing")]);
        let mut app = App::new(
            root.clone(),
            dir.path().to_path_buf(),
            crate::logger::LogBuffer::new(),
        );

        app.start_file_watcher();
        let AppEvent::Status { text, .. } = next_event(&mut app).await else {
            panic!("expected the watch problems first");
        };
        assert_eq!(text, "Not watching every file: 1 missing path (see logs)");

        app.apply_config(LoadedConfig {
            root,
            ..loaded(dir.path(), &[])
        });
        assert_eq!(status(&app), "Config reloaded");
        // The new watcher is up once a change reaches it; a repeated warning would come first
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        let event = loop {
            assert!(tokio::time::Instant::now() < deadline, "no watch event");
            std::fs::write(src.join("lib.rs"), "").unwrap();
            let wait = Duration::from_millis(200);
            if let Ok(event) = tokio::time::timeout(wait, app.event_rx.recv()).await {
                break event.expect("event channel closed");
            }
        };
        if let AppEvent::Status { text, .. } = &event {
            panic!("status posted again: {text}");
        }
        assert!(matches!(event, AppEvent::WatcherTriggered(_)));
        app.shutdown().await;
    }

    const ONE: &str = "name: root\ncommands:\n  - name: one\n    cmd: \"true\"\n";
    const TWO: &str = "name: root\ncommands:\n  - name: one\n    cmd: \"true\"\n  \
                       - name: two\n    cmd: \"true\"\n";

    /// An app for the config file [`ONE`] in a new directory, reloading from it
    fn app_from_disk() -> (tempfile::TempDir, App) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".fnug.yaml");
        std::fs::write(&path, ONE).unwrap();
        let opts = LoadOptions {
            config: Some(path),
            ..LoadOptions::default()
        };
        let loaded = crate::load(&opts).unwrap();
        let mut app = App::new(loaded.root, loaded.cwd, crate::logger::LogBuffer::new());
        app.watch_config(opts, &loaded.sources);
        (dir, app)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn config_file_change_reloads() {
        let (dir, mut app) = app_from_disk();

        std::fs::write(dir.path().join(".fnug.yaml"), TWO).unwrap();
        let changed = next_event(&mut app).await;
        assert!(matches!(changed, AppEvent::ConfigFileChanged));
        app.handle_app_event(changed);
        let reloaded = next_event(&mut app).await;
        app.handle_app_event(reloaded);

        assert!(app.find_command("two").is_some());
        app.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn f5_reloads() {
        let (dir, mut app) = app_from_disk();
        // Only F5 reloads from here on
        app.config_watcher = None;
        std::fs::write(dir.path().join(".fnug.yaml"), TWO).unwrap();

        press(&mut app, KeyCode::F(5));
        let reloaded = next_event(&mut app).await;
        assert!(matches!(reloaded, AppEvent::ConfigReloaded { .. }));
        app.handle_app_event(reloaded);

        assert!(app.find_command("two").is_some());
        app.shutdown().await;
    }
}
