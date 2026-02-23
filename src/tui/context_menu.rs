use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

use crate::theme;
use crate::tui::app::CommandStatus;

/// What the context menu was opened on
#[derive(Debug, Clone)]
pub enum ContextMenuTarget {
    Group {
        id: String,
        expanded: bool,
        selected: u16,
        total: u16,
    },
    Command {
        id: String,
        selected: bool,
        status: CommandStatus,
    },
    Terminal,
}

/// Actions that can be triggered from the context menu
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextMenuAction {
    Expand,
    Collapse,
    SelectAll,
    DeselectAll,
    RunSelected,
    Select,
    Deselect,
    Run,
    Restart,
    Stop,
    Clear,
    ScrollToTop,
    ScrollToBottom,
}

/// A single item in the context menu
#[derive(Debug, Clone)]
pub struct ContextMenuItem {
    pub label: &'static str,
    pub hint: &'static str,
    pub action: ContextMenuAction,
    pub enabled: bool,
}

/// The context menu state
#[derive(Debug, Clone)]
pub struct ContextMenu {
    pub target: ContextMenuTarget,
    pub items: Vec<ContextMenuItem>,
    pub cursor: usize,
    pub area: Rect,
}

impl ContextMenu {
    /// Move cursor up, skipping disabled items
    pub fn cursor_up(&mut self) {
        if self.items.is_empty() {
            return;
        }
        let mut next = self.cursor;
        loop {
            if next == 0 {
                break;
            }
            next -= 1;
            if self.items[next].enabled {
                self.cursor = next;
                break;
            }
        }
    }

    /// Move cursor down, skipping disabled items
    pub fn cursor_down(&mut self) {
        if self.items.is_empty() {
            return;
        }
        let mut next = self.cursor;
        loop {
            if next + 1 >= self.items.len() {
                break;
            }
            next += 1;
            if self.items[next].enabled {
                self.cursor = next;
                break;
            }
        }
    }

    /// Get the action of the currently selected item (if enabled)
    #[must_use]
    pub fn selected_action(&self) -> Option<ContextMenuAction> {
        self.items
            .get(self.cursor)
            .filter(|item| item.enabled)
            .map(|item| item.action)
    }
}

// Menu builders

#[must_use]
pub fn build_group_menu(expanded: bool, selected: u16, total: u16) -> Vec<ContextMenuItem> {
    let all_selected = selected == total && total > 0;
    vec![
        if expanded {
            ContextMenuItem {
                label: "Collapse",
                hint: "h",
                action: ContextMenuAction::Collapse,
                enabled: true,
            }
        } else {
            ContextMenuItem {
                label: "Expand",
                hint: "l",
                action: ContextMenuAction::Expand,
                enabled: true,
            }
        },
        if all_selected {
            ContextMenuItem {
                label: "Deselect all",
                hint: "Space",
                action: ContextMenuAction::DeselectAll,
                enabled: true,
            }
        } else {
            ContextMenuItem {
                label: "Select all",
                hint: "Space",
                action: ContextMenuAction::SelectAll,
                enabled: true,
            }
        },
        ContextMenuItem {
            label: "Run group",
            hint: "r",
            action: ContextMenuAction::Run,
            enabled: true,
        },
        ContextMenuItem {
            label: "Run selected",
            hint: "",
            action: ContextMenuAction::RunSelected,
            enabled: selected > 0,
        },
    ]
}

