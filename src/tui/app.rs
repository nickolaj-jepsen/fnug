use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use log::{debug, error, info, warn};
use ratatui::layout::Rect;
use tokio::sync::{Notify, mpsc};
use tokio::task::JoinHandle;

use crate::commands::command::Command;
use crate::commands::group::CommandGroup;
use crate::process::{ExitInfo, StopSignal};
use crate::pty::terminal::Terminal;
use crate::runner::DagState;
use crate::selectors::watch::WatchMatch;
use crate::selectors::{self, SelectOptions};

use super::context_menu::{ContextMenu, ContextMenuAction, ContextMenuTarget};
use super::log_state::LogBuffer;
use super::run_summary::{RunRecord, RunSummary};
use super::status::{StatusLevel, StatusMessage};
use super::toolbar;
use super::tree_state::{TreeContext, find_command_in_group, find_group_in_group, flatten_group};
use super::tree_widget::{NodeKind, VisibleNode};

/// Execution status of a command
#[derive(Debug, Clone, PartialEq)]
pub enum CommandStatus {
    Pending,
    Running,
    Success,
    /// Exit code, or 128 plus the signal number
    Failure(u32),
    Error(String),
    WaitingForDeps,
    /// Exited after the user stopped it
    Stopped,
}

/// A running or completed process with its terminal and status
///
/// Kept after the process exits until it is rerun or cleared: scrollback and copy read the
/// terminal's parser, and the idle terminal threads cost next to nothing.
pub struct ProcessInstance {
    pub terminal: Arc<Terminal>,
    pub status: CommandStatus,
    pub(super) task_handles: Vec<JoinHandle<()>>,
    pub started_at: Instant,
    pub finished_at: Option<Instant>,
    /// How the process ended, once it has
    pub exit: Option<ExitInfo>,
    /// Distinguishes this run's events from those of earlier runs of the same command
    pub generation: u64,
}

/// How long a stopped (or restarted, or cleared) command gets before it is killed
pub const STOP_GRACE: Duration = Duration::from_secs(2);
/// How long commands get to exit after `SIGHUP` when fnug quits
pub const QUIT_GRACE: Duration = Duration::from_secs(1);

impl ProcessInstance {
    /// Stop the command's process group with `sig` (escalating after [`STOP_GRACE`]) and abort
    /// the associated tasks. Sends nothing once the command has been reaped.
    pub fn stop_and_abort(self, id: &str, sig: StopSignal) {
        if let Err(e) = self.terminal.stop(sig, STOP_GRACE) {
            log::warn!("Failed to stop process '{id}': {e}");
        }
        for handle in self.task_handles {
            handle.abort();
        }
    }
}

/// Events dispatched to the main application loop
pub enum AppEvent {
    ProcessExited {
        id: String,
        generation: u64,
        exit: ExitInfo,
    },
    ProcessError {
        id: String,
        generation: u64,
        message: String,
    },
    WatcherTriggered(Vec<WatchMatch>),
    LogUpdated,
    GitSelectionComplete(u64, Vec<Command>),
    /// Show a message in the toolbar
    Status {
        text: String,
        level: StatusLevel,
    },
    /// A clipboard program finished copying `lines` lines. On failure, `fallback` holds the
    /// text to copy with OSC 52 instead.
    ClipboardResult {
        lines: usize,
        result: Result<&'static str, String>,
        fallback: Option<String>,
    },
}

/// Output for the terminal fnug runs in, written after the next draw
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outbound {
    /// Put the text on the clipboard with OSC 52
    Clipboard(String),
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
    reason = "5 independent boolean flags (fullscreen, should_quit, tree_dirty, show_logs, show_help) are reasonable for TUI state"
)]
pub struct App {
    pub config: CommandGroup,
    pub cwd: PathBuf,
    pub visible_nodes: Vec<VisibleNode>,
    /// Row of the cursor in `visible_nodes`; set it through `set_cursor_index`
    pub cursor: usize,
    /// Id of the node under the cursor, which it stays on when rows are rebuilt
    pub(super) cursor_id: Option<String>,
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
    /// Groups the user expanded or collapsed by hand, which selection changes leave alone
    pub(super) user_expansion: HashSet<String>,
    /// Track which commands are selected (by command id)
    pub(super) selected: HashSet<String>,
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
    /// Cursor row and tree height when the tree last scrolled to the cursor. Other frames
    /// leave the scroll alone, so the mouse wheel can move the cursor out of view.
    pub(super) last_scroll_anchor: Option<(usize, usize)>,
    /// Whether the log panel is shown instead of the terminal
    pub show_logs: bool,
    /// Ring buffer of log entries
    pub log_buffer: LogBuffer,
    /// Scroll offset for the log panel (0 = bottom / newest)
    pub log_scroll: usize,
    /// The log buffer's issue count when the log panel last showed
    pub(super) seen_log_issues: usize,
    /// Unseen warnings and errors as of the last log update, to redraw when the count changes
    log_badge: usize,
    /// Message shown in the toolbar until it expires
    pub status: Option<StatusMessage>,
    /// Search / filter bar state
    pub search: SearchState,
    /// Which commands wait on which, and how each command's last run in this session ended
    pub(super) dag: DagState,
    /// `next_generation` when each command was last queued: a process of a later generation
    /// belongs to that run, an earlier one is left over from a previous run
    pub(super) queued_generation: HashMap<String, u64>,
    /// Active context menu (right-click)
    pub context_menu: Option<ContextMenu>,
    /// Last known terminal area, which sizes commands started once their dependencies pass
    pub last_terminal_area: Rect,
    /// Pane size the running commands' terminals were last resized to, as (columns, rows)
    pub(super) applied_pty_size: Option<(u16, u16)>,
    /// Handle for in-flight async git selection task
    git_selection_handle: Option<JoinHandle<()>>,
    /// Generation counter for staleness detection of git selection results
    git_selection_generation: u64,
    /// Generation of the most recently started process
    pub(super) next_generation: u64,
    /// Command IDs from the current batch run (for auto-focus on failure)
    pub(super) batch_run_ids: Option<HashSet<String>>,
    /// Given to every terminal, which notifies it on new output
    pub(super) output_notify: Arc<Notify>,
    /// Whether the help overlay is shown
    pub show_help: bool,
    /// Written to the terminal after the next draw; see [`take_outbox`](Self::take_outbox)
    pub(super) outbox: Vec<Outbound>,
}

