use std::borrow::Cow;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use super::app::{App, CommandStatus, Focus};
use super::keymap::badge;
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
    LeaveTerminal,
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
    key: Cow<'static, str>,
    desc: Cow<'static, str>,
    action: ToolbarAction,
}

impl Shortcut {
    /// A shortcut for `key`, named as in the keymap and shown as its badge
    fn new(key: &'static str, desc: impl Into<Cow<'static, str>>, action: ToolbarAction) -> Self {
        Self {
            key: badge(key),
            desc: desc.into(),
            action,
        }
    }

    /// Width this shortcut occupies: " key " (padded badge) + space + desc
    fn width(&self) -> usize {
        1 + self.key.chars().count() + 1 + 1 + self.desc.chars().count()
    }
}

/// The shortcut kept at the toolbar's right edge, where no other shortcut can push it out
fn help_shortcut() -> Shortcut {
    Shortcut::new("?", "Help", ToolbarAction::ShowHelp)
}

/// The shortcuts for the current state, most useful first, without the pinned help.
fn get_shortcuts(app: &App) -> Vec<Shortcut> {
    let mut shortcuts = Vec::new();

    // Every other key goes to the command
    if app.focus == Focus::Terminal {
        shortcuts.push(Shortcut::new(
            "Ctrl+]",
            "Back",
            ToolbarAction::LeaveTerminal,
        ));
        if !app.active_command_on_alternate_screen() {
            shortcuts.push(Shortcut::new("Esc", "Back", ToolbarAction::LeaveTerminal));
        }
        return shortcuts;
    }

    if app.fullscreen {
        shortcuts.push(Shortcut::new(
            "Ctrl+R",
            "Exit fullscreen",
            ToolbarAction::ToggleFullscreen,
        ));
        shortcuts.push(Shortcut::new(
            "Esc",
            "Back to tree",
            ToolbarAction::BackToTree,
        ));
        shortcuts.push(Shortcut::new("q", "Quit", ToolbarAction::Quit));
        return shortcuts;
    }

    let selected_count = app.selected.len();
    if selected_count > 0 {
        shortcuts.push(Shortcut::new(
            "Enter",
            format!("Run selected ({selected_count})"),
            ToolbarAction::RunSelected,
        ));
    }
    push_node_shortcuts(app, &mut shortcuts);

    shortcuts.push(Shortcut::new("g", "Git select", ToolbarAction::GitSelect));
    shortcuts.push(Shortcut::new(
        "Ctrl+R",
        "Fullscreen",
        ToolbarAction::ToggleFullscreen,
    ));
    if app.active_command_is_running() {
        shortcuts.push(Shortcut::new(
            "Tab",
            "Terminal",
            ToolbarAction::FocusTerminal,
        ));
    }

    if app.search.is_editing() {
        shortcuts.push(Shortcut::new("Esc", "Clear", ToolbarAction::ClearSearch));
        shortcuts.push(Shortcut::new(
            "Enter",
            "Accept",
            ToolbarAction::AcceptSearch,
        ));
    } else if app.search.is_filtering() {
        shortcuts.push(Shortcut::new("/", "Edit filter", ToolbarAction::Search));
        shortcuts.push(Shortcut::new(
            "Esc",
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
    shortcuts.push(Shortcut::new("q", "Quit", ToolbarAction::Quit));
    shortcuts
}

/// The shortcuts for the node under the cursor
fn push_node_shortcuts(app: &App, shortcuts: &mut Vec<Shortcut>) {
    match app.visible_nodes.get(app.cursor).map(|n| &n.kind) {
        Some(NodeKind::Command {
            selected, status, ..
        }) => {
            let toggle_label = if *selected { "Deselect" } else { "Select" };
            shortcuts.push(Shortcut::new(
                "Space",
                toggle_label,
                ToolbarAction::ToggleSpace,
            ));
            shortcuts.push(Shortcut::new("r", "Run", ToolbarAction::Run));
            if matches!(status, CommandStatus::Running) {
                shortcuts.push(Shortcut::new("s", "Stop", ToolbarAction::Stop));
            }
            shortcuts.push(Shortcut::new("c", "Copy", ToolbarAction::Copy));
            if *status != CommandStatus::Pending {
                shortcuts.push(Shortcut::new("x", "Clear", ToolbarAction::Clear));
            }
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
                "Space",
                toggle_label,
                ToolbarAction::ToggleSpace,
            ));
            shortcuts.push(Shortcut::new("r", "Run all", ToolbarAction::Run));
        }
        None => {}
    }
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

/// Builds a toolbar line from the left, with an optional shortcut pinned to the right edge
struct LineBuilder<'a> {
    app: &'a App,
    width: usize,
    spans: Vec<Span<'static>>,
    regions: Vec<ToolbarRegion>,
    x: usize,
}

impl<'a> LineBuilder<'a> {
    fn new(app: &'a App, width: u16) -> Self {
        Self {
            app,
            width: usize::from(width),
            spans: Vec::new(),
            regions: Vec::new(),
            x: 0,
        }
    }

    fn bg() -> Style {
        Style::default().bg(theme::TOOLBAR_BG)
    }

    fn push_text(&mut self, text: String, style: Style) {
        self.x += text.chars().count();
        self.spans.push(Span::styled(text, style));
    }

    /// Put `shortcut` at the current position.
    fn push_shortcut(&mut self, shortcut: &Shortcut) {
        let hovered = self.app.toolbar.hover == Some(self.regions.len());
        let x_start = self.x;
        self.spans.extend(shortcut_spans(shortcut, hovered));
        self.x += shortcut.width();
        #[expect(
            clippy::cast_possible_truncation,
            reason = "toolbar x positions fit in u16"
        )]
        self.regions.push(ToolbarRegion {
            x_start: x_start as u16,
            x_end: self.x as u16,
            action: shortcut.action,
        });
    }

    /// Pad to the right edge, ending with `pinned` if it fits.
    fn finish(mut self, pinned: Option<&Shortcut>) -> (Line<'static>, Vec<ToolbarRegion>) {
        if let Some(pinned) = pinned.filter(|p| self.x + p.width() <= self.width) {
            let gap = self.width - pinned.width() - self.x;
            self.push_text(" ".repeat(gap), Self::bg());
            self.push_shortcut(pinned);
        } else if self.x < self.width {
            self.push_text(" ".repeat(self.width - self.x), Self::bg());
        }
        (Line::from(self.spans), self.regions)
    }
}