#[must_use]
pub fn build_command_menu(selected: bool, status: &CommandStatus) -> Vec<ContextMenuItem> {
    let is_running = matches!(status, CommandStatus::Running);
    let has_finished = matches!(
        status,
        CommandStatus::Success | CommandStatus::Failure(_) | CommandStatus::Error(_)
    );
    vec![
        if selected {
            ContextMenuItem {
                label: "Deselect",
                hint: "Space",
                action: ContextMenuAction::Deselect,
                enabled: true,
            }
        } else {
            ContextMenuItem {
                label: "Select",
                hint: "Space",
                action: ContextMenuAction::Select,
                enabled: true,
            }
        },
        ContextMenuItem {
            label: "Run",
            hint: "r",
            action: ContextMenuAction::Run,
            enabled: !is_running,
        },
        ContextMenuItem {
            label: "Restart",
            hint: "",
            action: ContextMenuAction::Restart,
            enabled: has_finished || is_running,
        },
        ContextMenuItem {
            label: "Stop",
            hint: "s",
            action: ContextMenuAction::Stop,
            enabled: is_running,
        },
        ContextMenuItem {
            label: "Clear",
            hint: "c",
            action: ContextMenuAction::Clear,
            enabled: true,
        },
    ]
}

#[must_use]
pub fn build_terminal_menu(
    has_scrollback: bool,
    is_scrolled: bool,
    status: Option<&CommandStatus>,
) -> Vec<ContextMenuItem> {
    let has_process = status.is_some();
    let is_running = status.is_some_and(|s| matches!(s, CommandStatus::Running));
    let mut items = Vec::new();

    if has_process {
        items.push(ContextMenuItem {
            label: "Scroll to top",
            hint: "",
            action: ContextMenuAction::ScrollToTop,
            enabled: has_scrollback,
        });
        items.push(ContextMenuItem {
            label: "Scroll to bottom",
            hint: "",
            action: ContextMenuAction::ScrollToBottom,
            enabled: is_scrolled,
        });
        items.push(ContextMenuItem {
            label: "Restart",
            hint: "r",
            action: ContextMenuAction::Restart,
            enabled: true,
        });
        items.push(ContextMenuItem {
            label: "Stop",
            hint: "s",
            action: ContextMenuAction::Stop,
            enabled: is_running,
        });
    } else {
        items.push(ContextMenuItem {
            label: "Run",
            hint: "r",
            action: ContextMenuAction::Run,
            enabled: true,
        });
    }

    items.push(ContextMenuItem {
        label: "Clear",
        hint: "c",
        action: ContextMenuAction::Clear,
        enabled: has_process,
    });

    items
}

/// Compute the menu rectangle, flipping left/up if it would overflow screen bounds
#[must_use]
#[expect(
    clippy::cast_possible_truncation,
    reason = "menu dimensions fit in u16"
)]
pub fn compute_area(items: &[ContextMenuItem], click_x: u16, click_y: u16, screen: Rect) -> Rect {
    // Calculate menu dimensions
    let max_label = items
        .iter()
        .map(|i| {
            i.label.len()
                + if i.hint.is_empty() {
                    0
                } else {
                    2 + i.hint.len()
                }
        })
        .max()
        .unwrap_or(0);
    let width = (max_label + 4).max(12) as u16; // +4 for padding and borders
    let height = items.len() as u16 + 2; // +2 for borders

    // Position: prefer below-right of click, flip if needed
    let x = if click_x + width <= screen.width {
        click_x
    } else {
        click_x.saturating_sub(width)
    };
    let y = if click_y + height <= screen.height {
        click_y
    } else {
        click_y.saturating_sub(height)
    };

    Rect::new(
        x,
        y,
        width.min(screen.width - x),
        height.min(screen.height - y),
    )
}

impl Widget for &ContextMenu {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let menu_area = self.area;

        // Clamp to the available area
        let x = menu_area.x.max(area.x);
        let y = menu_area.y.max(area.y);
        let w = menu_area.width.min(area.right().saturating_sub(x));
        let h = menu_area.height.min(area.bottom().saturating_sub(y));
        if w == 0 || h == 0 {
            return;
        }
        let rect = Rect::new(x, y, w, h);