/// Collect all group IDs in the tree (including root).
fn collect_all_group_ids(group: &CommandGroup) -> Vec<String> {
    let mut ids = vec![group.id.clone()];
    for child in &group.children {
        ids.extend(collect_all_group_ids(child));
    }
    ids
}

/// Collect group IDs of all children (excluding root).
fn collect_child_group_ids(group: &CommandGroup) -> Vec<String> {
    let mut ids = Vec::new();
    for child in &group.children {
        ids.push(child.id.clone());
        ids.extend(collect_child_group_ids(child));
    }
    ids
}

/// Recursively collect IDs of groups that contain no selected commands.
/// Skips the root group (called on children directly).
fn collect_inactive_groups(
    group: &CommandGroup,
    selected: &HashSet<String>,
    to_collapse: &mut Vec<String>,
    to_expand: &mut Vec<String>,
) {
    for child in &group.children {
        let has_selected = child
            .all_commands()
            .iter()
            .any(|cmd| selected.contains(&cmd.id));
        if has_selected {
            to_expand.push(child.id.clone());
        } else {
            to_collapse.push(child.id.clone());
        }
        collect_inactive_groups(child, selected, to_collapse, to_expand);
    }
}

impl App {
    #[must_use]
    pub fn new(config: CommandGroup, cwd: PathBuf, log_buffer: LogBuffer) -> Self {
        let (event_tx, event_rx) = mpsc::channel(256);
        let mut app = App {
            config,
            cwd,
            visible_nodes: Vec::new(),
            cursor: 0,
            cursor_id: None,
            processes: HashMap::new(),
            active_terminal_id: None,
            fullscreen: false,
            tree_width: 30,
            event_tx,
            event_rx,
            should_quit: false,
            focus: Focus::Tree,
            expanded: HashMap::new(),
            user_expansion: HashSet::new(),
            selected: HashSet::new(),
            mouse: MouseState::default(),
            toolbar: ToolbarCache::default(),
            error_messages: HashMap::new(),
            tree_dirty: false,
            tree_scroll: 0,
            last_scroll_anchor: None,
            show_logs: false,
            log_buffer,
            log_scroll: 0,
            seen_log_issues: 0,
            log_badge: 0,
            status: None,
            search: SearchState::Inactive,
            dag: DagState::new(),
            queued_generation: HashMap::new(),
            context_menu: None,
            last_terminal_area: Rect::default(),
            applied_pty_size: None,
            git_selection_handle: None,
            git_selection_generation: 0,
            next_generation: 0,
            batch_run_ids: None,
            output_notify: Arc::new(Notify::new()),
            show_help: false,
            outbox: Vec::new(),
        };
        app.rebuild_visible_nodes();
        app
    }

    /// Apply results from a headless check run: select the commands that failed or were
    /// skipped, and start them in `terminal_area` so the user sees PTY output immediately.
    /// The cursor goes to the first of them, and to the first that fails again once all end.
    pub fn apply_check_result(
        &mut self,
        result: &crate::check::CheckResult,
        terminal_area: ratatui::layout::Rect,
    ) {
        let rerun = result.report.rerun_ids();
        for cmd in &result.report.commands {
            if rerun.contains(&cmd.id) {
                self.selected.insert(cmd.id.clone());
            } else {
                self.selected.remove(&cmd.id);
            }
        }
        self.fit_expansion_to_selection(true);
        self.rebuild_visible_nodes();

        // Move cursor to the first failed command in the visible tree
        if let Some(first_failed) = self
            .visible_nodes
            .iter()
            .position(|n| rerun.contains(&n.id))
        {
            self.set_cursor_index(first_failed);
        }

        let focus = self.current_command_id();
        self.batch_run_ids = Some(rerun.iter().cloned().collect());
        self.run_commands(&rerun, terminal_area, focus.as_deref());
    }

