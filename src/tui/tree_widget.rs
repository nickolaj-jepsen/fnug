use crate::theme;
use crate::tui::app::CommandStatus;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

/// The type and display data for a tree node
#[derive(Debug, Clone)]
pub enum NodeKind {
    Group {
        name: String,
        expanded: bool,
        success: u16,
        running: u16,
        failure: u16,
        selected: u16,
        total: u16,
    },
    Command {
        name: String,
        selected: bool,
        status: CommandStatus,
        duration: Option<std::time::Duration>,
    },
}

/// A flattened tree node ready for rendering
#[derive(Debug, Clone)]
pub struct VisibleNode {
    pub id: String,
    pub depth: usize,
    pub is_last_sibling: bool,
    /// For each ancestor depth (0..depth), whether that ancestor was the last sibling.
    /// Used to decide between drawing `│ ` (continuation) or `  ` (blank).
    pub ancestor_is_last: Vec<bool>,
    pub kind: NodeKind,
}

/// Ratatui widget that renders the command tree
pub struct TreeWidget<'a> {
    nodes: &'a [VisibleNode],
    cursor: usize,
    scroll_offset: usize,
    hover_row: Option<usize>,
}

impl<'a> TreeWidget<'a> {
    #[must_use]
    pub fn new(
        nodes: &'a [VisibleNode],
        cursor: usize,
        scroll_offset: usize,
        hover_row: Option<usize>,
    ) -> Self {
        Self {
            nodes,
            cursor,
            scroll_offset,
            hover_row,
        }
    }
}

/// Format a duration for display in the tree
fn format_duration(d: std::time::Duration) -> String {
    let secs = d.as_secs();
    if secs >= 60 {
        let mins = secs / 60;
        let rem = secs % 60;
        format!(" ({mins}m {rem}s)")
    } else {
        let millis = d.as_millis();
        if millis < 1000 {
            format!(" ({millis}ms)")
        } else {
            format!(" ({:.1}s)", d.as_secs_f64())
        }
    }
}

/// Build the prefix string (tree guides) for a node, without styling.
#[cfg(test)]
fn build_prefix(node: &VisibleNode) -> String {
    let mut prefix = String::new();
    for &level in &node.ancestor_is_last {
        if level {
            prefix.push_str("  ");
        } else {
            prefix.push_str("│ ");
        }
    }
    if node.depth > 0 {
        if node.is_last_sibling {
            prefix.push_str("└─");
        } else {
            prefix.push_str("├─");
        }
    }
    prefix
}

/// Build the plain text representation of a node (prefix + content), for testing.
#[cfg(test)]
#[must_use]
pub fn render_node_text(node: &VisibleNode) -> String {
    use std::fmt::Write;

    let mut text = build_prefix(node);
    match &node.kind {
        NodeKind::Group {
            name,
            expanded,
            selected,
            total,
            ..
        } => {
            let arrow = if *expanded { "▼ " } else { "▶ " };
            text.push_str(arrow);
            text.push_str(name);
            let _ = write!(text, " ({selected}/{total})");
        }
        NodeKind::Command {
            name,
            selected,
            status,
            duration,
        } => {
            let indicator = if *selected { "● " } else { "○ " };
            text.push_str(indicator);
            text.push_str(name);
            match status {
                CommandStatus::Success => text.push_str(" ✔"),
                CommandStatus::Failure(_) => text.push_str(" ✘"),
                CommandStatus::Running => text.push_str(" ⧗"),
                CommandStatus::Error(_) => text.push_str(" ⚠"),
                CommandStatus::WaitingForDeps => text.push_str(" ◌"),
                CommandStatus::Pending => {}
            }
            if let Some(d) = duration {
                text.push_str(&format_duration(*d));
            }
        }
    }
    text
}