        // Dim entire background to ~50% by overwriting fg colors with DarkGray
        let dim_style = Style::default()
            .fg(Color::DarkGray)
            .remove_modifier(Modifier::BOLD | Modifier::UNDERLINED);
        for row in area.y..area.bottom() {
            for col in area.x..area.right() {
                buf[(col, row)].set_style(dim_style);
            }
        }

        // Clear menu background (reset any inherited styles like UNDERLINED)
        let reset_style = Style::reset().bg(Color::Rgb(30, 30, 30)).fg(Color::White);
        for row in rect.y..rect.bottom() {
            for col in rect.x..rect.right() {
                buf[(col, row)].set_style(reset_style).set_symbol(" ");
            }
        }

        // Draw border
        let border_style = Style::reset().fg(theme::ACCENT).bg(Color::Rgb(30, 30, 30));
        // Top/bottom borders
        for col in rect.x..rect.right() {
            buf[(col, rect.y)].set_style(border_style).set_symbol("─");
            buf[(col, rect.bottom() - 1)]
                .set_style(border_style)
                .set_symbol("─");
        }
        // Left/right borders
        for row in rect.y..rect.bottom() {
            buf[(rect.x, row)].set_style(border_style).set_symbol("│");
            buf[(rect.right() - 1, row)]
                .set_style(border_style)
                .set_symbol("│");
        }
        // Corners
        buf[(rect.x, rect.y)]
            .set_style(border_style)
            .set_symbol("┌");
        buf[(rect.right() - 1, rect.y)]
            .set_style(border_style)
            .set_symbol("┐");
        buf[(rect.x, rect.bottom() - 1)]
            .set_style(border_style)
            .set_symbol("└");
        buf[(rect.right() - 1, rect.bottom() - 1)]
            .set_style(border_style)
            .set_symbol("┘");