    /// Synchronously apply always-selected commands (no I/O, instant)
    pub fn apply_always_selection(&mut self) {
        use crate::selectors::RunnableSelector;
        use crate::selectors::always::AlwaysSelector;

        let commands: Vec<Command> = self.config.all_commands().into_iter().cloned().collect();
        let (always_cmds, _) = AlwaysSelector::split_active_commands(commands);
        for cmd in &always_cmds {
            self.selected.insert(cmd.id.clone());
        }
        debug!("Always-selected {} commands", always_cmds.len());
        self.fit_expansion_to_selection(true);
        self.rebuild_visible_nodes();
    }

    /// Spawn async git selection on a background thread
    pub fn spawn_git_selection(&mut self) {
        // Abort any in-flight selection
        if let Some(handle) = self.git_selection_handle.take() {
            handle.abort();
        }

        self.git_selection_generation += 1;
        let generation = self.git_selection_generation;
        let commands: Vec<Command> = self.config.all_commands().into_iter().cloned().collect();
        let event_tx = self.event_tx.clone();

        self.git_selection_handle = Some(tokio::task::spawn_blocking(move || {
            let refs: Vec<&Command> = commands.iter().collect();
            let output = selectors::select(&refs, &SelectOptions::default());
            for issue in &output.issues {
                warn!("{issue}");
            }
            let selected = commands
                .iter()
                .filter(|c| output.contains(&c.id))
                .cloned()
                .collect();
            let _ = event_tx.blocking_send(AppEvent::GitSelectionComplete(generation, selected));
        }));
    }

    /// Expand the groups that hold a selected command and, if `collapse`, collapse the others.
    /// Skips the root and the groups the user expanded or collapsed by hand.
    fn fit_expansion_to_selection(&mut self, collapse: bool) {
        let mut to_collapse = Vec::new();
        let mut to_expand = Vec::new();
        collect_inactive_groups(
            &self.config,
            &self.selected,
            &mut to_collapse,
            &mut to_expand,
        );
        if !collapse {
            to_collapse.clear();
        }
        let changes = to_collapse
            .into_iter()
            .map(|id| (id, false))
            .chain(to_expand.into_iter().map(|id| (id, true)));
        for (id, expanded) in changes {
            if !self.user_expansion.contains(&id) {
                self.expanded.insert(id, expanded);
            }
        }
    }

    /// Expand or collapse group `id` for the user; selection changes leave it that way.
    pub(super) fn set_expanded_by_user(&mut self, id: &str, expanded: bool) {
        self.expanded.insert(id.to_string(), expanded);
        self.user_expansion.insert(id.to_string());
        self.mark_tree_dirty();
    }

    /// Select the always and git-selected commands afresh, and fit every group to that
    /// selection, whatever the user expanded or collapsed.
    pub(super) fn git_select(&mut self) {
        self.selected.clear();
        self.user_expansion.clear();
        self.apply_always_selection();
        self.spawn_git_selection();
    }

    /// Mark the tree as needing a rebuild (lazy, happens at next render)
    pub fn mark_tree_dirty(&mut self) {
        self.tree_dirty = true;
    }

    /// Rebuild the flat `visible_nodes` list from the config tree
    pub fn rebuild_visible_nodes(&mut self) {
        let runs: HashMap<String, RunSummary> = self
            .config
            .all_commands()
            .into_iter()
            .map(|cmd| (cmd.id.clone(), self.run_summary(&cmd.id)))
            .collect();
        self.visible_nodes.clear();
        let mut ctx = TreeContext {
            expanded: &self.expanded,
            selected: &self.selected,
            runs: &runs,
            now: Instant::now(),
            nodes: &mut self.visible_nodes,
            filter: self.search.query(),
        };
        flatten_group(&self.config, 0, true, &[], &mut ctx);
        self.tree_dirty = false;
        self.resolve_cursor();
    }

    /// Where the latest run of command `id` stands
    #[must_use]
    pub fn run_summary(&self, id: &str) -> RunSummary {
        let queued = self.queued_generation.get(id).copied().unwrap_or(0);
        let run = self.processes.get(id).map(|p| RunRecord {
            status: &p.status,
            started_at: p.started_at,
            finished_at: p.finished_at,
            exit: p.exit.as_ref(),
            current: p.generation > queued,
        });
        RunSummary::derive(
            self.dag.state(id),
            run,
            self.error_messages.get(id).map(String::as_str),
        )
    }

    /// Whether any command is running or queued to run
    #[must_use]
    pub fn has_active_runs(&self) -> bool {
        !self.dag.is_idle()
    }

    /// Find a command by id in the config tree
    #[must_use]
    pub fn find_command(&self, id: &str) -> Option<Command> {
        find_command_in_group(&self.config, id)
    }

    /// Handle app events (called from event loop). Returns whether the screen needs a redraw.
    pub fn handle_app_event(&mut self, event: AppEvent) -> bool {
        let redraw = self.apply_app_event(event);
        let focused = self.focus;
        self.release_stale_focus();
        redraw || self.focus != focused
    }

