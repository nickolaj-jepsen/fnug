use std::io::Write;
use std::process::{Command as StdCommand, Stdio};
use std::sync::Arc;
use std::time::Instant;

use log::{debug, error, info, warn};
use ratatui::layout::Rect;

use crate::pty::terminal::{Terminal, TerminalSize};
use crate::pty::{format_failure_message, format_start_message, format_success_message};

use super::app::{App, AppEvent, CommandStatus, ProcessInstance};
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
        event_tx: &tokio::sync::mpsc::Sender<AppEvent>,
    ) -> tokio::task::JoinHandle<()> {
        let term = Arc::clone(term);
        let tx = event_tx.clone();
        let id = cmd_id.to_string();
        tokio::spawn(async move {
            match term.wait().await {
                Ok(code) => {
                    if code == 0 {
                        if let Err(e) = term.echo(format_success_message()) {
                            debug!("Failed to echo success message: {e}");
                        }
                    } else if let Err(e) = term.echo(format_failure_message(code)) {
                        debug!("Failed to echo failure message: {e}");
                    }
                    if let Err(e) = tx.send(AppEvent::ProcessExited(id, code)).await {
                        debug!("Failed to send process exit event: {e}");
                    }
                }
                Err(e) => {
                    let msg = format!("Process wait error: {e}");
                    let _ = tx.send(AppEvent::ProcessError(id, msg)).await;
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
                self.pending_deps.insert(cmd_id.to_string(), unresolved);
                if set_active {
                    self.active_terminal_id = Some(cmd_id.to_string());
                }
                self.mark_tree_dirty();
                return;
            }
        }

        info!("Starting command '{}'", cmd.name);

        // Kill existing process and abort its tasks
        if let Some(proc) = self.processes.remove(cmd_id) {
            proc.kill_and_abort(cmd_id);
        }

        let cols = terminal_area.width.max(2);
        let rows = terminal_area.height.max(2);
        let scrollback = cmd
            .scrollback
            .unwrap_or_else(Terminal::default_scrollback_size);

        match Terminal::new(&cmd, TerminalSize::new(cols, rows), scrollback) {
            Ok(terminal) => {
                if let Err(e) = terminal.echo(format_start_message(&cmd.cmd)) {
                    warn!("Failed to echo start message: {e}");
                }

                let term_ref = Arc::new(terminal);
                let exit_handle = Self::spawn_exit_watcher(&term_ref, cmd_id, &self.event_tx);

                self.processes.insert(
                    cmd_id.to_string(),
                    ProcessInstance {
                        terminal: term_ref,
                        status: CommandStatus::Running,
                        task_handles: vec![exit_handle],
                        started_at: Instant::now(),
                        finished_at: None,
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
                if set_active {
                    self.active_terminal_id = Some(cmd_id.to_string());
                }
            }
        }
        self.mark_tree_dirty();
    }

    /// Stop a command process
    pub fn stop_command(&mut self, cmd_id: &str) {
        info!("Stopping command '{cmd_id}'");
        if let Some(proc) = self.processes.get(cmd_id)
            && let Err(e) = proc.terminal.kill()
        {
            warn!("Failed to kill process '{cmd_id}': {e}");
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
            proc.kill_and_abort(cmd_id);
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

    fn exited(id: &str, code: u32) -> AppEvent {
        AppEvent::ProcessExited(id.into(), code)
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
        app.handle_app_event(exited("build", 1));
        assert_eq!(
            node_status(&mut app, "test"),
            CommandStatus::Error("Dependency 'build' failed".into())
        );

        app.start_command("test", AREA, true);
        app.handle_app_event(exited("build", 0));
        app.handle_app_event(exited("test", 0));

        assert_eq!(node_status(&mut app, "test"), CommandStatus::Success);
        assert!(!app.error_messages.contains_key("test"));
        app.shutdown();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn clear_command_drops_error_and_pending_deps() {
        if !pty_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let mut app = dep_app(dir.path());

        app.start_command("test", AREA, true);
        app.handle_app_event(exited("build", 1));
        app.clear_command("test");
        assert!(!app.error_messages.contains_key("test"));
        assert_eq!(node_status(&mut app, "test"), CommandStatus::Pending);

        app.start_command("test", AREA, true);
        assert!(app.pending_deps.contains_key("test"));
        app.clear_command("test");
        assert!(!app.pending_deps.contains_key("test"));

        // A cleared command must not be started when its dependency finishes
        app.handle_app_event(exited("build", 0));
        assert!(!app.processes.contains_key("test"));
        app.shutdown();
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

        app.handle_app_event(exited("a", 0));
        assert!(!app.processes.contains_key("b"));
        assert!(!app.processes.contains_key("c"));
        app.shutdown();
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
        app.shutdown();
    }
}