        // Draw items
        let inner_x = rect.x + 1;
        let inner_w = rect.width.saturating_sub(2);
        for (i, item) in self.items.iter().enumerate() {
            #[expect(
                clippy::cast_possible_truncation,
                reason = "menu item index bounded by area height which is u16"
            )]
            let row = rect.y + 1 + i as u16;
            if row >= rect.bottom() - 1 {
                break;
            }

            let is_selected = i == self.cursor;
            let style = if !item.enabled {
                Style::reset()
                    .fg(Color::DarkGray)
                    .bg(Color::Rgb(30, 30, 30))
            } else if is_selected {
                Style::reset()
                    .fg(Color::White)
                    .bg(theme::ACCENT)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::reset().fg(Color::White).bg(Color::Rgb(30, 30, 30))
            };

            // Fill row background
            for col in inner_x..inner_x + inner_w {
                buf[(col, row)].set_style(style).set_symbol(" ");
            }

            // Render label + hint as a Line
            let mut spans = vec![Span::raw(" "), Span::styled(item.label, style)];
            if !item.hint.is_empty() {
                let hint_style = if is_selected && item.enabled {
                    Style::reset().fg(Color::Rgb(30, 30, 30)).bg(theme::ACCENT)
                } else {
                    Style::reset()
                        .fg(Color::DarkGray)
                        .bg(Color::Rgb(30, 30, 30))
                };
                // Pad between label and hint
                let used = 1 + item.label.len() + item.hint.len() + 1;
                let padding = inner_w as usize - used.min(inner_w as usize);
                if padding > 0 {
                    spans.push(Span::styled(" ".repeat(padding), style));
                }
                spans.push(Span::styled(item.hint, hint_style));
            }

            let line = Line::from(spans);
            buf.set_line(inner_x, row, &line, inner_w);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_group_menu_collapsed() {
        let items = build_group_menu(false, 0, 4);
        assert_eq!(items[0].label, "Expand");
        assert_eq!(items[1].label, "Select all");
        assert_eq!(items[2].label, "Run group");
        assert!(!items[3].enabled); // Run selected disabled when none selected
    }

    #[test]
    fn test_build_group_menu_expanded_all_selected() {
        let items = build_group_menu(true, 4, 4);
        assert_eq!(items[0].label, "Collapse");
        assert_eq!(items[1].label, "Deselect all");
        assert!(items[3].enabled); // Run selected enabled
    }

    #[test]
    fn test_build_command_menu_not_running() {
        let items = build_command_menu(false, &CommandStatus::Pending);
        assert_eq!(items[0].label, "Select");
        assert!(items[1].label == "Run" && items[1].enabled);
        assert!(!items[2].enabled); // Restart disabled when pending
        assert!(!items[3].enabled); // Stop disabled when not running
    }

    #[test]
    fn test_build_command_menu_running() {
        let items = build_command_menu(true, &CommandStatus::Running);
        assert_eq!(items[0].label, "Deselect");
        assert!(!items[1].enabled); // Run disabled when running
        assert!(items[2].label == "Restart" && items[2].enabled);
        assert!(items[3].label == "Stop" && items[3].enabled);
    }

    #[test]
    fn test_build_command_menu_finished() {
        let items = build_command_menu(false, &CommandStatus::Failure(1));
        assert!(items[1].label == "Run" && items[1].enabled);
        assert!(items[2].label == "Restart" && items[2].enabled);
        assert!(!items[3].enabled); // Stop disabled when not running
    }

    #[test]
    fn test_build_terminal_menu_no_process() {
        let items = build_terminal_menu(false, false, None);
        assert_eq!(items[0].label, "Run");
        assert!(items[0].enabled);
        assert_eq!(items[1].label, "Clear");
        assert!(!items[1].enabled); // Clear disabled when no process
    }

    #[test]
    fn test_build_terminal_menu_with_scrollback() {
        let items = build_terminal_menu(true, true, Some(&CommandStatus::Running));
        assert!(items[0].label == "Scroll to top" && items[0].enabled);
        assert!(items[1].label == "Scroll to bottom" && items[1].enabled);
        assert_eq!(items[2].label, "Restart");
        assert!(items[3].label == "Stop" && items[3].enabled);
    }

    #[test]
    fn test_build_terminal_menu_not_scrolled() {
        let items = build_terminal_menu(true, false, Some(&CommandStatus::Success));
        assert!(items[0].enabled); // Scroll to top
        assert!(!items[1].enabled); // Scroll to bottom disabled when at bottom
        assert!(!items[3].enabled); // Stop disabled when not running
    }

    #[test]
    fn test_compute_area_fits() {
        let items = build_terminal_menu(true, false, Some(&CommandStatus::Running));
        let area = compute_area(&items, 10, 10, Rect::new(0, 0, 80, 24));
        assert_eq!(area.x, 10);
        assert_eq!(area.y, 10);
    }

    #[test]
    fn test_compute_area_flips() {
        let items = build_terminal_menu(true, false, Some(&CommandStatus::Running));
        let area = compute_area(&items, 75, 22, Rect::new(0, 0, 80, 24));
        // Should flip left and/or up
        assert!(area.x < 75 || area.y < 22);
    }

    #[test]
    fn test_cursor_navigation() {
        let items = vec![
            ContextMenuItem {
                label: "A",
                hint: "",
                action: ContextMenuAction::Run,
                enabled: true,
            },
            ContextMenuItem {
                label: "B",
                hint: "",
                action: ContextMenuAction::Stop,
                enabled: false,
            },
            ContextMenuItem {
                label: "C",
                hint: "",
                action: ContextMenuAction::Clear,
                enabled: true,
            },
        ];
        let mut menu = ContextMenu {
            target: ContextMenuTarget::Terminal,
            items,
            cursor: 0,
            area: Rect::default(),
        };

        // Down should skip disabled item B
        menu.cursor_down();
        assert_eq!(menu.cursor, 2);

        // Up should skip disabled item B
        menu.cursor_up();
        assert_eq!(menu.cursor, 0);
    }
}