    fn apply_app_event(&mut self, event: AppEvent) -> bool {
        match event {
            AppEvent::ProcessExited {
                id,
                generation,
                exit,
            } => {
                let Some(proc) = self.current_process(&id, generation) else {
                    return false;
                };
                proc.finished_at = Some(Instant::now());
                proc.status = if exit.stop_requested {
                    CommandStatus::Stopped
                } else if exit.success() {
                    CommandStatus::Success
                } else {
                    CommandStatus::Failure(exit.shell_code())
                };
                proc.exit = Some(exit);
                match proc.status {
                    CommandStatus::Stopped => {
                        info!("Command '{id}' stopped");
                        self.mark_stopped(&id);
                    }
                    CommandStatus::Success => {
                        self.selected.remove(&id);
                        self.dag.finish(&id, true);
                    }
                    _ => self.finish_failed(&id),
                }
                self.start_ready();
                self.mark_tree_dirty();
                self.check_batch_complete();
            }
            AppEvent::ProcessError {
                id,
                generation,
                message,
            } => {
                let Some(proc) = self.current_process(&id, generation) else {
                    return false;
                };
                error!("Process error for '{id}': {message}");
                proc.finished_at = Some(Instant::now());
                proc.status = CommandStatus::Error(message.clone());
                self.error_messages.insert(id.clone(), message);
                self.finish_failed(&id);
                self.mark_tree_dirty();
                self.check_batch_complete();
            }
            AppEvent::WatcherTriggered(matches) => {
                for m in matches {
                    self.selected.insert(m.id);
                }
                // Background events never collapse: the cursor or the user may be in there
                self.fit_expansion_to_selection(false);
                self.mark_tree_dirty();
            }
            AppEvent::GitSelectionComplete(generation, selected) => {
                if generation != self.git_selection_generation {
                    return false;
                }
                self.git_selection_handle = None;
                for cmd in &selected {
                    self.selected.insert(cmd.id.clone());
                }
                debug!("Git-selected {} commands", selected.len());
                self.fit_expansion_to_selection(false);
                self.mark_tree_dirty();
            }
            AppEvent::LogUpdated => {
                let unseen = self.unseen_log_issues();
                let badge_changed = unseen != self.log_badge;
                self.log_badge = unseen;
                return self.logs_on_screen() || badge_changed;
            }
            AppEvent::Status { text, level } => self.set_status(text, level),
            AppEvent::ClipboardResult {
                lines,
                result,
                fallback,
            } => self.finish_copy(lines, result, fallback),
        }
        true
    }

    /// What to write to the terminal fnug runs in, oldest first; call after each draw.
    pub fn take_outbox(&mut self) -> Vec<Outbound> {
        std::mem::take(&mut self.outbox)
    }

    /// Show `text` in the toolbar for a few seconds, replacing any earlier message.
    pub fn set_status(&mut self, text: impl Into<String>, level: StatusLevel) {
        self.status = Some(StatusMessage::new(text.into(), level, Instant::now()));
    }

    /// Warnings and errors logged since the log panel last showed
    #[must_use]
    pub fn unseen_log_issues(&self) -> usize {
        self.log_buffer
            .issue_count()
            .saturating_sub(self.seen_log_issues)
    }

    /// Record that the log panel shows every warning and error logged so far.
    pub(super) fn mark_logs_seen(&mut self) {
        self.seen_log_issues = self.log_buffer.issue_count();
        self.log_badge = 0;
    }

    /// Whether [`tick`](Self::tick) has anything to do: advance running commands' elapsed
    /// time, or expire the status message.
    #[must_use]
    pub fn wants_tick(&self) -> bool {
        self.has_active_runs() || self.status.is_some()
    }

    /// Update what changes with time alone, at `now`. Returns whether the screen needs a
    /// redraw.
    pub fn tick(&mut self, now: Instant) -> bool {
        let mut changed = false;
        if self.status.as_ref().is_some_and(|s| now >= s.expires_at) {
            self.status = None;
            changed = true;
        }
        if self.has_active_runs() {
            self.mark_tree_dirty();
            changed = true;
        }
        changed
    }

    /// The process for `id`, unless an event of `generation` comes from an earlier, replaced run
    fn current_process(&mut self, id: &str, generation: u64) -> Option<&mut ProcessInstance> {
        let proc = self
            .processes
            .get_mut(id)
            .filter(|p| p.generation == generation);
        if proc.is_none() {
            debug!("Ignoring event from a replaced run of '{id}'");
        }
        proc
    }

    /// Record that the running command `failed_id` failed, and show why the commands waiting on
    /// it, directly or not, won't run.
    pub(super) fn finish_failed(&mut self, failed_id: &str) {
        let skipped = self.dag.finish(failed_id, false);
        if skipped.is_empty() {
            return;
        }
        let failed_name = self
            .find_command(failed_id)
            .map_or_else(|| failed_id.to_string(), |c| c.name);
        for (cmd_id, _) in skipped {
            let msg = format!("Dependency '{failed_name}' failed");
            self.error_messages.insert(cmd_id, msg);
        }
    }

    /// Record that the user stopped `stopped_id`: the commands waiting on it, directly or not,
    /// won't run, and that is no failure.
    pub(super) fn mark_stopped(&mut self, stopped_id: &str) {
        for cmd_id in self.dag.stop(stopped_id) {
            info!("Cancelled '{cmd_id}', which was waiting on '{stopped_id}'");
        }
    }

