use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use log::{debug, error};
use ratatui::layout::Rect;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::commands::command::Command;
use crate::commands::group::CommandGroup;
use crate::pty::terminal::Terminal;
use crate::selectors::get_selected_commands;

use super::log_state::LogBuffer;
use super::toolbar;
use super::tree_state::{TreeContext, find_command_in_group, find_group_in_group, flatten_group};
use super::tree_widget::{NodeKind, VisibleNode};

/// Execution status of a command
#[derive(Debug, Clone, PartialEq)]
pub enum CommandStatus {
    Pending,
    Running,
    Success,
    Failure(u32),
    Error(String),
    WaitingForDeps,
}

/// A running or completed process with its terminal and status
pub struct ProcessInstance {
    pub terminal: Arc<Terminal>,
    pub status: CommandStatus,
    pub(super) task_handles: Vec<JoinHandle<()>>,
    pub started_at: Instant,
    pub finished_at: Option<Instant>,
}

impl ProcessInstance {
    /// Kill the terminal process and abort all associated task handles.
    pub fn kill_and_abort(self, id: &str) {
        if let Err(e) = self.terminal.kill() {
            log::warn!("Failed to kill process '{id}': {e}");
        }
        for handle in self.task_handles {
            handle.abort();
        }
    }
}

/// Events dispatched to the main application loop
pub enum AppEvent {
    ProcessExited(String, u32),
    ProcessError(String, String),
    WatcherTriggered(Vec<Command>),
    LogUpdated,
    ConfigChanged,
}

/// Which pane currently has keyboard focus
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Tree,
    Terminal,
}

/// State machine for the search / filter bar
#[derive(Debug, Clone, Default)]
pub enum SearchState {
    #[default]
    Inactive,
    /// User is typing in the search bar
    Editing(String),
    /// Filter applied, navigating results
    Active(String),
}

impl SearchState {
    /// The current query string, if any.
    #[must_use]
    pub fn query(&self) -> Option<&str> {
        match self {
            SearchState::Inactive => None,
            SearchState::Editing(q) | SearchState::Active(q) => Some(q),
        }
    }

    /// Whether the user is actively typing in the search bar.
    #[must_use]
    pub fn is_editing(&self) -> bool {
        matches!(self, SearchState::Editing(_))
    }

    /// Whether a filter is applied but the user is not editing.
    #[must_use]
    pub fn is_filtering(&self) -> bool {
        matches!(self, SearchState::Active(_))
    }

    /// Whether any search/filter is active (editing or applied).
    #[must_use]
    pub fn has_query(&self) -> bool {
        !matches!(self, SearchState::Inactive)
    }

    /// Append a character (only meaningful while editing).
    pub fn push_char(&mut self, c: char) {
        if let SearchState::Editing(q) = self {
            q.push(c);
        }
    }

    /// Remove the last character (only meaningful while editing).
    pub fn pop_char(&mut self) {
        if let SearchState::Editing(q) = self {
            q.pop();
        }
    }

    /// Accept the current query: transition Editing → Active.
    pub fn accept(&mut self) {
        if let SearchState::Editing(q) = self {
            *self = SearchState::Active(std::mem::take(q));
        }
    }

    /// Resume editing: transition Active → Editing.
    pub fn resume_editing(&mut self) {
        if let SearchState::Active(q) = self {
            *self = SearchState::Editing(std::mem::take(q));
        }
    }
}

/// Mouse interaction state (drag, double-click, hover)
#[derive(Debug, Default)]
pub struct MouseState {
    /// Is tree panel being resized via drag?
    pub resizing: bool,
    /// Is scrollbar being dragged?
    pub scrollbar_dragging: bool,
    /// Last click timestamp and row for double-click detection
    pub last_click: Option<(Instant, usize)>,
    /// Currently hovered tree row
    pub hover_row: Option<usize>,
}

/// Cached toolbar layout from the last render
#[derive(Debug)]
pub struct ToolbarCache {
    /// Toolbar shortcut hit regions
    pub regions: Vec<toolbar::ToolbarRegion>,
    /// Toolbar row (y coordinate)
    pub y: u16,
    /// Currently hovered toolbar shortcut index
    pub hover: Option<usize>,
}

impl Default for ToolbarCache {
    fn default() -> Self {
        Self {
            regions: Vec::new(),
            y: u16::MAX,
            hover: None,
        }
    }
}

