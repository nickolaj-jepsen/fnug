use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use log::debug;
use ratatui::layout::Rect;

use super::app::{App, Focus};
use super::event::translate_key_event;
use super::tree_widget::NodeKind;

impl App {
    /// Handle keyboard input
    #[expect(
        clippy::too_many_lines,
        reason = "key handler covers all keyboard shortcuts in one match"
    )]
    pub fn handle_key(&mut self, key: KeyEvent, terminal_area: Rect) {
        self.last_terminal_area = terminal_area;
        // Keys act on the rows as they are now, with the cursor still on its node
        if self.tree_dirty {
            self.rebuild_visible_nodes();
        }

        if self.show_help {
            match key.code {
                KeyCode::Esc | KeyCode::Char('?' | 'q') => self.show_help = false,
                // Rendering clamps the scroll to what doesn't fit
                KeyCode::Char('j') | KeyCode::Down => self.help_scroll += 1,
                KeyCode::Char('k') | KeyCode::Up => {
                    self.help_scroll = self.help_scroll.saturating_sub(1);
                }
                _ => {}
            }
            return;
        }

        // Context menu navigation
        if self.context_menu.is_some() {
            match key.code {
                KeyCode::Up | KeyCode::Char('k') => {
                    if let Some(ref mut menu) = self.context_menu {
                        menu.cursor_up();
                    }
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    if let Some(ref mut menu) = self.context_menu {
                        menu.cursor_down();
                    }
                }
                KeyCode::Enter => self.execute_context_menu_action(terminal_area),
                _ => self.close_context_menu(),
            }
            return;
        }

        if key.code == KeyCode::Esc && self.fullscreen && self.focus == Focus::Tree {
            self.fullscreen = false;
            return;
        }

        // A focused terminal gets every key, Ctrl+C and Ctrl+R included, except the way out
        self.release_stale_focus();
        if self.focus == Focus::Terminal {
            let leave = is_leave_terminal_key(&key)
                || (key.code == KeyCode::Esc && !self.active_command_on_alternate_screen());
            if leave {
                self.focus = Focus::Tree;
            } else if let Some(proc) = self
                .active_terminal_id
                .as_ref()
                .and_then(|id| self.processes.get(id))
                && let Some(bytes) = translate_key_event(&key, proc.terminal.parser())
                && let Err(e) = proc.terminal.write(bytes)
            {
                debug!("Failed to write to terminal: {e}");
            }
            return;
        }

        // Global keys (only when terminal is not focused)
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.should_quit = true;
            return;
        }

        if key.code == KeyCode::Char('r') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.fullscreen = !self.fullscreen;
            return;
        }

        // Phase 1: Search editing mode — typing in search bar
        if self.search.is_editing() && matches!(self.focus, Focus::Tree) {
            match key.code {
                KeyCode::Enter => {
                    self.search.accept();
                    return;
                }
                KeyCode::Esc => {
                    self.search = super::app::SearchState::Inactive;
                    self.mark_tree_dirty();
                    return;
                }
                KeyCode::Backspace => {
                    self.search.pop_char();
                    self.mark_tree_dirty();
                    return;
                }
                KeyCode::Char(c) => {
                    self.search.push_char(c);
                    self.mark_tree_dirty();
                    return;
                }
                // Allow navigation keys to pass through
                KeyCode::Down | KeyCode::Up => {}
                _ => return,
            }
        }

        // Phase 2: Filter active but not editing — normal keys work, Esc clears filter
        if self.search.is_filtering() && matches!(self.focus, Focus::Tree) {
            match key.code {
                KeyCode::Char('/') => {
                    self.search.resume_editing();
                    return;
                }
                KeyCode::Esc => {
                    self.search = super::app::SearchState::Inactive;
                    self.mark_tree_dirty();
                    return;
                }
                // All other keys fall through to normal tree navigation
                _ => {}
            }
        }

        // Terminal scrolling (Shift+Arrow/Home/End)
        if key.modifiers.contains(KeyModifiers::SHIFT) {
            match key.code {
                KeyCode::Up => {
                    self.scroll_terminal(1);
                    return;
                }
                KeyCode::Down => {
                    self.scroll_terminal_down(1);
                    return;
                }
                KeyCode::Home => {
                    self.scroll_terminal_to_top();
                    return;
                }
                KeyCode::End => {
                    self.scroll_terminal_to_bottom();
                    return;
                }
                _ => {}
            }
        }

        // Tree navigation
        match key.code {
            KeyCode::Char('q') => {
                self.should_quit = true;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if self.cursor + 1 < self.visible_nodes.len() {
                    self.set_cursor_index(self.cursor + 1);
                    self.update_active_terminal();
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if self.cursor > 0 {
                    self.set_cursor_index(self.cursor - 1);
                    self.update_active_terminal();
                }
            }
            KeyCode::Char('h') | KeyCode::Left => {
                if let Some(node) = self.visible_nodes.get(self.cursor) {
                    match &node.kind {
                        NodeKind::Group { expanded: true, .. } => {
                            let id = node.id.clone();
                            self.set_expanded_by_user(&id, false);
                        }
                        NodeKind::Command { selected: true, .. } => {
                            self.selected.remove(&node.id);
                            self.mark_tree_dirty();
                        }
                        _ => {}
                    }
                }
            }
            KeyCode::Char('l') | KeyCode::Right => {
                if let Some(node) = self.visible_nodes.get(self.cursor) {
                    match &node.kind {
                        NodeKind::Group {
                            expanded: false, ..
                        } => {
                            let id = node.id.clone();
                            self.set_expanded_by_user(&id, true);
                        }
                        NodeKind::Command {
                            selected: false, ..
                        } => {
                            let id = node.id.clone();
                            self.select_by_hand(id);
                        }
                        _ => {}
                    }
                }
            }
            KeyCode::Char(' ') => {
                self.toggle_current_node();
            }
            KeyCode::Char('g') => self.git_select(),
            KeyCode::Enter => {
                self.run_selected(terminal_area);
            }
            KeyCode::Char('r') => {
                if let Some(id) = self.current_command_id() {
                    self.run_command(&id, terminal_area);
                } else if let Some(id) = self.current_group_id() {
                    self.run_group(&id, terminal_area);
                }
            }
            KeyCode::Char('s') => {
                if let Some(id) = self.current_command_id() {
                    self.stop_command(&id);
                }
            }
            KeyCode::Char('c') => {
                if let Some(id) = self.current_command_id() {
                    self.copy_command_output(&id);
                }
            }
            KeyCode::Char('x') => {
                if let Some(id) = self.current_command_id() {
                    self.clear_command(&id);
                }
            }
            KeyCode::F(5) => self.reload_config(),
            KeyCode::Char('w') => self.toggle_auto_run(),
            KeyCode::Char('/') => {
                self.search = super::app::SearchState::Editing(String::new());
            }
            KeyCode::Char('?') => {
                self.show_help = true;
                self.help_scroll = 0;
            }
            KeyCode::Char('E') => {
                self.expand_all();
            }
            KeyCode::Char('W') => {
                self.collapse_all();
            }
            KeyCode::Char('{') => {
                self.scroll_terminal_to_top();
            }
            KeyCode::Char('}') => {
                self.scroll_terminal_to_bottom();
            }
            KeyCode::Char('L') => {
                self.show_logs = !self.show_logs;
                self.log_scroll = 0;
            }
            KeyCode::Tab => self.focus_terminal(),
            _ => {}
        }
    }
}