    /// Check if a batch run is complete and auto-focus first failure
    pub(super) fn check_batch_complete(&mut self) {
        let Some(ref batch_ids) = self.batch_run_ids else {
            return;
        };

        let all_done = batch_ids.iter().all(|id| !self.dag.is_active(id));

        if !all_done {
            return;
        }

        let failed_ids: Vec<String> = batch_ids
            .iter()
            .filter(|id| self.run_summary(id).is_failure())
            .cloned()
            .collect();

        // Clear batch tracking
        self.batch_run_ids = None;

        if failed_ids.is_empty() {
            return;
        }
        if self.tree_dirty {
            self.rebuild_visible_nodes();
        }

        // Don't move cursor if it's already on a failed command
        if let Some(node) = self.visible_nodes.get(self.cursor)
            && failed_ids.contains(&node.id)
        {
            return;
        }

        // Move cursor to first failed command in the visible tree
        if let Some(pos) = self
            .visible_nodes
            .iter()
            .position(|n| failed_ids.contains(&n.id))
        {
            self.set_cursor_index(pos);
            self.active_terminal_id = Some(self.visible_nodes[pos].id.clone());
        }
    }

    /// Expand all groups in the tree
    pub fn expand_all(&mut self) {
        for id in collect_all_group_ids(&self.config) {
            self.set_expanded_by_user(&id, true);
        }
    }

    /// Collapse all groups in the tree (except root)
    pub fn collapse_all(&mut self) {
        for id in collect_child_group_ids(&self.config) {
            self.set_expanded_by_user(&id, false);
        }
    }

    /// Send `SIGHUP` to every command's process group and wait until each is reaped, then abort
    /// all spawned tasks.
    ///
    /// Groups not reaped after [`QUIT_GRACE`], such as background processes ignoring `SIGHUP`
    /// that still hold a PTY, get `SIGKILL`, so this returns shortly after it.
    pub async fn shutdown(&mut self) {
        /// How long killed processes get to exit and drain their last output
        const KILL_WAIT: Duration = Duration::from_millis(500);

        if let Some(handle) = self.git_selection_handle.take() {
            handle.abort();
        }
        let processes: Vec<_> = self.processes.drain().collect();
        for (id, proc) in &processes {
            if let Err(e) = proc.terminal.stop(StopSignal::Hangup, QUIT_GRACE) {
                log::warn!("Failed to stop process '{id}': {e}");
            }
        }
        let all_reaped =
            || futures::future::join_all(processes.iter().map(|(_, p)| p.terminal.wait_reaped()));
        // Wait for reaping, not the exit: a background process can outlive the command, and the
        // escalation thread dies with fnug before it can kill it
        if tokio::time::timeout(QUIT_GRACE, all_reaped())
            .await
            .is_err()
        {
            // Safety net: normally the per-stop escalation already sent SIGKILL at QUIT_GRACE
            for (id, proc) in &processes {
                if let Err(e) = proc.terminal.force_kill() {
                    log::warn!("Failed to kill process '{id}': {e}");
                }
            }
            let _ = tokio::time::timeout(KILL_WAIT, all_reaped()).await;
        }
        for (_, proc) in processes {
            for handle in proc.task_handles {
                handle.abort();
            }
        }
    }

    /// Whether the terminal on screen has output not drawn yet. Other terminals' output needs
    /// no redraw, nor does any while the log panel covers the pane.
    #[must_use]
    pub fn any_terminal_dirty(&self) -> bool {
        !self.logs_on_screen() && self.active_process().is_some_and(|p| p.terminal.is_dirty())
    }

    /// Whether the log panel covers the terminal pane; fullscreen always shows the terminal.
    pub(super) fn logs_on_screen(&self) -> bool {
        self.show_logs && !self.fullscreen
    }

    /// Clear the dirty flag of the terminal on screen; call before drawing.
    ///
    /// Hidden terminals stay dirty, which keeps their output from waking the event loop.
    pub fn clear_terminal_dirty(&self) {
        if let Some(proc) = self.active_process() {
            proc.terminal.clear_dirty();
        }
    }

    /// Notified when a terminal gets output while it isn't dirty; the event loop waits on it.
    #[must_use]
    pub fn output_notify(&self) -> Arc<Notify> {
        Arc::clone(&self.output_notify)
    }

    /// Get a reference to the active terminal's process, if any.
    fn active_process(&self) -> Option<&ProcessInstance> {
        self.active_terminal_id
            .as_ref()
            .and_then(|id| self.processes.get(id))
    }

    /// Scroll the active terminal up by `lines`
    pub fn scroll_terminal(&self, lines: usize) {
        if let Some(proc) = self.active_process() {
            let parser = proc.terminal.parser().lock();
            let new_pos =
                (parser.screen().scrollback() + lines).min(parser.screen().scrollback_len());
            drop(parser);
            if let Err(e) = proc.terminal.set_scroll(new_pos) {
                log::debug!("Failed to scroll terminal: {e}");
            }
        }
    }