/// The status message in place of the shortcuts, with `? Help` kept at the right edge
fn status_line(
    app: &App,
    status: &StatusMessage,
    width: u16,
) -> (Line<'static>, Vec<ToolbarRegion>) {
    let color = match status.level {
        StatusLevel::Info => theme::TOOLBAR_DESC,
        StatusLevel::Warn => Color::Yellow,
        StatusLevel::Error => theme::FAILURE,
    };
    let help = help_shortcut();
    let mut line = LineBuilder::new(app, width);
    // A leading space, the text, and at least one space before the help
    let text = truncate(&status.text, line.width.saturating_sub(help.width() + 2));
    line.push_text(" ".into(), LineBuilder::bg());
    line.push_text(text, Style::default().fg(color).bg(theme::TOOLBAR_BG));
    let fits = line.x < line.width;
    line.finish(fits.then_some(&help))
}

pub fn build_toolbar_line(app: &App, width: u16) -> (Line<'static>, Vec<ToolbarRegion>) {
    // Fullscreen and terminal focus keep their few shortcuts: they tell how to get back
    if let Some(status) = &app.status
        && !app.fullscreen
        && app.focus == Focus::Tree
    {
        return status_line(app, status, width);
    }
    // `?` would go to the focused command
    let help = (app.focus == Focus::Tree).then(help_shortcut);
    let reserved = help.as_ref().map_or(0, |h| SEP.len() + h.width());

    let mut line = LineBuilder::new(app, width);
    for shortcut in get_shortcuts(app) {
        let sep = if line.x > 0 { SEP.len() } else { 0 };
        // Leave out what doesn't fit, but keep trying the shorter ones after it
        if line.x + sep + shortcut.width() + reserved > line.width {
            continue;
        }
        if sep > 0 {
            line.push_text(SEP.into(), LineBuilder::bg());
        }
        line.push_shortcut(&shortcut);
    }
    line.finish(help.as_ref())
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use crossterm::event::KeyCode;
    use log::Level;

    use super::*;
    use crate::logger::LogEntry;
    use crate::tui::app::AppEvent;
    use crate::tui::keymap::KEYMAP;
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

        assert!(toolbar_text(&app, 80).contains("^]  Back"));
    }

    #[test]
    fn toolbar_terminal_focus() {
        let mut app = two_groups();
        app.focus = Focus::Terminal;
        insta::assert_snapshot!(toolbar_text(&app, 80).trim_end());
    }

    /// [`two_groups`] with `a1` selected and the cursor on it
    fn cursor_on_selected_command() -> App {
        let mut app = two_groups();
        draw(&mut app, 80, 24);
        while crate::tui::test_util::cursor_node(&app) != Some("a1") {
            press(&mut app, KeyCode::Char('j'));
        }
        press(&mut app, KeyCode::Char(' '));
        app.rebuild_visible_nodes();
        app
    }

    #[test]
    fn toolbar_80_shows_help() {
        let app = cursor_on_selected_command();
        let (_, regions) = build_toolbar_line(&app, 80);
        let text = toolbar_text(&app, 80);

        assert!(text.ends_with(" ?  Help"), "{text:?}");
        assert_eq!(text.chars().count(), 80);
        let help = regions.last().unwrap();
        assert_eq!((help.action, help.x_end), (ToolbarAction::ShowHelp, 80));
        insta::assert_snapshot!(text.trim_end());
    }

    #[test]
    fn narrow_toolbar_skips_long_shortcuts_for_short_ones() {
        let app = cursor_on_selected_command();
        let text = toolbar_text(&app, 84);

        // "Git select" and "Fullscreen" don't fit before the help, but the shorter "Search" does
        assert!(text.contains(" c  Copy "), "{text:?}");
        assert!(!text.contains("Git select"), "{text:?}");
        assert!(text.contains(" /  Search "), "{text:?}");
        assert!(text.ends_with(" ?  Help"), "{text:?}");
    }

    #[test]
    fn toolbar_keys_are_in_keymap() {
        let badges: Vec<Cow<str>> = KEYMAP
            .iter()
            .flat_map(|k| k.keys.split(" / "))
            .map(badge)
            .collect();
        let check = |app: &App| {
            for shortcut in get_shortcuts(app).iter().chain([&help_shortcut()]) {
                assert!(badges.contains(&shortcut.key), "{}", shortcut.key);
            }
        };

        let mut app = cursor_on_selected_command();
        check(&app);
        app.fullscreen = true;
        check(&app);
        app.fullscreen = false;
        app.focus = Focus::Terminal;
        check(&app);
        app.focus = Focus::Tree;
        press(&mut app, KeyCode::Char('/'));
        check(&app);
        press(&mut app, KeyCode::Enter);
        check(&app);
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