impl Widget for TreeWidget<'_> {
    #[expect(
        clippy::too_many_lines,
        reason = "tree rendering builds styled spans for each node type"
    )]
    fn render(self, area: Rect, buf: &mut Buffer) {
        let visible_height = area.height as usize;
        let visible_nodes = self
            .nodes
            .iter()
            .skip(self.scroll_offset)
            .take(visible_height);

        for (i, node) in visible_nodes.enumerate() {
            #[expect(
                clippy::cast_possible_truncation,
                reason = "row index bounded by area.height which is u16"
            )]
            let y = area.y + i as u16;
            let node_index = i + self.scroll_offset;
            let is_highlighted = node_index == self.cursor;

            let is_hovered = self.hover_row == Some(node_index) && !is_highlighted;

            let mut spans = Vec::new();
            let highlight_style = if is_highlighted {
                Style::default()
                    .fg(theme::ACCENT)
                    .add_modifier(Modifier::UNDERLINED)
            } else if is_hovered {
                Style::default().fg(Color::White)
            } else {
                Style::default()
            };

            // Indentation with vertical continuation lines
            let tree_style = Style::default().fg(theme::TREE);
            for &level in &node.ancestor_is_last {
                let connector = if level { "  " } else { "│ " };
                spans.push(Span::styled(connector, tree_style));
            }

            // Tree guide for this node
            if node.depth > 0 {
                let guide = if node.is_last_sibling {
                    "└─"
                } else {
                    "├─"
                };
                spans.push(Span::styled(guide, tree_style));
            }

            match &node.kind {
                NodeKind::Group {
                    name,
                    expanded,
                    success,
                    running,
                    failure,
                    selected,
                    total,
                } => {
                    let arrow = if *expanded { "▼ " } else { "▶ " };
                    spans.push(Span::styled(arrow, highlight_style));
                    spans.push(Span::styled(name, highlight_style));

                    // Status counts
                    let mut status_parts = Vec::new();
                    if *success > 0 {
                        status_parts.push(Span::styled(
                            format!("{success}"),
                            Style::default().fg(theme::SUCCESS),
                        ));
                    }
                    if *running > 0 {
                        if !status_parts.is_empty() {
                            status_parts.push(Span::styled("|", Style::default().fg(theme::DIM)));
                        }
                        status_parts.push(Span::styled(
                            format!("{running}"),
                            Style::default().fg(theme::RUNNING),
                        ));
                    }
                    if *failure > 0 {
                        if !status_parts.is_empty() {
                            status_parts.push(Span::styled("|", Style::default().fg(theme::DIM)));
                        }
                        status_parts.push(Span::styled(
                            format!("{failure}"),
                            Style::default().fg(theme::FAILURE),
                        ));
                    }
                    if !status_parts.is_empty() {
                        spans.push(Span::styled(" [", Style::default().fg(theme::DIM)));
                        spans.extend(status_parts);
                        spans.push(Span::styled("]", Style::default().fg(theme::DIM)));
                    }

                    // Selection count
                    spans.push(Span::styled(
                        format!(" ({selected}/{total})"),
                        Style::default().fg(theme::DIM),
                    ));
                }
                NodeKind::Command {
                    name,
                    selected,
                    status,
                    duration,
                } => {
                    // Selection indicator
                    let indicator = if *selected { "● " } else { "○ " };
                    let indicator_style = if *selected {
                        highlight_style.fg(theme::ACCENT)
                    } else {
                        highlight_style.fg(theme::DIM)
                    };
                    spans.push(Span::styled(indicator, indicator_style));
                    spans.push(Span::styled(name, highlight_style));

                    // Status icon
                    let status_span = match status {
                        CommandStatus::Pending => None,
                        CommandStatus::Running => {
                            Some(Span::styled(" ⧗", Style::default().fg(theme::RUNNING)))
                        }
                        CommandStatus::Success => {
                            Some(Span::styled(" ✔", Style::default().fg(theme::SUCCESS)))
                        }
                        CommandStatus::Failure(_) => {
                            Some(Span::styled(" ✘", Style::default().fg(theme::FAILURE)))
                        }
                        CommandStatus::Error(_) => {
                            Some(Span::styled(" ⚠", Style::default().fg(theme::FAILURE)))
                        }
                        CommandStatus::WaitingForDeps => {
                            Some(Span::styled(" ◌", Style::default().fg(theme::RUNNING)))
                        }
                    };
                    spans.extend(status_span);

                    // Duration
                    if let Some(d) = duration {
                        spans.push(Span::styled(
                            format_duration(*d),
                            Style::default().fg(theme::DIM),
                        ));
                    }
                }
            }

            let line = Line::from(spans);
            let line_area = Rect::new(area.x, y, area.width, 1);
            buf.set_line(line_area.x, line_area.y, &line, line_area.width);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(clippy::too_many_arguments)]
    fn group_node(
        id: &str,
        name: &str,
        depth: usize,
        is_last: bool,
        ancestors: Vec<bool>,
        expanded: bool,
        selected: u16,
        total: u16,
    ) -> VisibleNode {
        VisibleNode {
            id: id.to_string(),
            depth,
            is_last_sibling: is_last,
            ancestor_is_last: ancestors,
            kind: NodeKind::Group {
                name: name.to_string(),
                expanded,
                success: 0,
                running: 0,
                failure: 0,
                selected,
                total,
            },
        }
    }

    fn cmd_node(
        id: &str,
        name: &str,
        depth: usize,
        is_last: bool,
        ancestors: Vec<bool>,
        selected: bool,
        status: CommandStatus,
    ) -> VisibleNode {
        VisibleNode {
            id: id.to_string(),
            depth,
            is_last_sibling: is_last,
            ancestor_is_last: ancestors,
            kind: NodeKind::Command {
                name: name.to_string(),
                selected,
                status,
                duration: None,
            },
        }
    }

    #[test]
    fn test_command_status_icons() {
        let success = cmd_node("s", "test", 1, false, vec![], false, CommandStatus::Success);
        assert!(render_node_text(&success).ends_with(" ✔"));

        let failure = cmd_node(
            "f",
            "test",
            1,
            false,
            vec![],
            false,
            CommandStatus::Failure(1),
        );
        assert!(render_node_text(&failure).ends_with(" ✘"));

        let running = cmd_node("r", "test", 1, false, vec![], false, CommandStatus::Running);
        assert!(render_node_text(&running).ends_with(" ⧗"));
    }

    #[test]
    fn test_collapsed_group() {
        let node = group_node("r", "rust", 1, false, vec![], false, 4, 4);
        assert_eq!(render_node_text(&node), "├─▶ rust (4/4)");
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn test_full_tree_alignment() {
        // Simulate the full .fnug.yaml tree structure and verify column alignment
        let nodes = vec![
            group_node("root", "fnug", 0, true, vec![], true, 4, 10),
            group_node("rust", "rust", 1, false, vec![], true, 4, 4),
            cmd_node(
                "fmt",
                "fmt",
                2,
                false,
                vec![false],
                true,
                CommandStatus::Success,
            ),
            cmd_node(
                "cwd",
                "cwd",
                2,
                true,
                vec![false],
                true,
                CommandStatus::Success,
            ),
            group_node("debug", "debug", 1, true, vec![], true, 0, 6),
            group_node("na", "nested-auto", 2, false, vec![true], true, 0, 2),
            cmd_node(
                "ta",
                "test-auto",
                3,
                false,
                vec![true, false],
                false,
                CommandStatus::Pending,
            ),
            cmd_node(
                "tna",
                "test-not-auto",
                3,
                true,
                vec![true, false],
                false,
                CommandStatus::Pending,
            ),
            group_node("ne", "not-expanded", 2, false, vec![true], true, 0, 1),
            cmd_node(
                "tne",
                "test-not-expanded",
                3,
                true,
                vec![true, false],
                false,
                CommandStatus::Pending,
            ),
            cmd_node(
                "htop",
                "htop",
                2,
                false,
                vec![true],
                false,
                CommandStatus::Pending,
            ),
            cmd_node(
                "rec",
                "recursive?!",
                2,
                false,
                vec![true],
                false,
                CommandStatus::Pending,
            ),
            cmd_node(
                "long",
                "A very long",
                2,
                true,
                vec![true],
                false,
                CommandStatus::Pending,
            ),
        ];

        let lines: Vec<String> = nodes.iter().map(render_node_text).collect();
        let expected = vec![
            "▼ fnug (4/10)",
            "├─▼ rust (4/4)",
            "│ ├─● fmt ✔",
            "│ └─● cwd ✔",
            "└─▼ debug (0/6)",
            "  ├─▼ nested-auto (0/2)",
            "  │ ├─○ test-auto",
            "  │ └─○ test-not-auto",
            "  ├─▼ not-expanded (0/1)",
            "  │ └─○ test-not-expanded",
            "  ├─○ htop",
            "  ├─○ recursive?!",
            "  └─○ A very long",
        ];
        assert_eq!(lines, expected);
    }
}
