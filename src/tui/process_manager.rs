use std::io::Write;
use std::process::{Command as StdCommand, Stdio};
use std::sync::Arc;
use std::time::Instant;

use log::{debug, error, info, warn};
use ratatui::layout::Rect;

use crate::commands::command::Command;
use crate::process::StopSignal;
use crate::pty::terminal::{Terminal, TerminalOptions, TerminalSize};
use crate::pty::{format_exit_message, format_start_message};
use crate::runner::{self, NodeState, PlanOptions, Selection};

use super::app::{App, AppEvent, CommandStatus, ProcessInstance, STOP_GRACE};
use super::tree_state::find_group_in_group;

/// Copy text to the system clipboard using platform-native commands.
fn set_clipboard(text: &str) -> Result<(), String> {
    // Try clipboard commands in order of preference
    let candidates: &[(&str, &[&str])] = if cfg!(target_os = "macos") {
        &[("pbcopy", &[])]
    } else {
        &[
            ("wl-copy", &[]),
            ("xclip", &["-selection", "clipboard"]),
            ("xsel", &["--clipboard", "--input"]),
        ]
    };

    for (cmd, args) in candidates {
        let result = StdCommand::new(cmd)
            .args(*args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn();

        match result {
            Ok(mut child) => {
                if let Some(mut stdin) = child.stdin.take()
                    && let Err(e) = stdin.write_all(text.as_bytes())
                {
                    return Err(format!("{cmd}: failed to write: {e}"));
                }
                match child.wait() {
                    Ok(status) if status.success() => return Ok(()),
                    Ok(status) => {
                        return Err(format!("{cmd}: exited with {status}"));
                    }
                    Err(e) => {
                        return Err(format!("{cmd}: failed to wait: {e}"));
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(format!("{cmd}: {e}"));
            }
        }
    }

    Err("no clipboard command found (tried wl-copy, xclip, xsel)".into())
}

impl App {
    fn spawn_exit_watcher(
        term: &Arc<Terminal>,
        cmd_id: &str,
        generation: u64,
        event_tx: &tokio::sync::mpsc::Sender<AppEvent>,
    ) -> tokio::task::JoinHandle<()> {
        let term = Arc::clone(term);
        let tx = event_tx.clone();
        let id = cmd_id.to_string();
        tokio::spawn(async move {
            match term.wait().await {
                Ok(exit) => {
                    if let Err(e) = term.echo(format_exit_message(&exit)) {
                        debug!("Failed to echo exit message: {e}");
                    }
                    let event = AppEvent::ProcessExited {
                        id,
                        generation,
                        exit,
                    };
                    if let Err(e) = tx.send(event).await {
                        debug!("Failed to send process exit event: {e}");
                    }
                }
                Err(e) => {
                    let message = format!("Process wait error: {e}");
                    let event = AppEvent::ProcessError {
                        id,
                        generation,
                        message,
                    };
                    let _ = tx.send(event).await;
                }
            }
        })
    }

    /// Run `ids` and the commands they depend on, each once and after its dependencies. The
    /// terminal pane switches to `focus`, if given.
    ///
    /// Every id in `ids` starts anew, stopping a run still going, and waits for the dependencies
    /// in `ids`. A dependency outside `ids` is reused if it passed earlier in the session or is
    /// still queued or running; otherwise it runs too.
    pub fn run_commands(&mut self, ids: &[String], terminal_area: Rect, focus: Option<&str>) {
        self.last_terminal_area = terminal_area;
        let dag = &self.dag;
        let reuse = |dep: &Command| {
            dag.state(&dep.id) == Some(&NodeState::Passed) || dag.is_active(&dep.id)
        };
        let opts = PlanOptions {
            reuse_dep: Some(&reuse),
        };
        let plan = match runner::plan(&self.config, &Selection::Ids(ids.to_vec()), &opts) {
            Ok(plan) => plan,
            Err(e) => {
                error!("Can't run {ids:?}: {e}");
                return;
            }
        };

        for id in plan.ids() {
            self.error_messages.remove(id);
        }
        for id in self.dag.submit(&plan) {
            // No exit event comes for the replaced run, and none is expected
            if let Some(proc) = self.processes.remove(&id) {
                proc.stop_and_abort(&id, StopSignal::Interrupt);
            }
        }
        if let Some(id) = focus {
            self.active_terminal_id = Some(id.to_string());
        }
        self.start_ready();
        self.mark_tree_dirty();
        self.check_batch_complete();
    }

    /// Run one command, after its dependencies, and show its terminal.
    pub fn run_command(&mut self, cmd_id: &str, terminal_area: Rect) {
        self.run_commands(&[cmd_id.to_string()], terminal_area, Some(cmd_id));
    }

    /// Start every command whose dependencies have passed. A command that fails to start fails
    /// the commands waiting on it.
    pub(super) fn start_ready(&mut self) {
        while let Some(id) = self.dag.pop_ready(usize::MAX) {
            if let Err(msg) = self.spawn_pty(&id) {
                error!("{msg}");
                self.error_messages.insert(id.clone(), msg);
                self.finish_failed(&id);
            }
        }
    }

    /// Start `cmd_id` in a new terminal, stopping its previous run. Returns the message to show
    /// when it can't start.
    fn spawn_pty(&mut self, cmd_id: &str) -> Result<(), String> {
        let Some(mut cmd) = self.find_command(cmd_id) else {
            return Err(format!("Unknown command '{cmd_id}'"));
        };
        cmd.cwd = cmd.effective_cwd(&self.cwd).to_path_buf();
        info!("Starting command '{}'", cmd.name);

        if let Some(proc) = self.processes.remove(cmd_id) {
            proc.stop_and_abort(cmd_id, StopSignal::Interrupt);
        }

        let size = TerminalSize::new(
            self.last_terminal_area.width.max(2),
            self.last_terminal_area.height.max(2),
        );
        let opts = TerminalOptions {
            scrollback: cmd
                .scrollback
                .unwrap_or_else(Terminal::default_scrollback_size),
            output_notify: None,
        };
        let terminal = Terminal::new(&cmd, size, opts)
            .map_err(|e| format!("Failed to start '{}': {e}", cmd.name))?;
        if let Err(e) = terminal.echo(format_start_message(&cmd.cmd)) {
            warn!("Failed to echo start message: {e}");
        }

        self.next_generation += 1;
        let generation = self.next_generation;
        let term_ref = Arc::new(terminal);
        let exit_handle = Self::spawn_exit_watcher(&term_ref, cmd_id, generation, &self.event_tx);
        self.processes.insert(
            cmd_id.to_string(),
            ProcessInstance {
                terminal: term_ref,
                status: CommandStatus::Running,
                task_handles: vec![exit_handle],
                started_at: Instant::now(),
                finished_at: None,
                exit: None,
                generation,
            },
        );
        Ok(())
    }

    /// Stop a command: drop its queued run, or interrupt its process. Either way, the commands
    /// queued behind it are cancelled.
    pub fn stop_command(&mut self, cmd_id: &str) {
        info!("Stopping command '{cmd_id}'");
        if matches!(
            self.dag.state(cmd_id),
            Some(NodeState::Waiting(_) | NodeState::Ready)
        ) {
            self.mark_stopped(cmd_id);
        }
        // A running command's dependents are cancelled once it exits
        if let Some(proc) = self.processes.get(cmd_id)
            && let Err(e) = proc.terminal.stop(StopSignal::Interrupt, STOP_GRACE)
        {
            warn!("Failed to stop process '{cmd_id}': {e}");
        }
        self.mark_tree_dirty();
        self.check_batch_complete();
    }

    /// Copy a command's terminal output to the system clipboard
    pub fn copy_command_output(&self, cmd_id: &str) {
        let Some(proc) = self.processes.get(cmd_id) else {
            info!("No process found for '{cmd_id}', nothing to copy");
            return;
        };
        let mut parser = proc.terminal.parser().lock();
        let scrollback_len = parser.screen().scrollback_len();
        let original_scrollback = parser.screen().scrollback();
        parser.set_scrollback(scrollback_len);
        let contents = parser.screen().contents();
        parser.set_scrollback(original_scrollback);
        drop(parser);

        // Strip fnug's own echoed lines (start banner and result status)
        let contents: String = contents
            .lines()
            .filter(|line| !line.contains('❱'))
            .collect::<Vec<_>>()
            .join("\n");
        let contents = contents.trim().to_string();

        let len = contents.len();
        info!("Copying {len} bytes from '{cmd_id}' to clipboard");

        std::thread::spawn(move || {
            if let Err(e) = set_clipboard(&contents) {
                log::error!("Failed to copy to clipboard: {e}");
            } else {
                log::info!("Copied to clipboard successfully");
            }
        });
    }

    /// Clear a command's terminal and error, and drop its queued run or stop its process. The
    /// commands queued behind it won't run.
    pub fn clear_command(&mut self, cmd_id: &str) {
        if let Some(proc) = self.processes.remove(cmd_id) {
            proc.stop_and_abort(cmd_id, StopSignal::Interrupt);
        }
        self.error_messages.remove(cmd_id);
        for (dependent, _) in self.dag.abort(cmd_id) {
            info!("Dropped '{dependent}', which was waiting on '{cmd_id}'");
        }
        self.mark_tree_dirty();
        self.check_batch_complete();
    }

    /// Run the selected commands, and focus the first failure once they have all finished.
    pub fn run_selected(&mut self, terminal_area: Rect) {
        let ids: Vec<String> = self
            .config
            .all_commands()
            .into_iter()
            .filter(|c| self.selected.contains(&c.id))
            .map(|c| c.id.clone())
            .collect();
        info!("Running {} selected commands", ids.len());
        // Stay on the command at the cursor if it has output to show
        let focus = self
            .current_command_id()
            .filter(|id| ids.contains(id) || self.processes.contains_key(id))
            .or_else(|| ids.first().cloned());
        self.batch_run_ids = Some(ids.iter().cloned().collect());
        self.run_commands(&ids, terminal_area, focus.as_deref());
    }

    /// Run all commands in a group (and nested subgroups).
    pub fn run_group(&mut self, group_id: &str, terminal_area: Rect) {
        let Some(group) = find_group_in_group(&self.config, group_id) else {
            return;
        };
        let ids: Vec<String> = group.all_commands().iter().map(|c| c.id.clone()).collect();
        info!("Running {} commands in group '{group_id}'", ids.len());
        let focus = ids.first().cloned();
        self.run_commands(&ids, terminal_area, focus.as_deref());
    }

    /// Resize all active terminals
    pub fn resize_terminals(&self, area: Rect) {
        for proc in self.processes.values() {
            if matches!(proc.status, CommandStatus::Running)
                && let Err(e) = proc
                    .terminal
                    .resize(TerminalSize::new(area.width.max(2), area.height.max(2)))
            {
                debug!("Failed to resize terminal: {e}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use ratatui::layout::Rect;

    use crate::commands::command::Command;
    use crate::commands::group::CommandGroup;
    use crate::process::ExitInfo;
    use crate::pty::test_util::{pty_available, wait_until};
    use crate::runner::NodeState;
    use crate::tui::app::{App, AppEvent, CommandStatus};
    use crate::tui::log_state::LogBuffer;
    use crate::tui::tree_widget::NodeKind;

    const AREA: Rect = Rect::new(0, 0, 80, 24);

    /// `test` depends on `build`; both run `true` in `dir`.
    fn dep_app(dir: &Path) -> App {
        let command = |id: &str, depends_on: Vec<String>| Command {
            id: id.into(),
            name: id.into(),
            cmd: "true".into(),
            cwd: dir.to_path_buf(),
            depends_on,
            ..Default::default()
        };
        let config = CommandGroup {
            id: "root".into(),
            name: "root".into(),
            commands: vec![
                command("build", vec![]),
                command("test", vec!["build".into()]),
            ],
            ..Default::default()
        };
        App::new(config, dir.to_path_buf(), LogBuffer::new())
    }

    /// Exit event for the current run of `id`
    fn exited(app: &App, id: &str, code: i32) -> AppEvent {
        AppEvent::ProcessExited {
            id: id.into(),
            generation: app.processes[id].generation,
            exit: ExitInfo {
                code: Some(code),
                signal: None,
                stop_requested: false,
            },
        }
    }

    fn node_status(app: &mut App, id: &str) -> CommandStatus {
        app.rebuild_visible_nodes();
        let node = app.visible_nodes.iter().find(|n| n.id == id).unwrap();
        match &node.kind {
            NodeKind::Command { status, .. } => status.clone(),
            NodeKind::Group { .. } => panic!("'{id}' is a group"),
        }
    }

    fn ids(ids: &[&str]) -> Vec<String> {
        ids.iter().map(ToString::to_string).collect()
    }

    /// Whether `id` is queued behind its dependencies
    fn waits(app: &App, id: &str) -> bool {
        matches!(app.dag.state(id), Some(NodeState::Waiting(_)))
    }

    fn running_ids(app: &App) -> Vec<&str> {
        let mut running: Vec<&str> = app
            .processes
            .iter()
            .filter(|(_, p)| p.status == CommandStatus::Running)
            .map(|(id, _)| id.as_str())
            .collect();
        running.sort_unstable();
        running
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn sticky_dep_error_cleared_on_rerun() {
        if !pty_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let mut app = dep_app(dir.path());

        app.run_command("test", AREA);
        app.handle_app_event(exited(&app, "build", 1));
        assert_eq!(
            node_status(&mut app, "test"),
            CommandStatus::Error("Dependency 'build' failed".into())
        );

        app.run_command("test", AREA);
        app.handle_app_event(exited(&app, "build", 0));
        app.handle_app_event(exited(&app, "test", 0));

        assert_eq!(node_status(&mut app, "test"), CommandStatus::Success);
        assert!(!app.error_messages.contains_key("test"));
        app.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn clear_command_drops_error_and_queued_run() {
        if !pty_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let mut app = dep_app(dir.path());

        app.run_command("test", AREA);
        app.handle_app_event(exited(&app, "build", 1));
        app.clear_command("test");
        assert!(!app.error_messages.contains_key("test"));
        assert_eq!(node_status(&mut app, "test"), CommandStatus::Pending);

        app.run_command("test", AREA);
        assert!(waits(&app, "test"));
        app.clear_command("test");
        assert!(!waits(&app, "test"));

        // A cleared command must not be started when its dependency finishes
        app.handle_app_event(exited(&app, "build", 0));
        assert!(!app.processes.contains_key("test"));
        app.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn clearing_queued_command_cancels_its_dependents() {
        if !pty_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let command = |id: &str, depends_on: &[&str]| Command {
            id: id.into(),
            name: id.into(),
            cmd: "true".into(),
            cwd: dir.path().to_path_buf(),
            depends_on: depends_on.iter().map(|d| (*d).to_string()).collect(),
            ..Default::default()
        };
        let config = CommandGroup {
            id: "root".into(),
            name: "root".into(),
            commands: vec![
                command("a", &[]),
                command("b", &["a"]),
                command("c", &["b"]),
            ],
            ..Default::default()
        };
        let mut app = App::new(config, dir.path().to_path_buf(), LogBuffer::new());

        app.run_command("c", AREA);
        assert!(waits(&app, "b"));
        assert!(waits(&app, "c"));

        app.clear_command("b");
        assert!(!waits(&app, "c"), "c still waits on b");
        assert!(!app.error_messages.contains_key("c"));

        app.handle_app_event(exited(&app, "a", 0));
        assert!(!app.processes.contains_key("b"));
        assert!(!app.processes.contains_key("c"));
        app.shutdown().await;
    }

    fn single_command_app(dir: &Path, cmd: &str, cwd: PathBuf) -> App {
        let config = CommandGroup {
            id: "root".into(),
            name: "root".into(),
            commands: vec![Command {
                id: "a".into(),
                name: "a".into(),
                cmd: cmd.into(),
                cwd,
                ..Default::default()
            }],
            ..Default::default()
        };
        App::new(config, dir.to_path_buf(), LogBuffer::new())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn missing_cwd_is_reported_not_run() {
        if !pty_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let gone = dir.path().join("gone");
        let mut app = single_command_app(dir.path(), "true", gone);

        app.run_command("a", AREA);
        assert!(!app.processes.contains_key("a"));
        let msg = app.error_messages.get("a").expect("no error shown");
        assert!(msg.contains("does not exist"), "{msg}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn spawn_failure_fails_dependents() {
        if !pty_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let config = CommandGroup {
            id: "root".into(),
            name: "root".into(),
            commands: vec![
                Command {
                    id: "gone".into(),
                    name: "gone".into(),
                    cmd: "true".into(),
                    cwd: dir.path().join("gone"),
                    ..Default::default()
                },
                Command {
                    id: "dependent".into(),
                    name: "dependent".into(),
                    cmd: "true".into(),
                    cwd: dir.path().to_path_buf(),
                    depends_on: vec!["gone".into()],
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let mut app = App::new(config, dir.path().to_path_buf(), LogBuffer::new());

        app.run_command("dependent", AREA);

        assert!(app.error_messages.contains_key("gone"));
        assert!(
            !waits(&app, "dependent"),
            "dependent waits for a dependency that never started"
        );
        assert_eq!(
            node_status(&mut app, "dependent"),
            CommandStatus::Error("Dependency 'gone' failed".into())
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn empty_cwd_runs_in_app_cwd() {
        if !pty_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let mut app = single_command_app(dir.path(), "touch marker", PathBuf::new());

        app.run_command("a", AREA);
        assert_eq!(app.error_messages.get("a"), None);
        assert!(wait_until(Duration::from_secs(5), || dir
            .path()
            .join("marker")
            .exists()));
        app.shutdown().await;
    }

    /// `a` creates `ready` and sleeps; `b` depends on `a`.
    fn sleeper_app(dir: &Path) -> App {
        let command = |id: &str, cmd: &str, depends_on: Vec<String>| Command {
            id: id.into(),
            name: id.into(),
            cmd: cmd.into(),
            cwd: dir.to_path_buf(),
            depends_on,
            ..Default::default()
        };
        let config = CommandGroup {
            id: "root".into(),
            name: "root".into(),
            commands: vec![
                // A builtin, not `touch`: bash drops a SIGINT that arrives while it waits on a child
                command("a", ": > ready; exec sleep 30", vec![]),
                command("b", "true", vec!["a".into()]),
            ],
            ..Default::default()
        };
        App::new(config, dir.to_path_buf(), LogBuffer::new())
    }

    async fn next_event(app: &mut App) -> AppEvent {
        let event = tokio::time::timeout(Duration::from_secs(5), app.event_rx.recv()).await;
        event.expect("no app event").expect("event channel closed")
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stopped_command_cancels_dependents() {
        if !pty_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let mut app = sleeper_app(dir.path());

        app.run_command("b", AREA);
        assert!(waits(&app, "b"));
        assert!(wait_until(Duration::from_secs(5), || dir
            .path()
            .join("ready")
            .exists()));

        app.stop_command("a");
        let event = next_event(&mut app).await;
        app.handle_app_event(event);

        assert_eq!(node_status(&mut app, "a"), CommandStatus::Stopped);
        assert!(app.processes["a"].exit.as_ref().unwrap().stop_requested);
        assert!(!waits(&app, "b"), "b still waits on a");
        assert_eq!(app.error_messages.get("b"), None);
        assert!(!app.processes.contains_key("b"));
        app.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stopping_queued_command_drops_it() {
        if !pty_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let mut app = sleeper_app(dir.path());

        app.run_command("b", AREA);
        app.stop_command("b");

        assert!(!waits(&app, "b"));
        assert!(
            app.processes["a"].terminal.is_running(),
            "stopped the dependency"
        );
        app.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn queued_rerun_stops_previous_run() {
        if !pty_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let command = |id: &str, cmd: &str, depends_on: Vec<String>| Command {
            id: id.into(),
            name: id.into(),
            cmd: cmd.into(),
            cwd: dir.path().to_path_buf(),
            depends_on,
            ..Default::default()
        };
        let config = CommandGroup {
            id: "root".into(),
            name: "root".into(),
            commands: vec![
                command("a", "exec sleep 30", vec![]),
                command("b", ": > ready; exec sleep 30", vec!["a".into()]),
            ],
            ..Default::default()
        };
        let mut app = App::new(config, dir.path().to_path_buf(), LogBuffer::new());
        app.run_command("b", AREA);
        app.handle_app_event(exited(&app, "a", 0));
        assert!(wait_until(Duration::from_secs(5), || dir
            .path()
            .join("ready")
            .exists()));
        let first_run = std::sync::Arc::clone(&app.processes["b"].terminal);

        // The rerun of `b` waits for the rerun of `a`; its first run must not keep going
        app.run_commands(&ids(&["a", "b"]), AREA, Some("b"));
        assert!(waits(&app, "b"));
        let exit = tokio::time::timeout(Duration::from_secs(3), first_run.wait()).await;
        let exit = exit.expect("previous run was not stopped").unwrap();
        assert!(exit.stop_requested);

        app.stop_command("b");
        assert!(!waits(&app, "b"));
        app.shutdown().await;
    }

    /// `a` <- `b` <- `c`, each running `true` in `dir`.
    fn chain_app(dir: &Path) -> App {
        let command = |id: &str, depends_on: &[&str]| Command {
            id: id.into(),
            name: id.into(),
            cmd: "true".into(),
            cwd: dir.to_path_buf(),
            depends_on: ids(depends_on),
            ..Default::default()
        };
        let config = CommandGroup {
            id: "root".into(),
            name: "root".into(),
            commands: vec![
                command("a", &[]),
                command("b", &["a"]),
                command("c", &["b"]),
            ],
            ..Default::default()
        };
        App::new(config, dir.to_path_buf(), LogBuffer::new())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn rerun_chain_respects_depends_on() {
        if !pty_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let mut app = chain_app(dir.path());
        let chain = ids(&["a", "b", "c"]);

        app.run_commands(&chain, AREA, None);
        for id in ["a", "b", "c"] {
            assert_eq!(running_ids(&app), [id]);
            app.handle_app_event(exited(&app, id, 0));
        }

        // Every command passed before, but each still waits for its dependency's rerun
        app.run_commands(&chain, AREA, None);
        assert_eq!(running_ids(&app), ["a"]);
        assert!(waits(&app, "b") && waits(&app, "c"));
        app.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn batch_spawns_dependency_once() {
        if !pty_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let mut app = dep_app(dir.path());

        app.run_commands(&ids(&["test", "build"]), AREA, None);

        assert_eq!(app.next_generation, 1, "build was spawned more than once");
        assert_eq!(running_ids(&app), ["build"]);
        assert!(waits(&app, "test"));
        app.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn clearing_running_dependency_resolves_dependents() {
        if !pty_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let mut app = sleeper_app(dir.path());
        app.run_commands(&ids(&["b"]), AREA, Some("b"));
        assert!(waits(&app, "b"));

        app.clear_command("a");

        assert!(!waits(&app, "b"), "b still waits on a cleared dependency");
        assert!(!app.processes.contains_key("b"));
        app.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn waiting_command_shows_as_waiting() {
        if !pty_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let mut app = sleeper_app(dir.path());

        app.run_commands(&ids(&["b"]), AREA, Some("b"));

        assert_eq!(node_status(&mut app, "b"), CommandStatus::WaitingForDeps);
        app.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn batch_completes_when_every_command_fails_to_spawn() {
        if !pty_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let command = |id: &str| Command {
            id: id.into(),
            name: id.into(),
            cmd: "true".into(),
            cwd: dir.path().join("gone"),
            ..Default::default()
        };
        let config = CommandGroup {
            id: "root".into(),
            name: "root".into(),
            commands: vec![command("x"), command("y")],
            ..Default::default()
        };
        let mut app = App::new(config, dir.path().to_path_buf(), LogBuffer::new());
        app.selected.extend(ids(&["x", "y"]));

        app.run_selected(AREA);

        assert!(app.batch_run_ids.is_none(), "the batch never completed");
        assert_eq!(app.active_terminal_id.as_deref(), Some("x"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stale_exit_event_ignored() {
        if !pty_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let mut app = single_command_app(dir.path(), "exec sleep 30", dir.path().to_path_buf());

        app.run_command("a", AREA);
        let stale = exited(&app, "a", 1);
        app.run_command("a", AREA);
        // Exit of the first run, already queued when the restart happened
        app.handle_app_event(stale);

        assert_eq!(node_status(&mut app, "a"), CommandStatus::Running);
        app.handle_app_event(exited(&app, "a", 0));
        assert_eq!(node_status(&mut app, "a"), CommandStatus::Success);
        app.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shutdown_is_bounded() {
        if !pty_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let cmd = "trap '' HUP; touch ready; exec sleep 30";
        let mut app = single_command_app(dir.path(), cmd, dir.path().to_path_buf());
        app.run_command("a", AREA);
        assert!(wait_until(Duration::from_secs(5), || dir
            .path()
            .join("ready")
            .exists()));
        let term = std::sync::Arc::clone(&app.processes["a"].terminal);

        let shutdown = tokio::time::timeout(Duration::from_secs(3), app.shutdown()).await;
        assert!(
            shutdown.is_ok(),
            "shutdown hung on a command ignoring SIGHUP"
        );

        let exit = term
            .exit_info()
            .expect("shutdown returned before the command exited");
        assert_eq!(exit.signal, Some(libc::SIGKILL));
    }

    /// Leaves a HUP-ignoring `sleep 30` holding the PTY, with its pid in `pid_file`, then runs `tail`
    fn pty_holder(pid_file: &str, tail: &str) -> String {
        format!(
            "sh -c 'trap \"\" HUP; echo $$ > {pid_file}; exec sleep 30' & \
             while [ ! -s {pid_file} ]; do sleep 0.01; done; {tail}"
        )
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shutdown_kills_hup_ignoring_pty_holders() {
        if !pty_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let command = |id: &str, cmd: String| Command {
            id: id.into(),
            name: id.into(),
            cmd,
            cwd: dir.path().to_path_buf(),
            ..Default::default()
        };
        let config = CommandGroup {
            id: "root".into(),
            name: "root".into(),
            commands: vec![
                command("finished", pty_holder("finished.pid", "true")),
                command("running", pty_holder("running.pid", "exec sleep 30")),
            ],
            ..Default::default()
        };
        let mut app = App::new(config, dir.path().to_path_buf(), LogBuffer::new());
        app.run_commands(&ids(&["finished"]), AREA, None);
        app.run_commands(&ids(&["running"]), AREA, None);
        let finished = std::sync::Arc::clone(&app.processes["finished"].terminal);
        let running = std::sync::Arc::clone(&app.processes["running"].terminal);
        let has_pid =
            |name: &str| std::fs::metadata(dir.path().join(name)).is_ok_and(|m| m.len() > 0);
        assert!(wait_until(Duration::from_secs(5), || {
            finished.exit_info().is_some() && has_pid("running.pid")
        }));

        let shutdown = tokio::time::timeout(Duration::from_secs(3), app.shutdown()).await;
        assert!(shutdown.is_ok(), "shutdown hung on a PTY holder");

        // Escalation threads die with fnug, so shutdown must not return before holders are reaped
        assert!(
            finished.is_reaped(),
            "holder of a finished command survived"
        );
        assert!(running.is_reaped(), "holder of a running command survived");
    }
}
