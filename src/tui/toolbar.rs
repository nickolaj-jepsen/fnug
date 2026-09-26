use std::borrow::Cow;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use super::app::{App, CommandStatus, Focus};
use super::status::{StatusLevel, StatusMessage};
use super::tree_widget::NodeKind;
use crate::theme;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ToolbarAction {
    RunSelected,
    ToggleSpace,
    Run,
    Stop,
    Clear,
    Copy,
    GitSelect,
    ToggleFullscreen,
    FocusTerminal,
    Quit,
    BackToTree,
    ToggleLogs,
    Search,
    ClearSearch,
    AcceptSearch,
    ExpandAll,
    CollapseAll,
    ShowHelp,
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
            let selected_count = app.selected.len();
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
                    shortcuts.push(Shortcut::new("C", "Copy", ToolbarAction::Copy));
                }
                Some(NodeKind::Group {
                    selected, total, ..
                }) => {
                    let toggle_label = if *selected == *total {
                        "Deselect all"
                    } else {
                        "Select all"
                    };
                    shortcuts.push(Shortcut::new(
                        "SPACE",
                        toggle_label,
                        ToolbarAction::ToggleSpace,
                    ));
                    shortcuts.push(Shortcut::new("R", "Run all", ToolbarAction::Run));
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

            let unseen = app.unseen_log_issues();
            let log_label: Cow<'static, str> = if app.show_logs {
                "Hide logs".into()
            } else if unseen > 0 {
                format!("Logs ({unseen}!)").into()
            } else {
                "Logs".into()
            };
            shortcuts.push(Shortcut::new("L", log_label, ToolbarAction::ToggleLogs));
            shortcuts.push(Shortcut::new("?", "Help", ToolbarAction::ShowHelp));
            shortcuts.push(Shortcut::new("Q", "Quit", ToolbarAction::Quit));
        }
    }

    shortcuts
}

/// Separator between shortcuts
const SEP: &str = "  ";

/// A shortcut as a key badge, " KEY ", then its description
fn shortcut_spans(shortcut: &Shortcut, hovered: bool) -> [Span<'static>; 3] {
    let key_style = Style::default()
        .fg(theme::TOOLBAR_KEY_FG)
        .bg(theme::TOOLBAR_KEY_BG)
        .add_modifier(Modifier::BOLD);
    let mut desc_style = Style::default()
        .fg(theme::TOOLBAR_DESC)
        .bg(theme::TOOLBAR_BG);
    if hovered {
        desc_style = desc_style.add_modifier(Modifier::UNDERLINED);
    }
    [
        Span::styled(format!(" {} ", shortcut.key), key_style),
        Span::styled(" ", Style::default().bg(theme::TOOLBAR_BG)),
        Span::styled(shortcut.desc.clone(), desc_style),
    ]
}

/// `text` cut to at most `max` characters, ending in `…` when cut
fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut cut: String = text.chars().take(max.saturating_sub(1)).collect();
    if max > 0 {
        cut.push('…');
    }
    cut
}

/// The status message in place of the shortcuts, with `? Help` kept at the right edge
fn status_line(
    app: &App,
    status: &StatusMessage,
    width: u16,
) -> (Line<'static>, Vec<ToolbarRegion>) {
    let max_width = usize::from(width);
    let bg_style = Style::default().bg(theme::TOOLBAR_BG);
    let color = match status.level {
        StatusLevel::Info => theme::TOOLBAR_DESC,
        StatusLevel::Warn => Color::Yellow,
        StatusLevel::Error => theme::FAILURE,
    };
    let help = Shortcut::new("?", "Help", ToolbarAction::ShowHelp);
    // A leading space, the text, and at least one space before the help
    let text = truncate(&status.text, max_width.saturating_sub(help.width() + 2));
    let used = 1 + text.chars().count();

    let mut spans = vec![
        Span::styled(" ", bg_style),
        Span::styled(text, Style::default().fg(color).bg(theme::TOOLBAR_BG)),
    ];
    let mut regions = Vec::new();
    if used + 1 + help.width() <= max_width {
        let x_start = max_width - help.width();
        spans.push(Span::styled(" ".repeat(x_start - used), bg_style));
        spans.extend(shortcut_spans(&help, app.toolbar.hover == Some(0)));
        #[expect(
            clippy::cast_possible_truncation,
            reason = "toolbar x position fits in u16"
        )]
        regions.push(ToolbarRegion {
            x_start: x_start as u16,
            x_end: width,
            action: help.action,
        });
    } else if used < max_width {
        spans.push(Span::styled(" ".repeat(max_width - used), bg_style));
    }
    (Line::from(spans), regions)
}