/// Main application state for the TUI
#[expect(
    clippy::struct_excessive_bools,
    reason = "4 independent boolean flags (fullscreen, should_quit, tree_dirty, show_logs) are reasonable for TUI state"
)]
pub struct App {
    pub config: CommandGroup,
    pub cwd: PathBuf,
    pub config_path: PathBuf,
    pub visible_nodes: Vec<VisibleNode>,
    pub cursor: usize,
    pub processes: HashMap<String, ProcessInstance>,
    pub active_terminal_id: Option<String>,
    pub fullscreen: bool,
    pub tree_width: u16,
    pub event_tx: mpsc::Sender<AppEvent>,
    pub event_rx: mpsc::Receiver<AppEvent>,
    pub should_quit: bool,
    pub focus: Focus,
    /// Track which groups are expanded (by group id)
    pub(super) expanded: HashMap<String, bool>,
    /// Track which commands are selected (by command id)
    pub(super) selected: HashMap<String, bool>,
    /// Mouse interaction state
    pub mouse: MouseState,
    /// Cached toolbar layout
    pub toolbar: ToolbarCache,
    /// Error messages for commands that failed to start
    pub(super) error_messages: HashMap<String, String>,
    /// Whether the `visible_nodes` list needs rebuilding
    pub(super) tree_dirty: bool,
    /// Scroll offset for the tree panel (first visible row index)
    pub tree_scroll: usize,
    /// Whether the log panel is shown instead of the terminal
    pub show_logs: bool,
    /// Ring buffer of log entries
    pub log_buffer: LogBuffer,
    /// Scroll offset for the log panel (0 = bottom / newest)
    pub log_scroll: usize,
    /// Search / filter bar state
    pub search: SearchState,
    /// Commands waiting for dependencies: `cmd_id` -> remaining dep IDs
    pub(super) pending_deps: HashMap<String, Vec<String>>,
}

/// Recursively collect IDs of groups that contain no selected commands.
/// Skips the root group (called on children directly).
fn collect_inactive_groups(
    group: &CommandGroup,
    selected: &HashMap<String, bool>,
    out: &mut Vec<String>,
) {
    for child in &group.children {
        let has_selected = child
            .all_commands()
            .iter()
            .any(|cmd| *selected.get(&cmd.id).unwrap_or(&false));
        if !has_selected {
            out.push(child.id.clone());
        }
        collect_inactive_groups(child, selected, out);
    }
}

impl App {
    #[must_use]
    pub fn new(
        config: CommandGroup,
        cwd: PathBuf,
        config_path: PathBuf,
        log_buffer: LogBuffer,
    ) -> Self {
        let (event_tx, event_rx) = mpsc::channel(256);
        let mut app = App {
            config,
            cwd,
            config_path,
            visible_nodes: Vec::new(),
            cursor: 0,
            processes: HashMap::new(),
            active_terminal_id: None,
            fullscreen: false,
            tree_width: 30,
            event_tx,
            event_rx,
            should_quit: false,
            focus: Focus::Tree,
            expanded: HashMap::new(),
            selected: HashMap::new(),
            mouse: MouseState::default(),
            toolbar: ToolbarCache::default(),
            error_messages: HashMap::new(),
            tree_dirty: false,
            tree_scroll: 0,
            show_logs: false,
            log_buffer,
            log_scroll: 0,
            search: SearchState::Inactive,
            pending_deps: HashMap::new(),
        };
        app.rebuild_visible_nodes();
        app
    }

    /// Apply results from a headless check run: mark failed commands as
    /// selected and auto-start them so the user sees PTY output immediately.
    pub fn apply_check_result(
        &mut self,
        result: &crate::check::CheckResult,
        terminal_area: ratatui::layout::Rect,
    ) {
        // Mark only the failed commands as selected
        for id in &result.selected_ids {
            let is_failed = result.failed_ids.contains(id);
            self.selected.insert(id.clone(), is_failed);
        }
        self.collapse_inactive_groups();
        self.rebuild_visible_nodes();

        // Move cursor to the first failed command in the visible tree
        if let Some(first_failed) = self
            .visible_nodes
            .iter()
            .position(|n| result.failed_ids.contains(&n.id))
        {
            self.cursor = first_failed;
        }

        // Auto-start the failed commands (deps are handled by start_command)
        for id in &result.failed_ids {
            self.start_command(id, terminal_area, true);
        }
    }

    /// Run initial git selection
    pub fn run_git_selection(&mut self) {
        let commands: Vec<Command> = self.config.all_commands().into_iter().cloned().collect();
        match get_selected_commands(commands) {
            Ok(selected) => {
                for cmd in &selected {
                    self.selected.insert(cmd.id.clone(), true);
                }
                debug!("Git-selected {} commands", selected.len());
            }
            Err(e) => {
                error!("Git selection failed: {e}");
            }
        }
        self.collapse_inactive_groups();
        self.rebuild_visible_nodes();
    }

