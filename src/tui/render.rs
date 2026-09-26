use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap};

use super::app::{App, CommandStatus, Focus};
use super::selection::SelectionReason;

fn render_scrollbar(frame: &mut Frame, area: Rect, total: usize, position: usize) {
    let mut state = ScrollbarState::new(total).position(position);
    frame.render_stateful_widget(
        Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .thumb_style(Style::default().fg(theme::ACCENT)),
        area,
        &mut state,
    );
}
use super::log_state::level_color;
use super::terminal_widget::PseudoTerminal;
use super::toolbar;
use super::tree_widget::TreeWidget;
use crate::runner::NodeState;
use crate::theme;

impl App {
    /// Render the app
    pub fn render(&mut self, frame: &mut Frame) -> (Rect, Rect) {
        if self.tree_dirty {
            self.rebuild_visible_nodes();
        }
        let size = frame.area();

        // Outer vertical split: main area + toolbar
        let outer = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(3), Constraint::Length(1)])
            .split(size);

        let main_area = outer[0];
        let toolbar_area = outer[1];

        // Render toolbar
        let (toolbar_line, regions) = toolbar::build_toolbar_line(self, toolbar_area.width);
        self.toolbar.regions = regions;
        self.toolbar.y = toolbar_area.y;
        frame.render_widget(Paragraph::new(toolbar_line), toolbar_area);

        if self.fullscreen {
            // Fullscreen terminal mode
            let terminal_area = main_area;
            self.render_terminal(frame, terminal_area);
            if let Some(ref menu) = self.context_menu {
                frame.render_widget(menu, frame.area());
            }
            if self.show_help {
                self.render_help_overlay(frame);
            }
            return (Rect::default(), terminal_area);
        }

        // Split into tree and terminal panels
        let tree_width = self.tree_width.min(main_area.width.saturating_sub(20));
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(tree_width),
                Constraint::Length(1), // separator
                Constraint::Min(20),
            ])
            .split(main_area);

        let tree_area = chunks[0];
        let separator_area = chunks[1];
        let terminal_area = chunks[2];

        // Split tree area: [search_bar?, tree_widget]
        let has_search = self.search.has_query();
        let tree_sub = Layout::default()
            .direction(Direction::Vertical)
            .constraints(if has_search {
                vec![Constraint::Length(1), Constraint::Min(1)]
            } else {
                vec![Constraint::Min(1)]
            })
            .split(tree_area);

        let (search_area, actual_tree_area) = if has_search {
            (Some(tree_sub[0]), tree_sub[1])
        } else {
            (None, tree_sub[0])
        };

        // Render search bar
        if let Some(search_area) = search_area {
            let query = self.search.query().unwrap_or("");
            let search_line = if self.search.is_editing() {
                Line::from(vec![
                    Span::styled("/ ", Style::default().fg(theme::ACCENT)),
                    Span::raw(query),
                    Span::styled("█", Style::default().fg(theme::ACCENT)),
                ])
            } else {
                Line::from(vec![
                    Span::styled("/ ", Style::default().fg(Color::DarkGray)),
                    Span::styled(query, Style::default().fg(Color::DarkGray)),
                ])
            };
            frame.render_widget(Paragraph::new(search_line), search_area);
        }

        let tree_height = usize::from(actual_tree_area.height);
        if self.last_scroll_anchor == Some((self.cursor, tree_height)) {
            self.clamp_tree_scroll(tree_height);
        } else {
            self.last_scroll_anchor = Some((self.cursor, tree_height));
            self.ensure_cursor_visible(tree_height);
        }
        let tree_widget = TreeWidget::new(
            &self.visible_nodes,
            self.cursor,
            self.tree_scroll,
            self.mouse.hover_row,
        );
        frame.render_widget(tree_widget, actual_tree_area);

        self.render_separator(frame, separator_area);

        // Render right pane: log panel or terminal
        if self.show_logs {
            self.render_log_panel(frame, terminal_area);
        } else {
            self.render_terminal(frame, terminal_area);
        }

        // Render context menu overlay last (on top of everything)
        if let Some(ref menu) = self.context_menu {
            frame.render_widget(menu, frame.area());
        }

        // Render help overlay on top of everything
        if self.show_help {
            self.render_help_overlay(frame);
        }

        (tree_area, terminal_area)
    }

    /// The line between tree and terminal, coloured like a running command while the terminal
    /// has the keyboard
    fn render_separator(&self, frame: &mut Frame, area: Rect) {
        let color = match self.focus {
            Focus::Terminal => theme::RUNNING,
            Focus::Tree => theme::ACCENT,
        };
        let lines: Vec<Line> = (0..area.height)
            .map(|_| Line::from(Span::styled("│", Style::default().fg(color))))
            .collect();
        frame.render_widget(Paragraph::new(lines), area);
    }

    fn render_terminal(&self, frame: &mut Frame, area: Rect) {
        if let Some(ref active_id) = self.active_terminal_id {
            let run = self.run_summary(active_id);
            if let CommandStatus::Error(message) = run.status {
                let error = Paragraph::new(message).style(Style::default().fg(theme::FAILURE));
                frame.render_widget(error, area);
                return;
            }

            if !run.waiting_on.is_empty() {
                let mut lines: Vec<Line> = vec![
                    Line::from(""),
                    Line::from(Span::styled(
                        " ❱ Waiting for dependencies:",
                        Style::default()
                            .fg(theme::RUNNING)
                            .add_modifier(Modifier::BOLD),
                    )),
                    Line::from(""),
                ];
                for dep_id in &run.waiting_on {
                    // Only dependencies still to run or finish are waited on
                    let (label, color) = match self.dag.state(dep_id) {
                        Some(NodeState::Running) => ("running", theme::RUNNING),
                        _ => ("waiting", theme::DIM),
                    };
                    let name = self
                        .find_command(dep_id)
                        .map_or_else(|| dep_id.clone(), |c| c.name);
                    lines.push(Line::from(vec![
                        Span::raw("   ◌ "),
                        Span::styled(name, Style::default().fg(Color::White)),
                        Span::styled(format!(" ({label})"), Style::default().fg(color)),
                    ]));
                }
                frame.render_widget(Paragraph::new(lines), area);
                return;
            }

            if let Some(proc) = self.processes.get(active_id) {
                let parser = proc.terminal.parser().lock();
                let screen = parser.screen();
                let pseudo_term = PseudoTerminal::new(screen);
                frame.render_widget(pseudo_term, area);

                let scrollback_len = screen.scrollback_len();
                if scrollback_len > 0 {
                    let scrollback_pos = screen.scrollback();
                    render_scrollbar(frame, area, scrollback_len, scrollback_len - scrollback_pos);
                }
                return;
            }
        }

        let placeholder = Paragraph::new(self.placeholder_text())
            .style(Style::default().fg(Color::DarkGray))
            .wrap(Wrap { trim: false });
        frame.render_widget(placeholder, area);
    }

    /// What the pane says for a command without output: why it is selected, if it is.
    fn placeholder_text(&self) -> String {
        let Some(id) = self.active_terminal_id.as_deref() else {
            return "No command running. Press 'r' to run a command.".into();
        };
        if !self.selected.contains(id) {
            return "Not run yet. Press 'r' to run it.".into();
        }
        let reason = self
            .selection_reason
            .get(id)
            .unwrap_or(&SelectionReason::Manual);
        format!("{}. Press 'r' to run it.", reason.describe(&self.cwd))
    }

    fn render_log_panel(&mut self, frame: &mut Frame, area: Rect) {
        self.mark_logs_seen();
        let entries = self.log_buffer.entries();
        let count = entries.len();

        // Split into header + content
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(1)])
            .split(area);

        let header_area = chunks[0];
        let content_area = chunks[1];

        // Header
        let header = Line::from(vec![Span::styled(
            format!(" Logs ({count}) "),
            Style::default()
                .fg(theme::ACCENT)
                .add_modifier(Modifier::BOLD),
        )]);
        frame.render_widget(Paragraph::new(header), header_area);

        if entries.is_empty() {
            let empty =
                Paragraph::new("No log messages yet.").style(Style::default().fg(Color::DarkGray));
            frame.render_widget(empty, content_area);
            return;
        }

        let visible_height = content_area.height as usize;
        let max_scroll = count.saturating_sub(visible_height);
        // Stored, so scrolling back down after scrolling past the top moves at once
        self.log_scroll = self.log_scroll.min(max_scroll);
        let scroll = self.log_scroll;

        // Show entries from bottom (newest last), scrolled up by `scroll`
        let start = count.saturating_sub(visible_height + scroll);
        let end = count.saturating_sub(scroll);

        let log_start = self.log_buffer.start();
        let lines: Vec<Line> = entries[start..end]
            .iter()
            .map(|entry| {
                let elapsed = entry.timestamp.duration_since(log_start).as_secs_f64();
                let level_str = format!("{:5}", entry.level);
                Line::from(vec![
                    Span::styled(
                        format!("{elapsed:>6.1}s "),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::styled(level_str, Style::default().fg(level_color(entry.level))),
                    Span::styled(" ", Style::default()),
                    Span::styled(
                        format!("{}: ", entry.target),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::raw(&entry.message),
                ])
            })
            .collect();

        frame.render_widget(Paragraph::new(lines), content_area);

        // Scrollbar
        if count > visible_height {
            render_scrollbar(frame, content_area, max_scroll, max_scroll - scroll);
        }
    }

    fn render_help_overlay(&mut self, frame: &mut Frame) {
        let area = frame.area();
        self.help_scroll = super::help::render_help(frame.buffer_mut(), area, self.help_scroll);
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Instant;

    use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    use ratatui::layout::Rect;

    use crate::logger::LogEntry;
    use crate::tui::app::App;
    use crate::tui::log_state::LogBuffer;
    use crate::tui::test_util::{command, cursor_node, draw, group, press, type_text};

    /// `root` holding commands `c01` to `c{count}`
    fn long_list(count: usize) -> App {
        let commands = (1..=count).map(|i| command(&format!("c{i:02}"))).collect();
        App::new(
            group("root", vec![], commands),
            PathBuf::new(),
            LogBuffer::new(),
        )
    }

    fn wheel(app: &mut App, kind: MouseEventKind, at: Rect, areas: (Rect, Rect)) {
        let event = MouseEvent {
            kind,
            column: at.x + 1,
            row: at.y + 1,
            modifiers: KeyModifiers::NONE,
        };
        app.handle_mouse(event, areas.0, areas.1);
    }

    #[test]
    fn wheel_scroll_survives_render() {
        let mut app = long_list(40);
        let (_, areas) = draw(&mut app, 80, 12);

        wheel(&mut app, MouseEventKind::ScrollDown, areas.0, areas);
        let (rows, _) = draw(&mut app, 80, 12);

        assert_eq!(app.tree_scroll, 5);
        assert!(rows[0].starts_with("├─○ c05"), "{rows:?}");
    }

    #[test]
    fn tree_scroll_clamped() {
        let mut app = long_list(40);
        let (_, areas) = draw(&mut app, 80, 12);
        let height = usize::from(areas.0.height);

        for _ in 0..20 {
            wheel(&mut app, MouseEventKind::ScrollDown, areas.0, areas);
        }
        assert_eq!(app.tree_scroll, 41 - height, "scrolled past the last row");
        let (rows, _) = draw(&mut app, 80, 12);
        assert!(rows[height - 1].starts_with("└─○ c40"), "{rows:?}");
    }

    #[test]
    fn tree_scroll_clamped_under_search_bar() {
        let mut app = long_list(40);
        type_text(&mut app, "/c");
        let (_, areas) = draw(&mut app, 80, 12);

        for _ in 0..20 {
            wheel(&mut app, MouseEventKind::ScrollDown, areas.0, areas);
        }
        let (rows, _) = draw(&mut app, 80, 12);

        // The search bar takes the first of the tree's rows
        let last = usize::from(areas.0.height) - 1;
        assert!(rows[last].starts_with("└─○ c40"), "{rows:?}");
    }

    #[test]
    fn click_under_search_bar_hits_the_row_shown() {
        let mut app = long_list(5);
        type_text(&mut app, "/c");
        let (rows, areas) = draw(&mut app, 80, 12);
        assert!(rows[2].starts_with("├─○ c01"), "{rows:?}");

        let click = |row| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 10,
            row,
            modifiers: KeyModifiers::NONE,
        };
        app.handle_mouse(click(2), areas.0, areas.1);
        assert_eq!(cursor_node(&app), Some("c01"));

        // The search bar itself holds no node
        app.handle_mouse(click(0), areas.0, areas.1);
        assert_eq!(cursor_node(&app), Some("c01"));
    }

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    #[test]
    fn divider_drag_ends_without_release_in_the_panes() {
        let mut app = long_list(3);
        let (_, areas) = draw(&mut app, 80, 12);
        let divider = areas.0.width;
        let press = mouse(MouseEventKind::Down(MouseButton::Left), divider, 2);

        // Released over the toolbar, which handles its own clicks
        app.handle_mouse(press, areas.0, areas.1);
        assert!(app.mouse.resizing);
        let release = MouseEventKind::Up(MouseButton::Left);
        app.handle_mouse(mouse(release, divider, app.toolbar.y), areas.0, areas.1);
        assert!(!app.mouse.resizing, "release on the toolbar");

        // Released outside the window: a move without a button held ends the drag
        app.handle_mouse(press, areas.0, areas.1);
        app.handle_mouse(mouse(MouseEventKind::Moved, 30, 3), areas.0, areas.1);
        assert!(!app.mouse.resizing, "move after a lost release");
    }

    #[test]
    fn fullscreen_click_on_first_column_starts_no_drag() {
        let mut app = long_list(3);
        app.fullscreen = true;
        let (_, areas) = draw(&mut app, 80, 12);
        assert_eq!(areas.0.width, 0);

        let press = MouseEventKind::Down(MouseButton::Left);
        app.handle_mouse(mouse(press, 0, 2), areas.0, areas.1);
        assert!(!app.mouse.resizing);
    }

    #[test]
    fn scrolled_search_not_blank() {
        let mut app = long_list(32);
        draw(&mut app, 80, 13);
        for _ in 0..20 {
            press(&mut app, KeyCode::Char('j'));
        }
        draw(&mut app, 80, 13);
        assert!(app.tree_scroll > 0);

        type_text(&mut app, "/c2");
        let (rows, _) = draw(&mut app, 80, 13);

        // All 11 rows fit below the search bar
        assert_eq!(app.tree_scroll, 0);
        assert!(rows[1].starts_with("▼ root"), "{rows:?}");
    }

    #[test]
    fn log_scroll_written_back() {
        let mut app = long_list(1);
        for i in 0..30 {
            app.log_buffer.push(LogEntry {
                level: log::Level::Info,
                target: "test".into(),
                message: format!("line {i}"),
                timestamp: Instant::now(),
            });
        }
        press(&mut app, KeyCode::Char('L'));
        let (_, areas) = draw(&mut app, 80, 12);

        for _ in 0..20 {
            wheel(&mut app, MouseEventKind::ScrollUp, areas.1, areas);
        }
        draw(&mut app, 80, 12);
        let at_top = app.log_scroll;
        assert!(at_top < 30, "log_scroll {at_top} not clamped");

        // The first notch down moves the view
        wheel(&mut app, MouseEventKind::ScrollDown, areas.1, areas);
        draw(&mut app, 80, 12);
        assert_eq!(app.log_scroll, at_top - 5);
    }
}
