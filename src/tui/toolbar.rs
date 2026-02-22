use std::borrow::Cow;

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::app::{App, CommandStatus, Focus};
use super::tree_widget::NodeKind;
use crate::theme;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ToolbarAction {
    RunSelected,
    ToggleSpace,
    Run,
    Stop,
    Clear,
    GitSelect,
    ToggleFullscreen,
    FocusTerminal,
    Quit,
    BackToTree,
    ToggleLogs,
    Search,
    ClearSearch,
    AcceptSearch,
}

#[derive(Debug)]
pub struct ToolbarRegion {
    pub x_start: u16,
    pub x_end: u16,
    pub action: ToolbarAction,
}

struct Shortcut {
    key: &'static str,
    desc: Cow<'static, str>,
    action: ToolbarAction,
}

impl Shortcut {
    fn new(key: &'static str, desc: impl Into<Cow<'static, str>>, action: ToolbarAction) -> Self {
        Self {
            key,
            desc: desc.into(),
            action,
        }
    }

    /// Width this shortcut occupies: " key " (padded badge) + space + desc
    fn width(&self) -> usize {
        1 + self.key.len() + 1 + 1 + self.desc.len()
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "toolbar shortcut list covers all UI states"
)]
fn get_shortcuts(app: &App) -> Vec<Shortcut> {
    let mut shortcuts = Vec::new();

    if app.fullscreen {
        shortcuts.push(Shortcut::new(
            "^R",
            "Exit fullscreen",
            ToolbarAction::ToggleFullscreen,
        ));
        shortcuts.push(Shortcut::new(
            "ESC",
            "Back to tree",
            ToolbarAction::BackToTree,
        ));
        shortcuts.push(Shortcut::new("^C", "Quit", ToolbarAction::Quit));
        return shortcuts;
    }

    match app.focus {
        Focus::Terminal => {
            shortcuts.push(Shortcut::new(
                "ESC",
                "Back to tree",
                ToolbarAction::BackToTree,
            ));
            shortcuts.push(Shortcut::new(
                "^R",
                "Fullscreen",
                ToolbarAction::ToggleFullscreen,
            ));
            shortcuts.push(Shortcut::new("^C", "Quit", ToolbarAction::Quit));
        }
        Focus::Tree => {
            let cursor_node = app.visible_nodes.get(app.cursor);

            // "Run selected (N)" — only when there are selected commands
            let selected_count: usize = app.selected.values().filter(|&&v| v).count();
            if selected_count > 0 {
                shortcuts.push(Shortcut::new(
                    "ENTER",
                    Cow::Owned(format!("Run selected ({selected_count})")),
                    ToolbarAction::RunSelected,
                ));
            }

            match cursor_node.map(|n| &n.kind) {
                Some(NodeKind::Command {
                    selected, status, ..
                }) => {
                    let toggle_label = if *selected { "Deselect" } else { "Select" };
                    shortcuts.push(Shortcut::new(
                        "SPACE",
                        toggle_label,
                        ToolbarAction::ToggleSpace,
                    ));
                    shortcuts.push(Shortcut::new("R", "Run", ToolbarAction::Run));
                    if matches!(status, CommandStatus::Running) {
                        shortcuts.push(Shortcut::new("S", "Stop", ToolbarAction::Stop));
                    }
                    shortcuts.push(Shortcut::new("C", "Clear", ToolbarAction::Clear));
                }
                Some(NodeKind::Group { expanded, .. }) => {
                    let toggle_label = if *expanded { "Collapse" } else { "Expand" };
                    shortcuts.push(Shortcut::new(
                        "SPACE",
                        toggle_label,
                        ToolbarAction::ToggleSpace,
                    ));
                }
                None => {}
            }

            shortcuts.push(Shortcut::new("G", "Git select", ToolbarAction::GitSelect));
            shortcuts.push(Shortcut::new(
                "^R",
                "Fullscreen",
                ToolbarAction::ToggleFullscreen,
            ));

            if app.active_terminal_id.is_some() && app.active_command_is_interactive() {
                shortcuts.push(Shortcut::new(
                    "TAB",
                    "Terminal",
                    ToolbarAction::FocusTerminal,
                ));
            }

            if app.search.is_editing() {
                shortcuts.push(Shortcut::new("ESC", "Clear", ToolbarAction::ClearSearch));
                shortcuts.push(Shortcut::new(
                    "ENTER",
                    "Accept",
                    ToolbarAction::AcceptSearch,
                ));
            } else if app.search.is_filtering() {
                shortcuts.push(Shortcut::new("/", "Edit filter", ToolbarAction::Search));
                shortcuts.push(Shortcut::new(
                    "ESC",
                    "Clear filter",
                    ToolbarAction::ClearSearch,
                ));
            } else {
                shortcuts.push(Shortcut::new("/", "Search", ToolbarAction::Search));
            }

            let log_label = if app.show_logs { "Hide logs" } else { "Logs" };
            shortcuts.push(Shortcut::new("L", log_label, ToolbarAction::ToggleLogs));
            shortcuts.push(Shortcut::new("Q", "Quit", ToolbarAction::Quit));
        }
    }

    shortcuts
}

/// Separator between shortcuts
const SEP: &str = "  ";

pub fn build_toolbar_line(app: &App, width: u16) -> (Line<'static>, Vec<ToolbarRegion>) {
    let shortcuts = get_shortcuts(app);
    let max_width = width as usize;

    let key_style = Style::default()
        .fg(theme::TOOLBAR_KEY_FG)
        .bg(theme::TOOLBAR_KEY_BG)
        .add_modifier(Modifier::BOLD);
    let desc_style = Style::default()
        .fg(theme::TOOLBAR_DESC)
        .bg(theme::TOOLBAR_BG);
    let bg_style = Style::default().bg(theme::TOOLBAR_BG);

    let hover_desc_style = Style::default()
        .fg(theme::TOOLBAR_DESC)
        .bg(theme::TOOLBAR_BG)
        .add_modifier(Modifier::UNDERLINED);

    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut regions: Vec<ToolbarRegion> = Vec::new();
    let mut x = 0usize;

    for (i, shortcut) in shortcuts.iter().enumerate() {
        let sep_width = if i > 0 { SEP.len() } else { 0 };
        let needed = sep_width + shortcut.width();

        if x + needed > max_width {
            break;
        }

        if i > 0 {
            spans.push(Span::styled(SEP, bg_style));
            x += sep_width;
        }

        #[expect(
            clippy::cast_possible_truncation,
            reason = "toolbar x position fits in u16"
        )]
        let x_start = x as u16;
        let is_hovered = app.toolbar.hover == Some(i);

        // Key rendered as a badge: " KEY " on dark background
        spans.push(Span::styled(format!(" {} ", shortcut.key), key_style));
        spans.push(Span::styled(" ", bg_style));
        spans.push(Span::styled(
            shortcut.desc.clone(),
            if is_hovered {
                hover_desc_style
            } else {
                desc_style
            },
        ));

        x += shortcut.width();

        regions.push(ToolbarRegion {
            x_start,
            #[expect(
                clippy::cast_possible_truncation,
                reason = "toolbar x position fits in u16"
            )]
            x_end: x as u16,
            action: shortcut.action,
        });
    }

    // Fill remaining width with background
    if x < max_width {
        let padding = " ".repeat(max_width - x);
        spans.push(Span::styled(padding, bg_style));
    }

    (Line::from(spans), regions)
}
