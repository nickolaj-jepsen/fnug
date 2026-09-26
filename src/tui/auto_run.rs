//! Running the commands with `auto.run_on_change` when the file watcher selects them.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use log::info;

use crate::runner::NodeState;

use super::app::App;
use super::status::StatusLevel;

/// How long after an auto-run ends that file changes don't run the command again, so the
/// files a formatter or fixer just wrote don't set it off once more
pub const QUIET_PERIOD: Duration = Duration::from_secs(1);

impl App {
    /// Whether any command runs on file changes, which is when the `w` toggle matters
    #[must_use]
    pub fn has_auto_run_commands(&self) -> bool {
        self.config
            .all_commands()
            .iter()
            .any(|c| c.auto.run_on_change == Some(true))
    }

    /// Turn running commands on file changes on or off for this session. Does nothing but say
    /// so when no command opts in.
    pub(super) fn toggle_auto_run(&mut self) {
        if !self.has_auto_run_commands() {
            self.set_status("No command sets auto.run_on_change", StatusLevel::Info);
            return;
        }
        self.auto_run_enabled = !self.auto_run_enabled;
        if !self.auto_run_enabled {
            self.auto_run_pending.clear();
        }
        let state = if self.auto_run_enabled { "on" } else { "off" };
        self.set_status(
            format!("Auto-run on file changes: {state}"),
            StatusLevel::Info,
        );
    }

    /// Run the commands among `ids`, which the file watcher selected at `now`, that opted in
    /// with `auto.run_on_change` and aren't in their quiet period. One that is still queued
    /// or running gets one more run once it ends.
    pub(super) fn auto_run(&mut self, ids: &[String], now: Instant) {
        if !self.auto_run_enabled {
            return;
        }
        self.settle_auto_runs(now);
        let mut start = Vec::new();
        for id in ids {
            let opted_in = self
                .find_command(id)
                .is_some_and(|c| c.auto.run_on_change == Some(true));
            let quiet = self.quiet_until.get(id).is_some_and(|until| now < *until);
            if !opted_in || quiet {
                continue;
            }
            if self.dag.is_active(id) {
                self.auto_run_pending.insert(id.clone());
                self.auto_running.insert(id.clone());
            } else {
                start.push(id.clone());
            }
        }
        self.start_auto_runs(&start);
    }

    fn start_auto_runs(&mut self, ids: &[String]) {
        if ids.is_empty() {
            return;
        }
        info!("Running {ids:?} after a file change");
        self.auto_running.extend(ids.iter().cloned());
        // So a failure gets the cursor, as in any batch
        self.batch_run_ids
            .get_or_insert_with(HashSet::new)
            .extend(ids.iter().cloned());
        self.run_commands(ids, self.last_terminal_area, None);
    }

