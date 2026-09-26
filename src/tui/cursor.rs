//! The tree cursor, which stays on the same node while the visible rows change around it.

use crate::commands::group::CommandGroup;

use super::app::App;

/// Ids of the groups enclosing node `id`, from the root down; `None` if `group` lacks it.
fn ancestor_ids(group: &CommandGroup, id: &str) -> Option<Vec<String>> {
    let holds = |g: &CommandGroup| {
        g.commands.iter().any(|c| c.id == id) || g.children.iter().any(|c| c.id == id)
    };
    if holds(group) {
        return Some(vec![group.id.clone()]);
    }
    group.children.iter().find_map(|child| {
        let mut ids = ancestor_ids(child, id)?;
        ids.insert(0, group.id.clone());
        Some(ids)
    })
}

impl App {
    /// Put the cursor on visible row `index`.
    pub(super) fn set_cursor_index(&mut self, index: usize) {
        self.cursor = index;
        self.cursor_id = self.visible_nodes.get(index).map(|n| n.id.clone());
    }

    /// Find the cursor's node in the rebuilt rows: the node itself, else its nearest visible
    /// group, else the same row, clamped. The pane follows if the cursor lands on another
    /// command.
    pub(super) fn resolve_cursor(&mut self) {
        if self.visible_nodes.is_empty() {
            // Nothing to point at, such as a search without matches: keep the node for later
            self.cursor = 0;
            return;
        }
        let position = |id: &str| self.visible_nodes.iter().position(|n| n.id == id);
        let found = self.cursor_id.as_deref().and_then(|id| {
            position(id).or_else(|| {
                ancestor_ids(&self.config, id)?
                    .iter()
                    .rev()
                    .find_map(|group| position(group))
            })
        });
        let index = found.unwrap_or(self.cursor.min(self.visible_nodes.len() - 1));
        let previous = self.cursor_id.take();
        self.set_cursor_index(index);
        if self.cursor_id != previous {
            self.update_active_terminal();
        }
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::KeyCode;

    use crate::selectors::watch::WatchMatch;
    use crate::tui::app::AppEvent;
    use crate::tui::test_util::{cursor_node, draw, press, two_groups, type_text};

    fn watcher_selects(ids: &[&str]) -> AppEvent {
        AppEvent::WatcherTriggered(
            ids.iter()
                .map(|id| WatchMatch {
                    id: (*id).to_string(),
                    files: vec![],
                })
                .collect(),
        )
    }

    fn move_down(app: &mut crate::tui::app::App, rows: usize) {
        for _ in 0..rows {
            press(app, KeyCode::Char('j'));
        }
    }

    #[test]
    fn search_shrink_keeps_cursor_on_match() {
        let mut app = two_groups();
        move_down(&mut app, 5);
        assert_eq!(cursor_node(&app), Some("b1"));

        type_text(&mut app, "/b1");
        press(&mut app, KeyCode::Enter);
        draw(&mut app, 80, 24);

        assert_eq!(cursor_node(&app), Some("b1"));
    }

    #[test]
    fn filter_clear_keeps_node() {
        let mut app = two_groups();
        type_text(&mut app, "/a2");
        press(&mut app, KeyCode::Enter);
        draw(&mut app, 80, 24);
        move_down(&mut app, 2);
        assert_eq!(cursor_node(&app), Some("a2"));

        press(&mut app, KeyCode::Esc);
        draw(&mut app, 80, 24);

        assert_eq!(cursor_node(&app), Some("a2"));
        assert_eq!(app.current_command_id().as_deref(), Some("a2"));
    }

    #[test]
    fn background_expand_keeps_cursor_node() {
        let mut app = two_groups();
        // Collapses alpha, keeps beta open
        app.handle_app_event(watcher_selects(&["b2"]));
        draw(&mut app, 80, 24);
        move_down(&mut app, 3);
        assert_eq!(cursor_node(&app), Some("b1"));

        // Expands alpha above the cursor
        app.handle_app_event(watcher_selects(&["a1"]));
        draw(&mut app, 80, 24);

        assert_eq!(cursor_node(&app), Some("b1"));
        assert_eq!(app.active_terminal_id.as_deref(), Some("b1"));
    }

    #[test]
    fn hidden_cursor_node_falls_back_to_its_group() {
        let mut app = two_groups();
        move_down(&mut app, 3);
        assert_eq!(cursor_node(&app), Some("a2"));

        press(&mut app, KeyCode::Char('W'));
        draw(&mut app, 80, 24);

        assert_eq!(cursor_node(&app), Some("alpha"));
    }
}