    /// Scroll the active terminal down by `lines`
    pub fn scroll_terminal_down(&self, lines: usize) {
        if let Some(proc) = self.active_process() {
            let current = proc.terminal.parser().lock().screen().scrollback();
            if let Err(e) = proc.terminal.set_scroll(current.saturating_sub(lines)) {
                log::debug!("Failed to scroll terminal: {e}");
            }
        }
    }

    /// Scroll the active terminal to the top of scrollback
    pub fn scroll_terminal_to_top(&self) {
        if let Some(proc) = self.active_process() {
            let len = proc.terminal.parser().lock().screen().scrollback_len();
            if let Err(e) = proc.terminal.set_scroll(len) {
                log::debug!("Failed to scroll to top: {e}");
            }
        }
    }

    /// Scroll the active terminal to the bottom
    pub fn scroll_terminal_to_bottom(&self) {
        if let Some(proc) = self.active_process()
            && let Err(e) = proc.terminal.set_scroll(0)
        {
            log::debug!("Failed to scroll to bottom: {e}");
        }
    }

    /// Whether the command on screen is running, so it can take keyboard focus
    #[must_use]
    pub fn active_command_is_running(&self) -> bool {
        self.active_process()
            .is_some_and(|proc| proc.status == CommandStatus::Running)
    }

    /// Whether the command on screen runs a full-screen program or reads the mouse, so a click
    /// in the pane is meant for it
    #[must_use]
    pub fn active_command_is_interactive(&self) -> bool {
        self.active_command_is_running()
            && self.active_screen_is(|screen| {
                screen.alternate_screen()
                    || screen.mouse_protocol_mode() != vt100::MouseProtocolMode::None
            })
    }

    /// Whether the command on screen shows the alternate screen, as full-screen programs do
    #[must_use]
    pub fn active_command_on_alternate_screen(&self) -> bool {
        self.active_screen_is(vt100::Screen::alternate_screen)
    }

    fn active_screen_is(&self, test: impl FnOnce(&vt100::Screen) -> bool) -> bool {
        self.active_process()
            .is_some_and(|proc| test(proc.terminal.parser().lock().screen()))
    }

    /// Give the command on screen keyboard focus, if it is running.
    pub(super) fn focus_terminal(&mut self) {
        if self.active_command_is_running() {
            self.focus = Focus::Terminal;
        }
    }

    /// Give focus back to the tree once the focused command ended or left the screen.
    pub(super) fn release_stale_focus(&mut self) {
        if self.focus == Focus::Terminal && !self.active_command_is_running() {
            self.focus = Focus::Tree;
        }
    }

    /// Adjust `tree_scroll` so the cursor row is visible within the given height, and no
    /// blank rows follow the last node
    pub fn ensure_cursor_visible(&mut self, height: usize) {
        if height == 0 {
            return;
        }
        if self.cursor < self.tree_scroll {
            self.tree_scroll = self.cursor;
        } else if self.cursor >= self.tree_scroll + height {
            self.tree_scroll = self.cursor - height + 1;
        }
        self.clamp_tree_scroll(height);
    }

    /// Keep the last row at or below the bottom of a tree `height` rows tall.
    pub(super) fn clamp_tree_scroll(&mut self, height: usize) {
        self.tree_scroll = self
            .tree_scroll
            .min(self.visible_nodes.len().saturating_sub(height));
    }

