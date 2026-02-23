use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use log::debug;
use ratatui::layout::Rect;

use super::app::{App, Focus};
use super::event::translate_key_event;
use super::tree_widget::NodeKind;

impl App {
    /// Handle keyboard input
    #[expect(
        clippy::too_many_lines,
        reason = "key handler covers all keyboard shortcuts in one match"
    )]
    pub fn handle_key(&mut self, key: KeyEvent, terminal_area: Rect) {
        // Global keys
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.should_quit = true;
            return;
        }

        if key.code == KeyCode::Char('r') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.fullscreen = !self.fullscreen;
            return;
        }

        // If terminal is focused and the active process is interactive, forward to PTY
        if matches!(self.focus, Focus::Terminal) {
            if key.code == KeyCode::Esc {
                self.focus = Focus::Tree;
                return;
            }
            if let Some(ref active_id) = self.active_terminal_id
                && let Some(proc) = self.processes.get(active_id)
            {
                if let Some(bytes) = translate_key_event(&key, proc.terminal.parser())
                    && let Err(e) = proc.terminal.write(bytes)
                {
                    debug!("Failed to write to terminal: {e}");
                }
                return;
            }
        }

        // Phase 1: Search editing mode — typing in search bar
        if self.search.is_editing() && matches!(self.focus, Focus::Tree) {
            match key.code {
                KeyCode::Enter => {
                    self.search.accept();
                    return;
                }
                KeyCode::Esc => {
                    self.search = super::app::SearchState::Inactive;
                    self.mark_tree_dirty();
                    return;
                }
                KeyCode::Backspace => {
                    self.search.pop_char();
                    self.mark_tree_dirty();
                    self.cursor = self.cursor.min(self.visible_nodes.len().saturating_sub(1));
                    return;
                }
                KeyCode::Char(c) => {
                    self.search.push_char(c);
                    self.mark_tree_dirty();
                    self.cursor = self.cursor.min(self.visible_nodes.len().saturating_sub(1));
                    return;
                }
                // Allow navigation keys to pass through
                KeyCode::Down | KeyCode::Up => {}
                _ => return,
            }
        }

        // Phase 2: Filter active but not editing — normal keys work, Esc clears filter
        if self.search.is_filtering() && matches!(self.focus, Focus::Tree) {
            match key.code {
                KeyCode::Char('/') => {
                    self.search.resume_editing();
                    return;
                }
                KeyCode::Esc => {
                    self.search = super::app::SearchState::Inactive;
                    self.mark_tree_dirty();
                    return;
                }
                // All other keys fall through to normal tree navigation
                _ => {}
            }
        }

        // Tree navigation
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => {
                self.should_quit = true;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if self.cursor + 1 < self.visible_nodes.len() {
                    self.cursor += 1;
                    self.update_active_terminal();
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                    self.update_active_terminal();
                }
            }
            KeyCode::Char('h') | KeyCode::Left => {
                if let Some(node) = self.visible_nodes.get(self.cursor) {
                    match &node.kind {
                        NodeKind::Group { expanded: true, .. } => {
                            self.expanded.insert(node.id.clone(), false);
                            self.mark_tree_dirty();
                        }
                        NodeKind::Command { selected: true, .. } => {
                            self.selected.insert(node.id.clone(), false);
                            self.mark_tree_dirty();
                        }
                        _ => {}
                    }
                }
            }
            KeyCode::Char('l') | KeyCode::Right => {
                if let Some(node) = self.visible_nodes.get(self.cursor) {
                    match &node.kind {
                        NodeKind::Group {
                            expanded: false, ..
                        } => {
                            self.expanded.insert(node.id.clone(), true);
                            self.mark_tree_dirty();
                        }
                        NodeKind::Command {
                            selected: false, ..
                        } => {
                            self.selected.insert(node.id.clone(), true);
                            self.mark_tree_dirty();
                        }
                        _ => {}
                    }
                }
            }
            KeyCode::Char(' ') => {
                self.toggle_current_node();
            }
            KeyCode::Char('g') => {
                // Git auto-select
                self.selected.clear();
                self.run_git_selection();
            }
            KeyCode::Enter => {
                self.run_selected(terminal_area);
            }
            KeyCode::Char('r') => {
                if let Some(id) = self.current_command_id() {
                    self.start_command(&id, terminal_area, true);
                } else if let Some(id) = self.current_group_id() {
                    self.run_group(&id, terminal_area);
                }
            }
            KeyCode::Char('s') => {
                if let Some(id) = self.current_command_id() {
                    self.stop_command(&id);
                }
            }
            KeyCode::Char('c') => {
                if let Some(id) = self.current_command_id() {
                    self.clear_command(&id);
                }
            }
            KeyCode::Char('/') => {
                self.search = super::app::SearchState::Editing(String::new());
            }
            KeyCode::Char('L') => {
                self.show_logs = !self.show_logs;
                self.log_scroll = 0;
            }
            KeyCode::Tab => {
                // Toggle focus to terminal if there's an active interactive command
                if self.active_terminal_id.is_some() {
                    self.focus = match self.focus {
                        Focus::Tree if self.active_command_is_interactive() => Focus::Terminal,
                        Focus::Terminal => Focus::Tree,
                        Focus::Tree => self.focus,
                    };
                }
            }
            _ => {}
        }
    }
}
