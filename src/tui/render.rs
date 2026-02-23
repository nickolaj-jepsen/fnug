use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState};

use super::app::{App, CommandStatus};

fn render_scrollbar(frame: &mut Frame, area: Rect, total: usize, position: usize) {
    let mut state = ScrollbarState::new(total).position(position);
    frame.render_stateful_widget(
        Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .thumb_style(Style::default().fg(theme::ACCENT)),
        area,
        &mut state,
    );
}
use super::terminal_widget::PseudoTerminal;
use super::toolbar;
use super::tree_widget::TreeWidget;
use crate::{logger, theme};

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

        // Render tree (ensure cursor is visible within the panel height)
        self.ensure_cursor_visible(actual_tree_area.height as usize);
        let tree_widget = TreeWidget::new(
            &self.visible_nodes,
            self.cursor,
            self.tree_scroll,
            self.mouse.hover_row,
        );
        frame.render_widget(tree_widget, actual_tree_area);

        // Render separator
        let separator_lines: Vec<Line> = (0..separator_area.height)
            .map(|_| Line::from(Span::styled("│", Style::default().fg(theme::ACCENT))))
            .collect();
        let separator = Paragraph::new(separator_lines);
        frame.render_widget(separator, separator_area);

        // Render right pane: log panel or terminal
        if self.show_logs {
            self.render_log_panel(frame, terminal_area);
        } else {
            self.render_terminal(frame, terminal_area);
        }

        (tree_area, terminal_area)
    }

    fn render_terminal(&self, frame: &mut Frame, area: Rect) {
        if let Some(ref active_id) = self.active_terminal_id {
            // Check for error messages first
            if let Some(error_msg) = self.error_messages.get(active_id) {
                let error =
                    Paragraph::new(error_msg.clone()).style(Style::default().fg(theme::FAILURE));
                frame.render_widget(error, area);
                return;
            }

            // Check for pending dependencies
            if let Some(dep_ids) = self.pending_deps.get(active_id) {
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
                for dep_id in dep_ids {
                    let (label, color) = match self.processes.get(dep_id).map(|p| &p.status) {
                        Some(CommandStatus::Running) => ("running", theme::RUNNING),
                        Some(CommandStatus::Success) => ("done", theme::SUCCESS),
                        Some(CommandStatus::Failure(_) | CommandStatus::Error(_)) => {
                            ("failed", theme::FAILURE)
                        }
                        _ => ("pending", theme::DIM),
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

        // No active terminal — show placeholder
        let placeholder = Paragraph::new("No command running. Press 'r' to run a command.")
            .style(Style::default().fg(Color::DarkGray));
        frame.render_widget(placeholder, area);
    }

    fn render_log_panel(&self, frame: &mut Frame, area: Rect) {
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
        let scroll = self.log_scroll.min(max_scroll);

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
                    Span::styled(
                        level_str,
                        Style::default().fg(logger::level_color(entry.level)),
                    ),
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
}
