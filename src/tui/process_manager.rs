use std::io::Write;
use std::process::{Command as StdCommand, Stdio};
use std::sync::Arc;
use std::time::Instant;

use log::{debug, error, info, warn};
use ratatui::layout::Rect;

use crate::process::StopSignal;
use crate::pty::terminal::{Terminal, TerminalOptions, TerminalSize};
use crate::pty::{format_exit_message, format_start_message};

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

    /// Start a command process (checks dependencies first).
    ///
    /// When `set_active` is true the terminal pane switches to this command.
    /// Pass `false` for background starts (dependency resolution, watcher
    /// triggers) so the user's current view is not disrupted.
    pub fn start_command(&mut self, cmd_id: &str, terminal_area: Rect, set_active: bool) {
        self.last_terminal_area = terminal_area;

        let Some(mut cmd) = self.find_command(cmd_id) else {
            return;
        };
        cmd.cwd = cmd.effective_cwd(&self.cwd).to_path_buf();
        self.error_messages.remove(cmd_id);

        // Check dependencies
        if !cmd.depends_on.is_empty() {
            let mut unresolved = Vec::new();
            for dep_id in &cmd.depends_on {
                match self.processes.get(dep_id).map(|p| &p.status) {
                    Some(CommandStatus::Success) => {} // dep satisfied
                    _ => unresolved.push(dep_id.clone()),
                }
            }
            if !unresolved.is_empty() {
                info!(
                    "Command '{}' waiting for {} dependencies",
                    cmd.name,
                    unresolved.len()
                );
                // Queued first, so a dependency that fails to start fails this command too
                self.pending_deps
                    .insert(cmd_id.to_string(), unresolved.clone());
                // Start unresolved deps that aren't running or pending
                for dep_id in &unresolved {
                    if !self
                        .processes
                        .get(dep_id)
                        .is_some_and(|p| matches!(p.status, CommandStatus::Running))
                        && !self.pending_deps.contains_key(dep_id)
                    {
                        self.start_command(dep_id, terminal_area, false);
                    }
                }
                if set_active {
                    self.active_terminal_id = Some(cmd_id.to_string());
                }
                self.mark_tree_dirty();
                return;
            }
        }

        info!("Starting command '{}'", cmd.name);

        // Stop the previous run and abort its tasks
        if let Some(proc) = self.processes.remove(cmd_id) {
            proc.stop_and_abort(cmd_id, StopSignal::Interrupt);
        }

        let cols = terminal_area.width.max(2);
        let rows = terminal_area.height.max(2);
        let opts = TerminalOptions {
            scrollback: cmd
                .scrollback
                .unwrap_or_else(Terminal::default_scrollback_size),
            output_notify: None,
        };

        match Terminal::new(&cmd, TerminalSize::new(cols, rows), opts) {
            Ok(terminal) => {
                if let Err(e) = terminal.echo(format_start_message(&cmd.cmd)) {
                    warn!("Failed to echo start message: {e}");
                }

                self.next_generation += 1;
                let generation = self.next_generation;
                let term_ref = Arc::new(terminal);
                let exit_handle =
                    Self::spawn_exit_watcher(&term_ref, cmd_id, generation, &self.event_tx);

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

                if set_active {
                    self.active_terminal_id = Some(cmd_id.to_string());
                }
            }
            Err(e) => {
                let msg = format!("Failed to start '{}': {}", cmd.name, e);
                error!("{msg}");
                self.error_messages.insert(cmd_id.to_string(), msg);
                self.fail_dependents(cmd_id);
                if set_active {
                    self.active_terminal_id = Some(cmd_id.to_string());
                }
            }
        }
        self.mark_tree_dirty();
    }

    /// Stop a command process and drop its queued run (and its dependents'), if any
    pub fn stop_command(&mut self, cmd_id: &str) {
        info!("Stopping command '{cmd_id}'");
        if self.pending_deps.remove(cmd_id).is_some() {
            self.cancel_dependents(cmd_id);
        }
        // A queued rerun can wait behind a previous run that is still going
        if let Some(proc) = self.processes.get(cmd_id)
            && let Err(e) = proc.terminal.stop(StopSignal::Interrupt, STOP_GRACE)
        {
            warn!("Failed to stop process '{cmd_id}': {e}");
        }
        self.mark_tree_dirty();
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

    /// Clear a command's terminal, error and queued run, and kill the process if running
    pub fn clear_command(&mut self, cmd_id: &str) {
        if let Some(proc) = self.processes.remove(cmd_id) {
            proc.stop_and_abort(cmd_id, StopSignal::Interrupt);
        }
        self.error_messages.remove(cmd_id);
        if self.pending_deps.remove(cmd_id).is_some() {
            self.cancel_dependents(cmd_id);
        }
        self.mark_tree_dirty();
    }

    /// Start all selected commands (deps are handled by `start_command`)
    pub fn run_selected(&mut self, terminal_area: Rect) {
        let selected_ids: Vec<String> = self.selected.iter().cloned().collect();

        // Track batch for auto-focus on failure
        self.batch_run_ids = Some(selected_ids.iter().cloned().collect());

        info!("Running {} selected commands", selected_ids.len());
        for id in &selected_ids {
            self.start_command(id, terminal_area, false);
        }
        // Set active terminal to the command at the cursor, or the first
        // started command if the cursor isn't on a started command.
        if self.current_command_id().is_some_and(|id| {
            self.processes.contains_key(&id) || self.pending_deps.contains_key(&id)
        }) {
            self.update_active_terminal();
        } else if let Some(first) = selected_ids.first() {
            self.active_terminal_id = Some(first.clone());
        }
    }

    /// Start all commands in a group (and nested subgroups).
    pub fn run_group(&mut self, group_id: &str, terminal_area: Rect) {
        let Some(group) = find_group_in_group(&self.config, group_id) else {
            return;
        };
        let cmd_ids: Vec<String> = group.all_commands().iter().map(|c| c.id.clone()).collect();
        info!("Running {} commands in group '{group_id}'", cmd_ids.len());
        let mut first = true;
        for id in &cmd_ids {
            self.start_command(id, terminal_area, first);
            first = false;
        }
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

    #[tokio::test(flavor = "multi_thread")]
    async fn sticky_dep_error_cleared_on_rerun() {
        if !pty_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let mut app = dep_app(dir.path());

        app.start_command("test", AREA, true);
        app.handle_app_event(exited(&app, "build", 1));
        assert_eq!(
            node_status(&mut app, "test"),
            CommandStatus::Error("Dependency 'build' failed".into())
        );

        app.start_command("test", AREA, true);
        app.handle_app_event(exited(&app, "build", 0));
        app.handle_app_event(exited(&app, "test", 0));

        assert_eq!(node_status(&mut app, "test"), CommandStatus::Success);
        assert!(!app.error_messages.contains_key("test"));
        app.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn clear_command_drops_error_and_pending_deps() {
        if !pty_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let mut app = dep_app(dir.path());

        app.start_command("test", AREA, true);
        app.handle_app_event(exited(&app, "build", 1));
        app.clear_command("test");
        assert!(!app.error_messages.contains_key("test"));
        assert_eq!(node_status(&mut app, "test"), CommandStatus::Pending);

        app.start_command("test", AREA, true);
        assert!(app.pending_deps.contains_key("test"));
        app.clear_command("test");
        assert!(!app.pending_deps.contains_key("test"));

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

        app.start_command("c", AREA, true);
        assert!(app.pending_deps.contains_key("b"));
        assert!(app.pending_deps.contains_key("c"));

        app.clear_command("b");
        assert!(!app.pending_deps.contains_key("c"), "c still waits on b");
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

        app.start_command("a", AREA, true);
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

        app.start_command("dependent", AREA, true);

        assert!(app.error_messages.contains_key("gone"));
        assert!(
            !app.pending_deps.contains_key("dependent"),
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

        app.start_command("a", AREA, true);
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

        app.start_command("b", AREA, true);
        assert!(app.pending_deps.contains_key("b"));
        assert!(wait_until(Duration::from_secs(5), || dir
            .path()
            .join("ready")
            .exists()));

        app.stop_command("a");
        let event = next_event(&mut app).await;
        app.handle_app_event(event);

        assert_eq!(node_status(&mut app, "a"), CommandStatus::Stopped);
        assert!(app.processes["a"].exit.as_ref().unwrap().stop_requested);
        assert!(!app.pending_deps.contains_key("b"), "b still waits on a");
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

        app.start_command("b", AREA, true);
        app.stop_command("b");

        assert!(!app.pending_deps.contains_key("b"));
        assert!(
            app.processes["a"].terminal.is_running(),
            "stopped the dependency"
        );
        app.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stop_reaches_run_still_going_behind_queued_rerun() {
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
        app.start_command("b", AREA, true);
        app.handle_app_event(exited(&app, "a", 0));
        assert!(wait_until(Duration::from_secs(5), || dir
            .path()
            .join("ready")
            .exists()));
        // Restarting the dependency queues the rerun of `b` while its first run keeps going
        app.start_command("a", AREA, false);
        app.start_command("b", AREA, true);
        assert!(app.pending_deps.contains_key("b"));
        let first_run = std::sync::Arc::clone(&app.processes["b"].terminal);

        app.stop_command("b");

        assert!(!app.pending_deps.contains_key("b"));
        let exit = tokio::time::timeout(Duration::from_secs(3), first_run.wait()).await;
        let exit = exit.expect("running instance was not stopped").unwrap();
        assert!(exit.stop_requested);
        app.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stale_exit_event_ignored() {
        if !pty_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let mut app = single_command_app(dir.path(), "exec sleep 30", dir.path().to_path_buf());

        app.start_command("a", AREA, true);
        let stale = exited(&app, "a", 1);
        app.start_command("a", AREA, true);
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
        app.start_command("a", AREA, true);
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
        app.start_command("finished", AREA, false);
        app.start_command("running", AREA, false);
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
