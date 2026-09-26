//! Helpers for TUI tests that need no PTY.

use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;

use crate::commands::command::Command;
use crate::commands::group::CommandGroup;
use crate::selectors::{SelectedBy, SelectedCommand};

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

/// `root` holding a command per `(id, cmd)`, each running `cmd` in `dir`
pub(super) fn shell_group(dir: &Path, commands: &[(&str, &str)]) -> CommandGroup {
    let commands = commands
        .iter()
        .map(|(id, cmd)| Command {
            cmd: (*cmd).into(),
            cwd: dir.to_path_buf(),
            ..command(id)
        })
        .collect();
    group("root", vec![], commands)
}

/// An app for [`shell_group`]
pub(super) fn shell_app(dir: &Path, commands: &[(&str, &str)]) -> App {
    App::new(
        shell_group(dir, commands),
        dir.to_path_buf(),
        LogBuffer::new(),
    )
}

/// Git selection's verdict that changes to `files` select `id`
pub(super) fn git_selected(id: &str, files: &[&str]) -> SelectedCommand {
    SelectedCommand {
        id: id.into(),
        by: SelectedBy::Git,
        files: files.iter().map(PathBuf::from).collect(),
    }
}

pub(super) fn press(app: &mut App, code: KeyCode) {
    app.handle_key(KeyEvent::new(code, KeyModifiers::NONE), AREA);
}

pub(super) fn press_ctrl(app: &mut App, c: char) {
    app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL), AREA);
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
