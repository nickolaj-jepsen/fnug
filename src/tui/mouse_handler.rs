use crossterm::event::{self, MouseEvent, MouseEventKind};
use log::debug;
use ratatui::layout::Rect;
use std::time::Instant;

use super::app::{App, Focus, ProcessInstance};
use super::context_menu::{
    ContextMenu, ContextMenuTarget, build_command_menu, build_group_menu, build_terminal_menu,
    compute_area,
};
use super::tree_widget::{NodeKind, VisibleNode};

impl App {
    /// Scroll the terminal to match a scrollbar click/drag position
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss,
        reason = "scroll position fits in usize"
    )]
    fn scroll_to_scrollbar_position(proc: &ProcessInstance, mouse_y: u16, area: Rect) {
        let scrollback_len = {
            let parser = proc.terminal.parser().lock();
            parser.screen().scrollback_len()
        };
        if scrollback_len > 0 {
            let ratio = f64::from(mouse_y.saturating_sub(area.y)) / f64::from(area.height.max(1));
            let ratio = ratio.clamp(0.0, 1.0);
            let target = scrollback_len - (ratio * scrollback_len as f64) as usize;
            if let Err(e) = proc.terminal.set_scroll(target) {
                debug!("Failed to set scroll: {e}");
            }
        }
    }

    /// Compute the column where the orb/arrow starts for a given node
    #[expect(
        clippy::cast_possible_truncation,
        reason = "tree depth never exceeds u16"
    )]
    fn orb_column(node: &VisibleNode) -> u16 {
        let prefix_width = node.ancestor_is_last.len() * 2;
        let guide_width = if node.depth > 0 { 2 } else { 0 };
        (prefix_width + guide_width) as u16
    }

    /// Handle mouse input
    #[expect(
        clippy::too_many_lines,
        reason = "mouse handler covers all mouse interactions in one function"
    )]
    pub fn handle_mouse(&mut self, mouse: MouseEvent, tree_area: Rect, terminal_area: Rect) {
        // Handle context menu interactions when open
        if let Some(ref menu) = self.context_menu {
            let menu_area = menu.area;
            let inside = mouse.column >= menu_area.x
                && mouse.column < menu_area.right()
                && mouse.row >= menu_area.y
                && mouse.row < menu_area.bottom();

            match mouse.kind {
                MouseEventKind::Down(event::MouseButton::Left) => {
                    if inside {
                        let row = (mouse.row - menu_area.y).saturating_sub(1) as usize; // -1 for border
                        if let Some(menu) = self.context_menu.as_mut()
                            && row < menu.items.len()
                        {
                            menu.cursor = row;
                        }
                        self.execute_context_menu_action(terminal_area);
                    } else {
                        self.close_context_menu();
                    }
                    return;
                }
                MouseEventKind::Moved => {
                    if inside {
                        let row = (mouse.row - menu_area.y).saturating_sub(1) as usize;
                        if let Some(menu) = self.context_menu.as_mut()
                            && row < menu.items.len()
                            && menu.items[row].enabled
                        {
                            menu.cursor = row;
                        }
                    }
                    return;
                }
                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                    self.close_context_menu();
                    return;
                }
                _ => {
                    return;
                }
            }
        }

        // Handle toolbar interactions
        if mouse.row >= self.toolbar.y {
            match mouse.kind {
                MouseEventKind::Down(event::MouseButton::Left) => {
                    if let Some(region) = self
                        .toolbar
                        .regions
                        .iter()
                        .find(|r| mouse.column >= r.x_start && mouse.column < r.x_end)
                    {
                        let action = region.action;
                        self.execute_toolbar_action(action, terminal_area);
                    }
                }
                MouseEventKind::Moved => {
                    self.toolbar.hover = self
                        .toolbar
                        .regions
                        .iter()
                        .position(|r| mouse.column >= r.x_start && mouse.column < r.x_end);
                }
                _ => {}
            }
            return;
        }

        // Mouse is not on toolbar, clear hover
        self.toolbar.hover = None;

        match mouse.kind {
            MouseEventKind::Down(event::MouseButton::Left) => {
                match mouse.column.cmp(&tree_area.width) {
                    std::cmp::Ordering::Less => {
                        self.focus = Focus::Tree;
                        let row = mouse.row.saturating_sub(tree_area.y) as usize + self.tree_scroll;
                        if row < self.visible_nodes.len() {
                            let node = self.visible_nodes[row].clone();
                            let orb_col = Self::orb_column(&node);
                            let on_orb = mouse.column >= orb_col && mouse.column < orb_col + 2;

                            // Double-click detection (only outside the orb)
                            let is_double_click = !on_orb
                                && self.mouse.last_click.is_some_and(|(t, r)| {
                                    r == row && t.elapsed().as_millis() < 400
                                });

                            if is_double_click {
                                self.mouse.last_click = None;
                                match &node.kind {
                                    NodeKind::Command { .. } => {
                                        self.cursor = row;
                                        self.update_active_terminal();
                                        self.start_command(&node.id, terminal_area, true);
                                    }
                                    NodeKind::Group { expanded, .. } => {
                                        self.expanded.insert(node.id.clone(), !expanded);
                                        self.mark_tree_dirty();
                                    }
                                }
                            } else {
                                self.mouse.last_click = Some((Instant::now(), row));
                                self.cursor = row;
                                self.update_active_terminal();

                                // Click on orb/arrow toggles the item
                                if on_orb {
                                    self.toggle_current_node();
                                }
                            }
                        }
                    }
                    std::cmp::Ordering::Equal => {
                        // Start resize drag
                        self.mouse.resizing = true;
                    }
                    std::cmp::Ordering::Greater => {
                        // Check if click is on the scrollbar track (rightmost column)
                        if mouse.column == terminal_area.right().saturating_sub(1)
                            && let Some(ref active_id) = self.active_terminal_id
                            && let Some(proc) = self.processes.get(active_id)
                        {
                            let has_scrollback =
                                proc.terminal.parser().lock().screen().scrollback_len() > 0;
                            if has_scrollback {
                                self.mouse.scrollbar_dragging = true;
                                Self::scroll_to_scrollbar_position(proc, mouse.row, terminal_area);
                                if self.active_command_is_interactive() {
                                    self.focus = Focus::Terminal;
                                }
                                return;
                            }
                        }

                        // Only focus terminal for interactive commands
                        if self.active_command_is_interactive() {
                            self.focus = Focus::Terminal;
                            // Forward click to terminal
                            if let Some(ref active_id) = self.active_terminal_id
                                && let Some(proc) = self.processes.get(active_id)
                            {
                                let x = mouse.column.saturating_sub(terminal_area.x);
                                let y = mouse.row.saturating_sub(terminal_area.y);
                                if let Err(e) = proc.terminal.click(x, y) {
                                    debug!("Failed to send click to terminal: {e}");
                                }
                            }
                        }
                    }
                }
            }
            MouseEventKind::Drag(event::MouseButton::Left) => {
                if self.mouse.resizing {
                    self.tree_width = mouse
                        .column
                        .max(10)
                        .min(terminal_area.right().saturating_sub(10));
                } else if self.mouse.scrollbar_dragging
                    && let Some(ref active_id) = self.active_terminal_id
                    && let Some(proc) = self.processes.get(active_id)
                {
                    Self::scroll_to_scrollbar_position(proc, mouse.row, terminal_area);
                }
            }
            MouseEventKind::Up(event::MouseButton::Left) => {
                self.mouse.resizing = false;
                self.mouse.scrollbar_dragging = false;
            }
            MouseEventKind::Moved => {
                if mouse.column < tree_area.width {
                    let row = mouse.row.saturating_sub(tree_area.y) as usize + self.tree_scroll;
                    if row < self.visible_nodes.len() {
                        self.mouse.hover_row = Some(row);
                    } else {
                        self.mouse.hover_row = None;
                    }
                } else {
                    self.mouse.hover_row = None;
                }
            }
            MouseEventKind::ScrollUp => {
                if mouse.column < tree_area.width {
                    self.tree_scroll = self.tree_scroll.saturating_sub(5);
                } else if self.show_logs {
                    self.log_scroll = self.log_scroll.saturating_add(5);
                } else if let Some(ref active_id) = self.active_terminal_id
                    && let Some(proc) = self.processes.get(active_id)
                {
                    let x = mouse.column.saturating_sub(terminal_area.x);
                    let y = mouse.row.saturating_sub(terminal_area.y);
                    match proc.terminal.mouse_scroll(true, x, y) {
                        Ok(true) => {} // forwarded to PTY
                        Ok(false) => {
                            // No mouse protocol — fall back to scrollback
                            if let Err(e) = proc.terminal.scroll(-5) {
                                debug!("Failed to scroll up: {e}");
                            }
                        }
                        Err(e) => debug!("Failed to send scroll to terminal: {e}"),
                    }
                }
            }
            MouseEventKind::ScrollDown => {
                if mouse.column < tree_area.width {
                    let max_scroll = self.visible_nodes.len().saturating_sub(1);
                    self.tree_scroll = (self.tree_scroll + 5).min(max_scroll);
                } else if self.show_logs {
                    self.log_scroll = self.log_scroll.saturating_sub(5);
                } else if let Some(ref active_id) = self.active_terminal_id
                    && let Some(proc) = self.processes.get(active_id)
                {
                    let x = mouse.column.saturating_sub(terminal_area.x);
                    let y = mouse.row.saturating_sub(terminal_area.y);
                    match proc.terminal.mouse_scroll(false, x, y) {
                        Ok(true) => {} // forwarded to PTY
                        Ok(false) => {
                            // No mouse protocol — fall back to scrollback
                            if let Err(e) = proc.terminal.scroll(5) {
                                debug!("Failed to scroll down: {e}");
                            }
                        }
                        Err(e) => debug!("Failed to send scroll to terminal: {e}"),
                    }
                }
            }
            MouseEventKind::Down(event::MouseButton::Right) => {
                self.close_context_menu();

                if mouse.column < tree_area.width {
                    // Right-click in tree area
                    let row = mouse.row.saturating_sub(tree_area.y) as usize + self.tree_scroll;
                    if row < self.visible_nodes.len() {
                        self.cursor = row;
                        self.update_active_terminal();
                        let node = self.visible_nodes[row].clone();
                        let (items, target) = match &node.kind {
                            NodeKind::Group {
                                expanded,
                                selected,
                                total,
                                ..
                            } => (
                                build_group_menu(*expanded, *selected, *total),
                                ContextMenuTarget::Group {
                                    id: node.id.clone(),
                                    expanded: *expanded,
                                    selected: *selected,
                                    total: *total,
                                },
                            ),
                            NodeKind::Command {
                                selected, status, ..
                            } => (
                                build_command_menu(*selected, status),
                                ContextMenuTarget::Command {
                                    id: node.id.clone(),
                                    selected: *selected,
                                    status: status.clone(),
                                },
                            ),
                        };
                        let screen = Rect::new(
                            0,
                            0,
                            tree_area.right() + terminal_area.width + 1,
                            tree_area.height + 1,
                        );
                        let area = compute_area(&items, mouse.column, mouse.row, screen);
                        self.context_menu = Some(ContextMenu {
                            target,
                            items,
                            cursor: 0,
                            area,
                        });
                    }
                } else if mouse.column > tree_area.width && self.active_terminal_id.is_some() {
                    // Right-click in terminal area
                    let proc_ref = self
                        .active_terminal_id
                        .as_ref()
                        .and_then(|id| self.processes.get(id));
                    let (has_scrollback, is_scrolled) = proc_ref.map_or((false, false), |proc| {
                        let parser = proc.terminal.parser().lock();
                        let screen = parser.screen();
                        (screen.scrollback_len() > 0, screen.scrollback() > 0)
                    });
                    let status = proc_ref.map(|p| &p.status);
                    let items = build_terminal_menu(has_scrollback, is_scrolled, status);
                    let screen = Rect::new(
                        0,
                        0,
                        tree_area.right() + terminal_area.width + 1,
                        tree_area.height + 1,
                    );
                    let area = compute_area(&items, mouse.column, mouse.row, screen);
                    self.context_menu = Some(ContextMenu {
                        target: ContextMenuTarget::Terminal,
                        items,
                        cursor: 0,
                        area,
                    });
                }
            }
            _ => {}
        }
    }
}
