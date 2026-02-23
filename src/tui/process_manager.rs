use log::{debug, error, info, warn};
use ratatui::layout::Rect;
use std::sync::Arc;
use std::time::Instant;

use crate::pty::terminal::{Terminal, TerminalSize};
use crate::pty::{format_failure_message, format_start_message, format_success_message};

use super::app::{App, AppEvent, CommandStatus, ProcessInstance};
use super::tree_state::find_group_in_group;

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
        let Some(cmd) = self.find_command(cmd_id) else {
            return;
        };

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

    /// Clear a command's terminal
    pub fn clear_command(&mut self, cmd_id: &str) {
        if let Some(proc) = self.processes.remove(cmd_id) {
            if let Err(e) = proc.terminal.clear() {
                warn!("Failed to clear terminal '{cmd_id}': {e}");
            }
            for handle in proc.task_handles {
                handle.abort();
            }
        }
        self.mark_tree_dirty();
    }

    /// Start all selected commands (deps are handled by `start_command`)
    pub fn run_selected(&mut self, terminal_area: Rect) {
        let selected_ids: Vec<String> = self
            .selected
            .iter()
            .filter(|(_, v)| **v)
            .map(|(k, _)| k.clone())
            .collect();

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