    /// Collapse groups that contain no selected commands, skipping the root.
    fn collapse_inactive_groups(&mut self) {
        let mut to_collapse = Vec::new();
        collect_inactive_groups(&self.config, &self.selected, &mut to_collapse);
        for id in to_collapse {
            self.expanded.insert(id, false);
        }
    }

    /// Mark the tree as needing a rebuild (lazy, happens at next render)
    pub fn mark_tree_dirty(&mut self) {
        self.tree_dirty = true;
    }

    /// Rebuild the flat `visible_nodes` list from the config tree
    pub fn rebuild_visible_nodes(&mut self) {
        self.visible_nodes.clear();
        let mut ctx = TreeContext {
            expanded: &self.expanded,
            selected: &self.selected,
            processes: &self.processes,
            error_messages: &self.error_messages,
            nodes: &mut self.visible_nodes,
            filter: self.search.query(),
        };
        flatten_group(&self.config, 0, true, &[], &mut ctx);
        self.tree_dirty = false;
    }

    /// Find a command by id in the config tree
    #[must_use]
    pub fn find_command(&self, id: &str) -> Option<Command> {
        find_command_in_group(&self.config, id)
    }

    /// Handle app events (called from event loop)
    pub fn handle_app_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::ProcessExited(cmd_id, exit_code) => {
                if let Some(proc) = self.processes.get_mut(&cmd_id) {
                    proc.finished_at = Some(Instant::now());
                    proc.status = if exit_code == 0 {
                        CommandStatus::Success
                    } else {
                        CommandStatus::Failure(exit_code)
                    };
                }
                if exit_code == 0 {
                    self.selected.insert(cmd_id.clone(), false);
                    // Check pending deps - start commands whose deps are now satisfied
                    self.resolve_dependency(&cmd_id);
                } else {
                    // Propagate failure to dependents
                    self.fail_dependents(&cmd_id);
                }
                self.mark_tree_dirty();
            }
            AppEvent::ProcessError(cmd_id, msg) => {
                error!("Process error for '{cmd_id}': {msg}");
                if let Some(proc) = self.processes.get_mut(&cmd_id) {
                    proc.finished_at = Some(Instant::now());
                    proc.status = CommandStatus::Error(msg.clone());
                }
                self.error_messages.insert(cmd_id.clone(), msg);
                self.fail_dependents(&cmd_id);
                self.mark_tree_dirty();
            }
            AppEvent::WatcherTriggered(commands) => {
                for cmd in &commands {
                    self.selected.insert(cmd.id.clone(), true);
                }
                self.mark_tree_dirty();
                // Auto-run triggered commands
                let terminal_area = Rect::new(0, 0, 80, 24); // will be corrected on next render
                for cmd in commands {
                    self.start_command(&cmd.id, terminal_area, false);
                }
            }
            AppEvent::LogUpdated => {
                // Redraw happens automatically on next frame
            }
            AppEvent::ConfigChanged => {
                self.reload_config();
            }
        }
    }

    /// Reload configuration from disk, preserving running processes
    fn reload_config(&mut self) {
        use crate::load_config;
        use log::info;

        let config_str = self.config_path.to_string_lossy().to_string();
        match load_config(Some(&config_str)) {
            Ok((new_config, new_cwd, _)) => {
                // Collect IDs that still exist in new config
                let new_ids: std::collections::HashSet<String> = new_config
                    .all_commands()
                    .into_iter()
                    .map(|c| c.id.clone())
                    .collect();

                // Remove processes for deleted commands
                self.processes.retain(|id, _| new_ids.contains(id));
                self.error_messages.retain(|id, _| new_ids.contains(id));
                self.expanded.retain(|id, _| new_ids.contains(id));
                self.selected.retain(|id, _| new_ids.contains(id));
                self.pending_deps.retain(|id, _| new_ids.contains(id));

                self.config = new_config;
                self.cwd = new_cwd;
                self.mark_tree_dirty();
                info!("Configuration reloaded successfully");
            }
            Err(e) => {
                error!("Failed to reload config: {e}");
            }
        }
    }

    /// Remove a satisfied dependency and start commands whose deps are all clear
    fn resolve_dependency(&mut self, completed_id: &str) {
        let mut ready = Vec::new();
        for (cmd_id, deps) in &mut self.pending_deps {
            deps.retain(|d| d != completed_id);
            if deps.is_empty() {
                ready.push(cmd_id.clone());
            }
        }
        for cmd_id in ready {
            self.pending_deps.remove(&cmd_id);
            let terminal_area = ratatui::layout::Rect::new(0, 0, 80, 24);
            self.start_command(&cmd_id, terminal_area, false);
        }
    }

    /// Propagate failure to commands waiting on a failed dependency
    fn fail_dependents(&mut self, failed_id: &str) {
        let failed_id_owned = failed_id.to_string();
        let dependents: Vec<String> = self
            .pending_deps
            .keys()
            .filter(|cmd_id| {
                self.pending_deps
                    .get(*cmd_id)
                    .is_some_and(|deps| deps.contains(&failed_id_owned))
            })
            .cloned()
            .collect();
        for cmd_id in dependents {
            self.pending_deps.remove(&cmd_id);
            let failed_name = self
                .find_command(failed_id)
                .map_or_else(|| failed_id.to_string(), |c| c.name.clone());
            let msg = format!("Dependency '{failed_name}' failed");
            self.error_messages.insert(cmd_id, msg);
        }
    }

    /// Shut down all processes and abort all spawned tasks
    pub fn shutdown(&mut self) {
        for (id, proc) in self.processes.drain() {
            proc.kill_and_abort(&id);
        }
    }

    /// Check if any running terminal has new output that needs rendering
    #[must_use]
    pub fn any_terminal_dirty(&self) -> bool {
        self.processes.values().any(|p| p.terminal.is_dirty())
    }

    /// Clear dirty flags on all terminals (call after rendering)
    pub fn clear_terminal_dirty(&self) {
        for proc in self.processes.values() {
            proc.terminal.clear_dirty();
        }
    }

    /// Whether the currently active terminal is interactive (using alternate screen)
    #[must_use]
    pub fn active_command_is_interactive(&self) -> bool {
        self.active_terminal_id
            .as_ref()
            .and_then(|id| self.processes.get(id))
            .is_some_and(|proc| proc.terminal.parser().lock().screen().alternate_screen())
    }

    /// Adjust `tree_scroll` so the cursor row is visible within the given height
    pub fn ensure_cursor_visible(&mut self, height: usize) {
        if height == 0 {
            return;
        }
        if self.cursor < self.tree_scroll {
            self.tree_scroll = self.cursor;
        } else if self.cursor >= self.tree_scroll + height {
            self.tree_scroll = self.cursor - height + 1;
        }
    }

    /// Toggle the current node: expand/collapse for groups, select/deselect for commands.
    pub(super) fn toggle_current_node(&mut self) {
        if let Some(node) = self.visible_nodes.get(self.cursor) {
            match &node.kind {
                NodeKind::Group { .. } => {
                    if let Some(group) = find_group_in_group(&self.config, &node.id) {
                        let cmd_ids: Vec<String> =
                            group.all_commands().iter().map(|c| c.id.clone()).collect();
                        let all_selected = cmd_ids
                            .iter()
                            .all(|id| *self.selected.get(id).unwrap_or(&false));
                        let new_state = !all_selected;
                        for id in cmd_ids {
                            self.selected.insert(id, new_state);
                        }
                        // Keep expand/collapse in sync: expand when selecting, collapse when deselecting
                        self.expanded.insert(node.id.clone(), new_state);
                        self.mark_tree_dirty();
                    }
                }
                NodeKind::Command { selected, .. } => {
                    self.selected.insert(node.id.clone(), !selected);
                    self.mark_tree_dirty();
                }
            }
        }
    }

    /// Return the command id at the current cursor position, if it's a command node.
    pub(super) fn current_command_id(&self) -> Option<String> {
        self.visible_nodes
            .get(self.cursor)
            .and_then(|node| matches!(node.kind, NodeKind::Command { .. }).then(|| node.id.clone()))
    }

    /// Return the group id at the current cursor position, if it's a group node.
    pub(super) fn current_group_id(&self) -> Option<String> {
        self.visible_nodes
            .get(self.cursor)
            .and_then(|node| matches!(node.kind, NodeKind::Group { .. }).then(|| node.id.clone()))
    }

    pub(super) fn update_active_terminal(&mut self) {
        if let Some(id) = self.current_command_id()
            && (self.processes.contains_key(&id) || self.pending_deps.contains_key(&id))
        {
            self.active_terminal_id = Some(id);
        }
    }

    pub(super) fn execute_toolbar_action(
        &mut self,
        action: toolbar::ToolbarAction,
        terminal_area: Rect,
    ) {
        use toolbar::ToolbarAction;
        match action {
            ToolbarAction::RunSelected => self.run_selected(terminal_area),
            ToolbarAction::ToggleSpace => {
                self.toggle_current_node();
            }
            ToolbarAction::Run => {
                if let Some(id) = self.current_command_id() {
                    self.start_command(&id, terminal_area, true);
                } else if let Some(id) = self.current_group_id() {
                    self.run_group(&id, terminal_area);
                }
            }
            ToolbarAction::Stop => {
                if let Some(id) = self.current_command_id() {
                    self.stop_command(&id);
                }
            }
            ToolbarAction::Clear => {
                if let Some(id) = self.current_command_id() {
                    self.clear_command(&id);
                }
            }
            ToolbarAction::GitSelect => {
                self.selected.clear();
                self.run_git_selection();
            }
            ToolbarAction::ToggleFullscreen => {
                self.fullscreen = !self.fullscreen;
            }
            ToolbarAction::FocusTerminal => {
                if self.active_terminal_id.is_some() && self.active_command_is_interactive() {
                    self.focus = Focus::Terminal;
                }
            }
            ToolbarAction::Quit => {
                self.should_quit = true;
            }
            ToolbarAction::BackToTree => {
                self.focus = Focus::Tree;
                self.fullscreen = false;
            }
            ToolbarAction::ToggleLogs => {
                self.show_logs = !self.show_logs;
                self.log_scroll = 0;
            }
            ToolbarAction::Search => {
                self.search = SearchState::Editing(String::new());
            }
            ToolbarAction::ClearSearch => {
                self.search = SearchState::Inactive;
                self.mark_tree_dirty();
            }
            ToolbarAction::AcceptSearch => {
                self.search.accept();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::log_state::LogBuffer;
    use crate::tui::tree_widget::render_node_text;

    fn cmd(id: &str, name: &str) -> Command {
        Command {
            id: id.to_string(),
            name: name.to_string(),
            ..Default::default()
        }
    }

    fn group(
        id: &str,
        name: &str,
        children: Vec<CommandGroup>,
        commands: Vec<Command>,
    ) -> CommandGroup {
        CommandGroup {
            id: id.to_string(),
            name: name.to_string(),
            children,
            commands,
            ..Default::default()
        }
    }

    /// Build a config tree similar to .fnug.yaml:
    ///
    /// fnug (root)
    /// ├─ rust (child group, 2 commands)
    /// │  ├─ fmt
    /// │  └─ clippy
    /// └─ debug (child group, 2 child groups + 2 commands)
    ///    ├─ nested-auto (child group, 2 commands)
    ///    │  ├─ test-auto
    ///    │  └─ test-not-auto
    ///    ├─ not-expanded (child group, 1 command)
    ///    │  └─ test-not-expanded
    ///    ├─ htop
    ///    └─ recursive
    fn make_test_tree() -> CommandGroup {
        let nested_auto = group(
            "nested-auto",
            "nested-auto",
            vec![],
            vec![
                cmd("test-auto", "test-auto"),
                cmd("test-not-auto", "test-not-auto"),
            ],
        );
        let not_expanded = group(
            "not-expanded",
            "not-expanded",
            vec![],
            vec![cmd("test-not-expanded", "test-not-expanded")],
        );
        let debug = group(
            "debug",
            "debug",
            vec![nested_auto, not_expanded],
            vec![cmd("htop", "htop"), cmd("recursive", "recursive")],
        );
        let rust = group(
            "rust",
            "rust",
            vec![],
            vec![cmd("fmt", "fmt"), cmd("clippy", "clippy")],
        );
        group("root", "fnug", vec![rust, debug], vec![])
    }

    #[test]
    fn test_flatten_group_renders_correct_tree() {
        let config = make_test_tree();
        let app = App::new(config, PathBuf::new(), PathBuf::new(), LogBuffer::new());

        let lines: Vec<String> = app.visible_nodes.iter().map(render_node_text).collect();
        let expected = vec![
            "▼ fnug (0/7)",
            "├─▼ rust (0/2)",
            "│ ├─○ fmt",
            "│ └─○ clippy",
            "└─▼ debug (0/5)",
            "  ├─▼ nested-auto (0/2)",
            "  │ ├─○ test-auto",
            "  │ └─○ test-not-auto",
            "  ├─▼ not-expanded (0/1)",
            "  │ └─○ test-not-expanded",
            "  ├─○ htop",
            "  └─○ recursive",
        ];
        assert_eq!(lines, expected);
    }
}