    /// Toggle the current node: expand/collapse for groups, select/deselect for commands.
    pub(super) fn toggle_current_node(&mut self) {
        if let Some(node) = self.visible_nodes.get(self.cursor) {
            match &node.kind {
                NodeKind::Group { .. } => {
                    if let Some(group) = find_group_in_group(&self.config, &node.id) {
                        let group_id = group.id.clone();
                        let cmd_ids: Vec<String> =
                            group.all_commands().iter().map(|c| c.id.clone()).collect();
                        let all_selected = cmd_ids.iter().all(|id| self.selected.contains(id));
                        let select = !all_selected;
                        for id in cmd_ids {
                            if select {
                                self.selected.insert(id);
                            } else {
                                self.selected.remove(&id);
                            }
                        }
                        // Keep expand/collapse in sync: expand when selecting, collapse when deselecting
                        self.set_expanded_by_user(&group_id, select);
                    }
                }
                NodeKind::Command { selected, .. } => {
                    if *selected {
                        self.selected.remove(&node.id);
                    } else {
                        self.selected.insert(node.id.clone());
                    }
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
        if let Some(id) = self.current_command_id() {
            self.active_terminal_id = Some(id);
        }
    }

    /// Close the context menu
    pub fn close_context_menu(&mut self) {
        self.context_menu = None;
    }

    /// Execute the currently selected context menu action
    #[expect(
        clippy::too_many_lines,
        reason = "dispatches all context menu action/target combinations"
    )]
    pub fn execute_context_menu_action(&mut self, terminal_area: Rect) {
        let Some(menu) = self.context_menu.take() else {
            return;
        };
        let Some(action) = menu
            .items
            .get(menu.cursor)
            .filter(|i| i.enabled)
            .map(|i| i.action)
        else {
            return;
        };

        match (&menu.target, action) {
            (ContextMenuTarget::Group { id, .. }, ContextMenuAction::Expand) => {
                self.set_expanded_by_user(id, true);
            }
            (ContextMenuTarget::Group { id, .. }, ContextMenuAction::Collapse) => {
                self.set_expanded_by_user(id, false);
            }
            (ContextMenuTarget::Group { id, .. }, ContextMenuAction::SelectAll) => {
                if let Some(group) = find_group_in_group(&self.config, id) {
                    for cmd in group.all_commands() {
                        self.selected.insert(cmd.id.clone());
                    }
                    self.mark_tree_dirty();
                }
            }
            (ContextMenuTarget::Group { id, .. }, ContextMenuAction::DeselectAll) => {
                if let Some(group) = find_group_in_group(&self.config, id) {
                    for cmd in group.all_commands() {
                        self.selected.remove(&cmd.id);
                    }
                    self.mark_tree_dirty();
                }
            }
            (ContextMenuTarget::Group { id, .. }, ContextMenuAction::Run) => {
                self.run_group(id, terminal_area);
            }
            (ContextMenuTarget::Group { id, .. }, ContextMenuAction::RunSelected) => {
                if let Some(group) = find_group_in_group(&self.config, id) {
                    let selected_ids: Vec<String> = group
                        .all_commands()
                        .iter()
                        .filter(|c| self.selected.contains(&c.id))
                        .map(|c| c.id.clone())
                        .collect();
                    let focus = selected_ids.first().cloned();
                    self.run_commands(&selected_ids, terminal_area, focus.as_deref());
                }
            }
            (ContextMenuTarget::Command { id, .. }, ContextMenuAction::Select) => {
                self.selected.insert(id.clone());
                self.mark_tree_dirty();
            }
            (ContextMenuTarget::Command { id, .. }, ContextMenuAction::Deselect) => {
                self.selected.remove(id);
                self.mark_tree_dirty();
            }
            (
                ContextMenuTarget::Command { id, .. },
                ContextMenuAction::Run | ContextMenuAction::Restart,
            ) => {
                self.run_command(id, terminal_area);
            }
            (ContextMenuTarget::Command { id, .. }, ContextMenuAction::Stop) => {
                self.stop_command(id);
            }
            (ContextMenuTarget::Command { id, .. }, ContextMenuAction::Copy) => {
                self.copy_command_output(id);
            }
            (ContextMenuTarget::Command { id, .. }, ContextMenuAction::Clear) => {
                self.clear_command(id);
            }
            (ContextMenuTarget::Terminal, ContextMenuAction::ScrollToTop) => {
                if let Some(ref active_id) = self.active_terminal_id
                    && let Some(proc) = self.processes.get(active_id)
                {
                    let scrollback_len = proc.terminal.parser().lock().screen().scrollback_len();
                    if let Err(e) = proc.terminal.set_scroll(scrollback_len) {
                        debug!("Failed to scroll to top: {e}");
                    }
                }
            }
            (ContextMenuTarget::Terminal, ContextMenuAction::ScrollToBottom) => {
                if let Some(ref active_id) = self.active_terminal_id
                    && let Some(proc) = self.processes.get(active_id)
                    && let Err(e) = proc.terminal.set_scroll(0)
                {
                    debug!("Failed to scroll to bottom: {e}");
                }
            }
            (ContextMenuTarget::Terminal, ContextMenuAction::Run | ContextMenuAction::Restart) => {
                if let Some(id) = self.active_terminal_id.clone() {
                    self.run_command(&id, terminal_area);
                }
            }
            (ContextMenuTarget::Terminal, ContextMenuAction::Stop) => {
                if let Some(id) = self.active_terminal_id.clone() {
                    self.stop_command(&id);
                }
            }
            (ContextMenuTarget::Terminal, ContextMenuAction::Copy) => {
                if let Some(id) = self.active_terminal_id.clone() {
                    self.copy_command_output(&id);
                }
            }
            (ContextMenuTarget::Terminal, ContextMenuAction::Clear) => {
                if let Some(id) = self.active_terminal_id.clone() {
                    self.clear_command(&id);
                }
            }
            _ => {}
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
                    self.run_command(&id, terminal_area);
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
            ToolbarAction::Copy => {
                if let Some(id) = self.current_command_id() {
                    self.copy_command_output(&id);
                }
            }
            ToolbarAction::GitSelect => self.git_select(),
            ToolbarAction::ToggleFullscreen => {
                self.fullscreen = !self.fullscreen;
            }
            ToolbarAction::FocusTerminal => self.focus_terminal(),
            ToolbarAction::LeaveTerminal => self.focus = Focus::Tree,
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
            ToolbarAction::ExpandAll => {
                self.expand_all();
            }
            ToolbarAction::CollapseAll => {
                self.collapse_all();
            }
            ToolbarAction::ShowHelp => {
                self.show_help = !self.show_help;
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
        let app = App::new(config, PathBuf::new(), LogBuffer::new());

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

    #[test]
    fn status_expires_by_level() {
        let mut app = App::new(make_test_tree(), PathBuf::new(), LogBuffer::new());
        assert!(!app.wants_tick());

        app.set_status("copied", StatusLevel::Info);
        assert!(app.wants_tick());
        let later = Instant::now() + Duration::from_secs(5);
        assert!(app.tick(later));
        assert_eq!(app.status, None);

        app.set_status("failed", StatusLevel::Error);
        assert!(!app.tick(later));
        assert!(app.status.is_some());
    }

    #[test]
    fn status_event_shows_message() {
        let mut app = App::new(make_test_tree(), PathBuf::new(), LogBuffer::new());
        let event = AppEvent::Status {
            text: "watch limit".into(),
            level: StatusLevel::Warn,
        };
        assert!(app.handle_app_event(event));
        assert_eq!(app.status.as_ref().unwrap().text, "watch limit");
    }

    #[test]
    fn copy_reports_what_happened() {
        let mut app = App::new(make_test_tree(), PathBuf::new(), LogBuffer::new());
        app.copy_command_output("fmt");
        assert!(app.status.as_ref().unwrap().text.contains("hasn't run"));

        app.handle_app_event(AppEvent::ClipboardResult {
            lines: 1,
            result: Ok("wl-copy"),
            fallback: Some("x".into()),
        });
        assert_eq!(app.status.as_ref().unwrap().text, "Copied 1 line");
        assert!(app.take_outbox().is_empty());

        app.handle_app_event(AppEvent::ClipboardResult {
            lines: 200,
            result: Err("no clipboard program found".into()),
            fallback: Some("text".into()),
        });
        assert_eq!(
            app.status.as_ref().unwrap().text,
            "Copied 200 lines via OSC 52"
        );
        assert_eq!(app.take_outbox(), [Outbound::Clipboard("text".into())]);

        app.handle_app_event(AppEvent::ClipboardResult {
            lines: 2,
            result: Err("xclip: exited with 1".into()),
            fallback: None,
        });
        let status = app.status.as_ref().unwrap();
        assert_eq!(status.text, "Copy failed: xclip: exited with 1");
        assert_eq!(status.level, StatusLevel::Error);
    }

    #[test]
    fn log_update_redraws_only_under_log_panel() {
        let mut app = App::new(make_test_tree(), PathBuf::new(), LogBuffer::new());
        assert!(!app.handle_app_event(AppEvent::LogUpdated));
        app.show_logs = true;
        assert!(app.handle_app_event(AppEvent::LogUpdated));
        // Fullscreen shows the terminal instead
        app.fullscreen = true;
        assert!(!app.handle_app_event(AppEvent::LogUpdated));
    }

    mod expansion {
        use crossterm::event::KeyCode;

        use crate::selectors::watch::WatchMatch;
        use crate::tui::app::{App, AppEvent};
        use crate::tui::test_util::{command, cursor_node, draw, press, two_groups};

        fn watcher_selects(id: &str) -> AppEvent {
            AppEvent::WatcherTriggered(vec![WatchMatch {
                id: id.into(),
                files: vec![],
            }])
        }

        fn expanded(app: &App, id: &str) -> bool {
            app.expanded.get(id).copied().unwrap_or(true)
        }

        /// [`two_groups`] as the TUI starts: nothing selected, so both groups collapsed
        fn started() -> App {
            let mut app = two_groups();
            app.apply_always_selection();
            assert!(!expanded(&app, "alpha") && !expanded(&app, "beta"));
            app
        }

        /// Move the cursor to `id` and press `key` there
        fn press_on(app: &mut App, id: &str, key: KeyCode) {
            draw(app, 80, 24);
            while cursor_node(app) != Some(id) {
                press(app, KeyCode::Char('j'));
            }
            press(app, key);
        }

        #[test]
        fn git_selection_keeps_group_user_expanded() {
            let mut app = started();
            press_on(&mut app, "alpha", KeyCode::Char('l'));

            app.handle_app_event(AppEvent::GitSelectionComplete(0, vec![command("b1")]));

            assert!(
                expanded(&app, "alpha"),
                "collapsed a group the user expanded"
            );
            assert!(expanded(&app, "beta"));
        }

        #[test]
        fn watcher_keeps_group_user_collapsed() {
            let mut app = two_groups();
            press_on(&mut app, "beta", KeyCode::Char('h'));

            app.handle_app_event(watcher_selects("b1"));

            assert!(
                !expanded(&app, "beta"),
                "expanded a group the user collapsed"
            );
        }

        #[test]
        fn watcher_event_keeps_cursor_node() {
            let mut app = started();
            press_on(&mut app, "alpha", KeyCode::Char('l'));
            press_on(&mut app, "a2", KeyCode::Null);

            app.handle_app_event(watcher_selects("b1"));
            draw(&mut app, 80, 24);

            assert_eq!(cursor_node(&app), Some("a2"));
            assert_eq!(app.active_terminal_id.as_deref(), Some("a2"));
        }

        #[tokio::test]
        async fn git_select_key_resets_user_expansion() {
            let mut app = started();
            press_on(&mut app, "alpha", KeyCode::Char('l'));

            press(&mut app, KeyCode::Char('g'));

            assert!(!expanded(&app, "alpha"));
        }
    }
}