    /// Give the auto-runs that ended by `now` their quiet period, and start the reruns queued
    /// while they ran, unless they were stopped or never started.
    pub(super) fn settle_auto_runs(&mut self, now: Instant) {
        let ended: Vec<String> = self
            .auto_running
            .iter()
            .filter(|id| !self.dag.is_active(id))
            .cloned()
            .collect();
        let mut rerun = Vec::new();
        for id in ended {
            self.auto_running.remove(&id);
            self.quiet_until.insert(id.clone(), now + QUIET_PERIOD);
            let finished = matches!(
                self.dag.state(&id),
                Some(NodeState::Passed | NodeState::Failed)
            );
            if self.auto_run_pending.remove(&id) && finished && self.auto_run_enabled {
                rerun.push(id);
            }
        }
        self.start_auto_runs(&rerun);
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::{Duration, Instant};

    use crossterm::event::KeyCode;

    use crate::process::ExitInfo;
    use crate::pty::test_util::pty_available;
    use crate::selectors::watch::WatchMatch;
    use crate::tui::app::{App, AppEvent, Focus};
    use crate::tui::test_util::{cursor_node, draw, press, shell_app};

    /// `auto` running `cmd` on file changes and `plain` running `true`, both in `dir`
    fn auto_app(dir: &Path, cmd: &str) -> App {
        let mut app = shell_app(dir, &[("auto", cmd), ("plain", "true")]);
        app.config.commands[0].auto.run_on_change = Some(true);
        app
    }

    fn changed(ids: &[&str]) -> AppEvent {
        AppEvent::WatcherTriggered(
            ids.iter()
                .map(|id| WatchMatch {
                    id: (*id).into(),
                    files: vec![],
                })
                .collect(),
        )
    }

    fn exited(app: &App, id: &str) -> AppEvent {
        AppEvent::ProcessExited {
            id: id.into(),
            generation: app.processes[id].generation,
            exit: ExitInfo {
                code: Some(0),
                signal: None,
                stop_requested: false,
            },
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn watch_trigger_runs_opted_in() {
        if !pty_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let mut app = auto_app(dir.path(), "true");

        app.handle_app_event(changed(&["auto", "plain"]));

        assert!(app.processes.contains_key("auto"));
        assert!(!app.processes.contains_key("plain"));
        assert!(app.selected.contains("auto") && app.selected.contains("plain"));
        app.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn watch_quiet_period() {
        if !pty_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let mut app = auto_app(dir.path(), "true");
        app.handle_app_event(changed(&["auto"]));
        app.handle_app_event(exited(&app, "auto"));
        let first = app.processes["auto"].generation;

        // Such as the files the command itself just wrote
        app.handle_app_event(changed(&["auto"]));
        assert_eq!(app.processes["auto"].generation, first);

        let later = Instant::now() + super::QUIET_PERIOD + Duration::from_millis(100);
        app.auto_run(&["auto".into()], later);
        assert!(app.processes["auto"].generation > first);
        app.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn change_during_run_queues_one_rerun() {
        if !pty_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let mut app = auto_app(dir.path(), "exec sleep 30");
        app.handle_app_event(changed(&["auto"]));
        let first = app.processes["auto"].generation;

        app.handle_app_event(changed(&["auto"]));
        app.handle_app_event(changed(&["auto"]));
        assert_eq!(app.processes["auto"].generation, first, "restarted mid-run");

        app.handle_app_event(exited(&app, "auto"));
        assert_eq!(app.processes["auto"].generation, first + 1);
        assert!(app.auto_run_pending.is_empty());
        app.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn failed_auto_run_leaves_focused_terminal_alone() {
        if !pty_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let mut app = shell_app(
            dir.path(),
            &[("auto", "exit 1"), ("typing", "exec sleep 30")],
        );
        app.config.commands[0].auto.run_on_change = Some(true);
        draw(&mut app, 100, 24);
        while cursor_node(&app) != Some("typing") {
            press(&mut app, KeyCode::Char('j'));
        }
        app.run_command("typing", app.last_terminal_area);
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.focus, Focus::Terminal);

        app.handle_app_event(changed(&["auto"]));
        let generation = app.processes["auto"].generation;
        app.handle_app_event(AppEvent::ProcessExited {
            id: "auto".into(),
            generation,
            exit: ExitInfo {
                code: Some(1),
                signal: None,
                stop_requested: false,
            },
        });

        assert_eq!(cursor_node(&app), Some("typing"));
        assert_eq!(app.active_terminal_id.as_deref(), Some("typing"));
        assert_eq!(app.focus, Focus::Terminal);
        app.shutdown().await;
    }

    #[tokio::test]
    async fn w_toggles_auto_run() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = auto_app(dir.path(), "true");
        assert!(app.has_auto_run_commands());

        press(&mut app, KeyCode::Char('w'));
        assert!(!app.auto_run_enabled);
        let status = &app.status.as_ref().unwrap().text;
        assert_eq!(status, "Auto-run on file changes: off");
        app.handle_app_event(changed(&["auto"]));
        assert!(app.processes.is_empty());
        assert!(app.selected.contains("auto"));

        app.status = None;
        assert!(toolbar_text(&app).contains(" w  Auto-run: off "));

        press(&mut app, KeyCode::Char('w'));
        assert!(app.auto_run_enabled);
        app.status = None;
        assert!(toolbar_text(&app).contains(" w  Auto-run: on "));
    }

    fn toolbar_text(app: &App) -> String {
        let (line, _) = crate::tui::toolbar::build_toolbar_line(app, 300);
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn auto_run_toggle_hidden_without_opted_in_commands() {
        let mut app = crate::tui::test_util::two_groups();
        assert!(!app.has_auto_run_commands());
        assert!(!toolbar_text(&app).contains("Auto-run"));

        press(&mut app, KeyCode::Char('w'));
        assert!(app.auto_run_enabled, "toggled with nothing to auto-run");
        let status = &app.status.as_ref().unwrap().text;
        assert_eq!(status, "No command sets auto.run_on_change");
    }
}
