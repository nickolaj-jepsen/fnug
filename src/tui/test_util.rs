//! Helpers for TUI tests that need no PTY.

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;

use crate::commands::command::Command;
use crate::commands::group::CommandGroup;

use super::app::App;
use super::log_state::LogBuffer;

pub(super) const AREA: Rect = Rect::new(0, 0, 80, 24);

pub(super) fn command(id: &str) -> Command {
    Command {
        id: id.into(),
        name: id.into(),
        ..Default::default()
    }
}

pub(super) fn group(id: &str, children: Vec<CommandGroup>, commands: Vec<Command>) -> CommandGroup {
    CommandGroup {
        id: id.into(),
        name: id.into(),
        children,
        commands,
        ..Default::default()
    }
}

/// `root` holding `alpha` (`a1`, `a2`) and `beta` (`b1`, `b2`)
pub(super) fn two_groups() -> App {
    let config = group(
        "root",
        vec![
            group("alpha", vec![], vec![command("a1"), command("a2")]),
            group("beta", vec![], vec![command("b1"), command("b2")]),
        ],
        vec![],
    );
    App::new(config, PathBuf::new(), LogBuffer::new())
}

pub(super) fn press(app: &mut App, code: KeyCode) {
    app.handle_key(KeyEvent::new(code, KeyModifiers::NONE), AREA);
}

pub(super) fn type_text(app: &mut App, text: &str) {
    for c in text.chars() {
        press(app, KeyCode::Char(c));
    }
}

/// Render `app` on a `width`×`height` screen. Returns the screen's rows and the
/// `(tree, terminal)` areas.
pub(super) fn draw(app: &mut App, width: u16, height: u16) -> (Vec<String>, (Rect, Rect)) {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    let mut areas = (Rect::default(), Rect::default());
    let frame = terminal.draw(|f| areas = app.render(f)).unwrap();
    let buffer = frame.buffer;
    let rows = (0..height)
        .map(|y| {
            (0..width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect();
    (rows, areas)
}

/// Id of the node under the cursor
pub(super) fn cursor_node(app: &App) -> Option<&str> {
    app.visible_nodes.get(app.cursor).map(|n| n.id.as_str())
}