pub fn build_toolbar_line(app: &App, width: u16) -> (Line<'static>, Vec<ToolbarRegion>) {
    // Fullscreen and terminal focus keep their few shortcuts: they tell how to get back
    if let Some(status) = &app.status
        && !app.fullscreen
        && app.focus == Focus::Tree
    {
        return status_line(app, status, width);
    }
    let shortcuts = get_shortcuts(app);
    let max_width = width as usize;
    let bg_style = Style::default().bg(theme::TOOLBAR_BG);

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
        spans.extend(shortcut_spans(shortcut, app.toolbar.hover == Some(i)));
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

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use crossterm::event::KeyCode;
    use log::Level;

    use super::*;
    use crate::logger::LogEntry;
    use crate::tui::app::AppEvent;
    use crate::tui::test_util::{draw, press, two_groups};

    fn toolbar_text(app: &App, width: u16) -> String {
        let (line, _) = build_toolbar_line(app, width);
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn status_replaces_shortcuts_but_keeps_help() {
        let mut app = two_groups();
        app.set_status("Not watching every file", StatusLevel::Warn);

        let (_, regions) = build_toolbar_line(&app, 80);
        let text = toolbar_text(&app, 80);

        assert!(text.starts_with(" Not watching every file "), "{text:?}");
        assert!(text.ends_with(" ?  Help"), "{text:?}");
        assert!(!text.contains("Quit"), "{text:?}");
        assert_eq!(text.chars().count(), 80);
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].action, ToolbarAction::ShowHelp);
        assert_eq!(regions[0].x_end, 80);
    }

    #[test]
    fn long_status_is_cut_before_help() {
        let mut app = two_groups();
        app.set_status("x".repeat(100), StatusLevel::Error);

        let text = toolbar_text(&app, 40);

        assert!(text.contains("x…"), "{text:?}");
        assert!(text.ends_with(" ?  Help"), "{text:?}");
        assert_eq!(text.chars().count(), 40);
    }

    #[test]
    fn terminal_focus_keeps_its_shortcuts_over_status() {
        let mut app = two_groups();
        app.set_status("Copied", StatusLevel::Info);
        app.focus = Focus::Terminal;

        assert!(toolbar_text(&app, 80).contains("Back to tree"));
    }

    #[test]
    fn log_badge_counts_unseen_issues() {
        let mut app = two_groups();
        let warn = LogEntry {
            level: Level::Warn,
            target: "test".into(),
            message: "careful".into(),
            timestamp: Instant::now(),
        };
        app.log_buffer.push(warn.clone());
        app.log_buffer.push(warn);
        assert!(app.handle_app_event(AppEvent::LogUpdated));
        assert!(toolbar_text(&app, 120).contains("Logs (2!)"));
        // Nothing new to show
        assert!(!app.handle_app_event(AppEvent::LogUpdated));

        press(&mut app, KeyCode::Char('L'));
        draw(&mut app, 120, 24);
        press(&mut app, KeyCode::Char('L'));

        let text = toolbar_text(&app, 120);
        assert!(
            text.contains(" Logs ") && !text.contains("(2!)"),
            "{text:?}"
        );
    }
}