/// Whether `key` is Ctrl+], which leaves a focused terminal. Terminals without the kitty
/// keyboard protocol send it as the byte 0x1d, which crossterm reads as Ctrl+5.
fn is_leave_terminal_key(key: &KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char(']' | '5'))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::layout::Rect;

    use crate::commands::command::Command;
    use crate::commands::group::CommandGroup;
    use crate::tui::app::App;
    use crate::tui::log_state::LogBuffer;

    fn test_app() -> App {
        let config = CommandGroup {
            id: "root".into(),
            name: "root".into(),
            commands: vec![Command {
                id: "a".into(),
                name: "a".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        App::new(config, PathBuf::new(), LogBuffer::new())
    }

    fn press(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
        app.handle_key(KeyEvent::new(code, modifiers), Rect::new(0, 0, 80, 24));
    }

    #[test]
    fn esc_in_fullscreen_exits_fullscreen_not_quit() {
        let mut app = test_app();
        app.fullscreen = true;

        press(&mut app, KeyCode::Esc, KeyModifiers::NONE);

        assert!(!app.fullscreen);
        assert!(!app.should_quit);
    }

    #[test]
    fn esc_in_tree_does_not_quit() {
        let mut app = test_app();

        press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(!app.should_quit);

        press(&mut app, KeyCode::Char('q'), KeyModifiers::NONE);
        assert!(app.should_quit);
    }

    #[test]
    fn x_clears_command() {
        let mut app = test_app();
        app.error_messages
            .insert("a".into(), "failed to start".into());

        press(&mut app, KeyCode::Char('j'), KeyModifiers::NONE);
        press(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);

        assert!(app.error_messages.is_empty());
    }

    #[test]
    fn ctrl_c_quits_from_tree() {
        let mut app = test_app();
        press(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert!(app.should_quit);
    }

    mod focus {
        use std::path::Path;
        use std::time::Duration;

        use crossterm::event::KeyCode;

        use crate::process::ExitInfo;
        use crate::pty::test_util::{pty_available, wait_until};
        use crate::tui::app::{App, AppEvent, Focus};
        use crate::tui::test_util::{AREA, press, press_ctrl, shell_app, type_text};

        /// A command `a` running `cmd` in `dir`, shown in the pane
        fn started(dir: &Path, cmd: &str) -> App {
            let mut app = shell_app(dir, &[("a", cmd)]);
            app.run_command("a", AREA);
            app
        }

        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn tab_focuses_line_mode_command() {
            if !pty_available() {
                return;
            }
            let dir = tempfile::tempdir().unwrap();
            let mut app = started(dir.path(), r#"read -r ans; echo "got=$ans" > answer"#);

            press(&mut app, KeyCode::Tab);
            assert_eq!(app.focus, Focus::Terminal);
            type_text(&mut app, "y");
            press(&mut app, KeyCode::Enter);

            let answer = dir.path().join("answer");
            assert!(wait_until(Duration::from_secs(5), || {
                std::fs::read_to_string(&answer).is_ok_and(|s| s == "got=y\n")
            }));
            app.shutdown().await;
        }

        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn ctrl_bracket_leaves_focus() {
            if !pty_available() {
                return;
            }
            let dir = tempfile::tempdir().unwrap();
            let mut app = started(dir.path(), "exec sleep 30");

            // With and without the kitty keyboard protocol
            for c in [']', '5'] {
                app.focus = Focus::Terminal;
                press_ctrl(&mut app, c);
                assert_eq!(app.focus, Focus::Tree, "Ctrl+{c}");
            }
            app.shutdown().await;
        }

        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn esc_forwarded_in_alt_screen() {
            if !pty_available() {
                return;
            }
            let dir = tempfile::tempdir().unwrap();
            let cmd = r"stty raw -echo; printf '\033[?1049h'; head -c 1 > key";
            let mut app = started(dir.path(), cmd);
            let term = std::sync::Arc::clone(&app.processes["a"].terminal);
            assert!(wait_until(Duration::from_secs(5), || {
                term.parser().lock().screen().alternate_screen()
            }));

            app.focus = Focus::Terminal;
            press(&mut app, KeyCode::Esc);

            assert_eq!(app.focus, Focus::Terminal);
            let key = dir.path().join("key");
            assert!(wait_until(Duration::from_secs(5), || {
                std::fs::read(&key).is_ok_and(|k| k == b"\x1b")
            }));
            app.shutdown().await;
        }

        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn esc_leaves_line_mode_command() {
            if !pty_available() {
                return;
            }
            let dir = tempfile::tempdir().unwrap();
            let mut app = started(dir.path(), "exec sleep 30");

            press(&mut app, KeyCode::Tab);
            press(&mut app, KeyCode::Esc);

            assert_eq!(app.focus, Focus::Tree);
            app.shutdown().await;
        }

        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn focus_returns_on_exit() {
            if !pty_available() {
                return;
            }
            let dir = tempfile::tempdir().unwrap();
            let mut app = started(dir.path(), "exec sleep 30");
            app.focus = Focus::Terminal;

            let exited = AppEvent::ProcessExited {
                id: "a".into(),
                generation: app.processes["a"].generation,
                exit: ExitInfo {
                    code: Some(0),
                    signal: None,
                    stop_requested: false,
                },
            };
            assert!(app.handle_app_event(exited));

            assert_eq!(app.focus, Focus::Tree);
            app.shutdown().await;
        }
    }

    /// Every key in `KEYMAP` does what it says, and unmodified keys not in it do nothing.
    mod keymap_parity {
        use std::path::Path;
        use std::sync::Arc;
        use std::time::Duration;

        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        use crate::commands::command::Command;
        use crate::pty::test_util::{pty_available, wait_until};
        use crate::tui::app::{App, CommandStatus, Focus};
        use crate::tui::keymap::{KEYMAP, KeyContext};
        use crate::tui::log_state::LogBuffer;
        use crate::tui::test_util::{AREA, cursor_node, group, type_text};

        /// The key events `name`, one key as `KEYMAP` writes it, stands for
        fn events(name: &str) -> Vec<KeyEvent> {
            let (modifiers, key) = if let Some(key) = name.strip_prefix("Ctrl+") {
                (KeyModifiers::CONTROL, key)
            } else if let Some(key) = name.strip_prefix("Shift+") {
                (KeyModifiers::SHIFT, key)
            } else {
                (KeyModifiers::NONE, name)
            };
            let codes = match key {
                "↑/↓" => vec![KeyCode::Up, KeyCode::Down],
                "Home" => vec![KeyCode::Home],
                "End" => vec![KeyCode::End],
                "↑" => vec![KeyCode::Up],
                "↓" => vec![KeyCode::Down],
                "←" => vec![KeyCode::Left],
                "→" => vec![KeyCode::Right],
                "Space" => vec![KeyCode::Char(' ')],
                "Enter" => vec![KeyCode::Enter],
                "Esc" => vec![KeyCode::Esc],
                "Tab" => vec![KeyCode::Tab],
                "F5" => vec![KeyCode::F(5)],
                _ => {
                    let mut chars = key.chars();
                    let c = chars.next().unwrap();
                    assert!(chars.next().is_none(), "unknown key {name:?}");
                    // Terminals send Ctrl+R as Ctrl and `r`
                    let c = if modifiers == KeyModifiers::CONTROL {
                        c.to_ascii_lowercase()
                    } else {
                        c
                    };
                    vec![KeyCode::Char(c)]
                }
            };
            codes
                .into_iter()
                .map(|code| KeyEvent::new(code, modifiers))
                .collect()
        }

        /// Everything a key could change that shows
        #[derive(Debug, PartialEq)]
        struct Seen {
            cursor: Option<String>,
            expanded: Vec<(String, bool)>,
            selected: Vec<String>,
            runs: Vec<(String, CommandStatus, Option<u64>)>,
            scroll: Option<usize>,
            search: String,
            focus: Focus,
            flags: [bool; 5],
            status: Option<String>,
            batch: Option<Vec<String>>,
        }

        fn seen(app: &mut App) -> Seen {
            app.rebuild_visible_nodes();
            let mut expanded: Vec<_> = app.expanded.clone().into_iter().collect();
            expanded.sort();
            let mut selected: Vec<_> = app.selected.iter().cloned().collect();
            selected.sort();
            let runs = app
                .config
                .all_commands()
                .iter()
                .map(|c| {
                    let generation = app.processes.get(&c.id).map(|p| p.generation);
                    (c.id.clone(), app.run_summary(&c.id).status, generation)
                })
                .collect();
            let scroll = app.active_terminal_id.as_ref().and_then(|id| {
                let proc = app.processes.get(id)?;
                Some(proc.terminal.parser().lock().screen().scrollback())
            });
            let mut batch: Option<Vec<String>> = app
                .batch_run_ids
                .as_ref()
                .map(|b| b.iter().cloned().collect());
            if let Some(batch) = &mut batch {
                batch.sort();
            }
            Seen {
                cursor: cursor_node(app).map(String::from),
                expanded,
                selected,
                runs,
                scroll,
                search: format!("{:?}", app.search),
                focus: app.focus,
                flags: [
                    app.fullscreen,
                    app.show_help,
                    app.show_logs,
                    app.should_quit,
                    app.auto_run_enabled,
                ],
                status: app.status.as_ref().map(|s| s.text.clone()),
                batch,
            }
        }

        /// ```text
        /// root
        /// ├─ grp         expanded
        /// │  ├─ out      running, its output scrolled up 5 lines, in the pane
        /// │  └─ waiter   waiting for out
        /// └─ closed      collapsed
        ///    └─ sel      selected
        /// ```
        fn fixture(dir: &Path) -> App {
            let command = |id: &str, cmd: &str, depends_on: &[&str]| Command {
                id: id.into(),
                name: id.into(),
                cmd: cmd.into(),
                cwd: dir.to_path_buf(),
                depends_on: depends_on.iter().map(|d| (*d).to_string()).collect(),
                ..Default::default()
            };
            let config = group(
                "root",
                vec![
                    group(
                        "grp",
                        vec![],
                        vec![
                            command("out", "seq 300; exec sleep 30", &[]),
                            command("waiter", "true", &["out"]),
                        ],
                    ),
                    group("closed", vec![], vec![command("sel", "true", &[])]),
                ],
                vec![],
            );
            let mut app = App::new(config, dir.to_path_buf(), LogBuffer::new());
            app.expanded.insert("closed".into(), false);
            app.select_by_hand("sel".into());
            app.run_command("waiter", AREA);
            app.active_terminal_id = Some("out".into());

            let out = Arc::clone(&app.processes["out"].terminal);
            let screen = |test: &dyn Fn(&vt100::Screen) -> bool| {
                wait_until(Duration::from_secs(5), || {
                    test(out.parser().lock().screen())
                })
            };
            assert!(
                screen(&|s| s.scrollback_len() >= 250),
                "out printed too little"
            );
            out.set_scroll(5).unwrap();
            assert!(screen(&|s| s.scrollback() == 5), "out didn't scroll");
            app
        }

        fn cursor_to(app: &mut App, id: &str) {
            app.rebuild_visible_nodes();
            let row = app.visible_nodes.iter().position(|n| n.id == id).unwrap();
            app.set_cursor_index(row);
            app.update_active_terminal();
        }

        /// Set `app` up where `name` of `context` has something to act on.
        fn place(app: &mut App, context: KeyContext, name: &str) {
            match context {
                KeyContext::Search => {
                    app.handle_key(KeyEvent::from(KeyCode::Char('/')), AREA);
                    type_text(app, "o");
                }
                KeyContext::Terminal => {
                    cursor_to(app, "out");
                    app.focus = Focus::Terminal;
                }
                KeyContext::Fullscreen => app.fullscreen = true,
                _ => {
                    let at = match name {
                        "k" | "↑" | "h" | "←" => "grp",
                        "l" | "→" => "closed",
                        "s" | "c" => "waiter",
                        "Space" | "r" | "x" | "Tab" | "Shift+↑/↓" | "{" | "}" | "Shift+Home"
                        | "Shift+End" => "out",
                        _ => "root",
                    };
                    cursor_to(app, at);
                }
            }
        }

        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn every_listed_key_acts() {
            if !pty_available() {
                return;
            }
            let dir = tempfile::tempdir().unwrap();
            for help in KEYMAP {
                for name in help.keys.split(" / ") {
                    for event in events(name) {
                        let mut app = fixture(dir.path());
                        place(&mut app, help.context, name);
                        let before = seen(&mut app);

                        app.handle_key(event, AREA);

                        // Scrolling reaches the output a moment later
                        let acted = wait_until(Duration::from_secs(2), || seen(&mut app) != before);
                        app.shutdown().await;
                        assert!(
                            acted,
                            "{event:?} ({name} in {:?}: {}) did nothing",
                            help.context, help.desc
                        );
                    }
                }
            }
        }

        /// Unmodified keys a terminal sends
        fn unmodified_keys() -> Vec<KeyEvent> {
            let named = [
                KeyCode::Up,
                KeyCode::Down,
                KeyCode::Left,
                KeyCode::Right,
                KeyCode::Home,
                KeyCode::End,
                KeyCode::PageUp,
                KeyCode::PageDown,
                KeyCode::Tab,
                KeyCode::BackTab,
                KeyCode::Backspace,
                KeyCode::Delete,
                KeyCode::Insert,
                KeyCode::Enter,
                KeyCode::Esc,
            ];
            let function = (1..=12).map(KeyCode::F);
            let chars = (' '..='~').map(KeyCode::Char);
            named
                .into_iter()
                .chain(function)
                .chain(chars)
                .map(KeyEvent::from)
                .collect()
        }

        /// Modified keys aren't tried, since most bindings ignore Shift.
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn unlisted_keys_do_nothing_in_the_tree() {
            if !pty_available() {
                return;
            }
            let tree = [
                KeyContext::Navigate,
                KeyContext::Run,
                KeyContext::Output,
                KeyContext::Other,
            ];
            let listed: Vec<KeyEvent> = KEYMAP
                .iter()
                .filter(|k| tree.contains(&k.context))
                .flat_map(|k| k.keys.split(" / ").flat_map(events))
                .collect();
            let dir = tempfile::tempdir().unwrap();
            let mut app = fixture(dir.path());
            app.rebuild_visible_nodes();
            let rows: Vec<String> = app.visible_nodes.iter().map(|n| n.id.clone()).collect();
            for row in rows {
                cursor_to(&mut app, &row);
                for event in unmodified_keys() {
                    if listed.contains(&event) {
                        continue;
                    }
                    let before = seen(&mut app);
                    app.handle_key(event, AREA);
                    assert_eq!(before, seen(&mut app), "{event:?} on {row} did something");
                }
            }
            app.shutdown().await;
        }
    }
}
