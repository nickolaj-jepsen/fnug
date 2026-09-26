use std::fmt::Display;
use std::io::{self, Write};
use std::process::ExitCode;

use crossterm::clipboard::CopyToClipboard;
use crossterm::event::{DisableMouseCapture, EnableMouseCapture, Event, EventStream};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use futures::StreamExt;
use log::{debug, error, warn};
use ratatui::Terminal;
use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::layout::Rect;

use fnug::check::CheckResult;
use fnug::logger::LoggerHandle;
use fnug::tui::app::{App, AppEvent, Outbound};
use fnug::{LoadOptions, LoadedConfig};
use tokio_util::sync::CancellationToken;

use crate::signals;

/// Log line for a panic on `thread`, or `None` for the main thread, which runs the UI.
///
/// A worker thread's panic (caught or not) must not restore the terminal under a TUI that keeps
/// running, so it is only logged.
fn worker_panic_message(thread: Option<&str>, panic: &impl Display) -> Option<String> {
    match thread {
        Some("main") => None,
        name => Some(format!("thread '{}' {panic}", name.unwrap_or("<unnamed>"))),
    }
}

/// Run the TUI on `loaded`, reloading it with `reload` when its files change.
pub async fn run(
    loaded: LoadedConfig,
    reload: LoadOptions,
    logger: LoggerHandle,
    check_result: Option<CheckResult>,
) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        if let Some(message) = worker_panic_message(std::thread::current().name(), panic_info) {
            error!("{message}");
        } else {
            let _ = disable_raw_mode();
            let _ = execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture);
            original_hook(panic_info);
        }
    }));

    // From here on SIGINT, SIGTERM and SIGHUP quit like `q`, so the terminal gets restored
    let signals = signals::install()?;
    // Log lines would corrupt the TUI; the log panel shows them instead
    logger.set_stderr(false);
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Create app
    let mut app = App::new(loaded.root, loaded.cwd, logger.buffer());
    if let Some(ref result) = check_result {
        apply_check_result(&mut terminal, &mut app, result)?;
    } else {
        app.apply_always_selection();
        app.spawn_git_selection();
    }

    let log_tx = app.event_tx.clone();
    logger.set_notifier(Box::new(move || {
        let _ = log_tx.try_send(AppEvent::LogUpdated);
    }));

    app.start_file_watcher();
    app.watch_config(reload, &loaded.sources);

    // Main event loop
    let result = run_event_loop(&mut terminal, &mut app, &signals.cancel).await;
    if let Err(e) = &result {
        // Only to the log panel's buffer and the log file; stderr gets it once, below
        error!("Application error: {e}");
    }

    // Give the terminal back before waiting for commands to stop. It may be hung up, which
    // fails every write, so nothing from here on may panic on one, as eprintln! does.
    let restored = restore_terminal(terminal);
    // So problems stopping the commands reach stderr
    logger.set_stderr(true);
    if let Err(e) = restored {
        warn!("Failed to restore the terminal: {e}");
    }
    let running = app
        .processes
        .values()
        .filter(|p| p.terminal.is_running())
        .count();
    if running > 0 {
        let _ = writeln!(io::stderr(), "Stopping {running} running command(s)…");
    }
    app.shutdown().await;

    if let Err(e) = &result {
        let _ = writeln!(io::stderr(), "Error: {e}");
    }
    let code = if result.is_ok() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    };
    Ok(signals.exit_code().unwrap_or(code))
}

/// Draw a first frame, then rerun the check's failures at the pane size it drew: output
/// already printed never reflows, so fixing the size later would be too late.
fn apply_check_result<B: Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
    result: &CheckResult,
) -> Result<(), B::Error> {
    let mut pane = Rect::default();
    terminal.draw(|frame| pane = app.render(frame).1)?;
    app.apply_check_result(result, pane);
    Ok(())
}

/// Leave raw mode, the alternate screen and mouse capture, and show the cursor. Tries every
/// step and returns the first failure.
fn restore_terminal(mut terminal: Terminal<CrosstermBackend<io::Stdout>>) -> io::Result<()> {
    let raw_mode = disable_raw_mode();
    let screen = execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    );
    let cursor = terminal.show_cursor();
    if cursor.is_err() {
        // Its Drop would retry and eprintln! the failure, which panics on a hung-up terminal
        std::mem::forget(terminal);
    }
    raw_mode.and(screen).and(cursor)
}

/// Write what the app queued for the terminal, such as an OSC 52 copy.
fn flush_outbox(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, app: &mut App) {
    for outbound in app.take_outbox() {
        match outbound {
            Outbound::Clipboard(text) => {
                let copy = CopyToClipboard::to_clipboard_from(text);
                if let Err(e) = execute!(terminal.backend_mut(), copy) {
                    warn!("Failed to copy with OSC 52: {e}");
                }
            }
        }
    }
}

/// Draw and handle events until the user quits or `cancel` is cancelled.
async fn run_event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    cancel: &CancellationToken,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::time::Duration;
    use tokio::time::Instant;

    /// Shortest time between two frames, which caps redraws at about 60 a second
    const FRAME: Duration = Duration::from_millis(16);

    let mut event_stream = EventStream::new();
    let mut tree_area = Rect::default();
    let mut terminal_area = Rect::default();
    let mut needs_render = true;
    let mut next_frame = Instant::now();
    let output = app.output_notify();

    // Advances running commands' elapsed time and expires status messages
    let mut clock = tokio::time::interval(Duration::from_secs(1));
    clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        needs_render |= app.any_terminal_dirty();
        if needs_render && Instant::now() >= next_frame {
            app.clear_terminal_dirty();
            terminal.draw(|frame| {
                let (ta, term_a) = app.render(frame);
                tree_area = ta;
                terminal_area = term_a;
            })?;
            needs_render = false;
            next_frame = Instant::now() + FRAME;
            // Whatever changed the pane: a window resize, fullscreen, or the divider
            app.sync_pty_size(terminal_area);
            flush_outbox(terminal, app);
        }

        if app.should_quit {
            break;
        }

        // Wait for events
        tokio::select! {
            () = tokio::time::sleep_until(next_frame), if needs_render => {}
            // Output for the terminal on screen; the loop's top checks which one got it
            () = output.notified() => {}
            _ = clock.tick(), if app.wants_tick() => {
                needs_render |= app.tick(std::time::Instant::now());
            }
            // Crossterm events
            maybe_event = event_stream.next() => {
                match maybe_event {
                    Some(Ok(Event::Key(key))) => {
                        needs_render = true;
                        app.handle_key(key, terminal_area);
                    }
                    Some(Ok(Event::Mouse(mouse))) => {
                        // Only re-render for move events if hover state changed
                        if matches!(mouse.kind, crossterm::event::MouseEventKind::Moved) {
                            let old_hover = app.mouse.hover_row;
                            let old_toolbar_hover = app.toolbar.hover;
                            let had_context_menu = app.context_menu.is_some();
                            app.handle_mouse(mouse, tree_area, terminal_area);
                            if app.mouse.hover_row != old_hover || app.toolbar.hover != old_toolbar_hover || had_context_menu {
                                needs_render = true;
                            }
                        } else {
                            needs_render = true;
                            app.handle_mouse(mouse, tree_area, terminal_area);
                        }
                    }
                    Some(Ok(Event::Resize(_w, _h))) => {
                        needs_render = true;
                    }
                    Some(Err(e)) => {
                        error!("Event error: {e}");
                        break;
                    }
                    None => break,
                    _ => {}
                }
            }
            // App events (process exit, watcher)
            maybe_app_event = app.event_rx.recv() => {
                if let Some(app_event) = maybe_app_event {
                    needs_render |= app.handle_app_event(app_event);
                }
            }
            // SIGINT, SIGTERM or SIGHUP; in raw mode Ctrl+C is a key, not SIGINT
            () = cancel.cancelled() => {
                debug!("Quitting on a signal");
                break;
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use fnug::check::CheckResult;
    use fnug::commands::command::Command;
    use fnug::commands::group::CommandGroup;
    use fnug::logger::LogBuffer;
    use fnug::pty::terminal::TerminalSize;
    use fnug::runner::{CommandReport, Failure, Outcome, RunReport};
    use fnug::tui::app::App;
    use portable_pty::{PtySize, native_pty_system};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;

    use super::{apply_check_result, worker_panic_message};

    /// `first` and `second`, both sleeping in `dir`, which a check reported as failed
    fn failed_pair(dir: &Path) -> (App, CheckResult) {
        let ids = ["first", "second"];
        let config = CommandGroup {
            id: "root".into(),
            name: "root".into(),
            commands: ids
                .iter()
                .map(|id| Command {
                    id: (*id).into(),
                    name: (*id).into(),
                    cmd: "exec sleep 30".into(),
                    cwd: dir.to_path_buf(),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        };
        let report = RunReport {
            commands: ids
                .iter()
                .map(|id| CommandReport {
                    id: (*id).into(),
                    name: (*id).into(),
                    outcome: Outcome::Failed(Failure::Exit(1)),
                    duration: None,
                    output: None,
                })
                .collect(),
            ..RunReport::default()
        };
        let app = App::new(config, dir.to_path_buf(), LogBuffer::new());
        (
            app,
            CheckResult {
                exit_code: 1,
                report,
            },
        )
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn handoff_uses_real_area() {
        if native_pty_system().openpty(PtySize::default()).is_err() {
            eprintln!("skipping: no PTY available");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let (mut app, result) = failed_pair(dir.path());
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();

        apply_check_result(&mut terminal, &mut app, &result).unwrap();

        let mut pane = Rect::default();
        terminal.draw(|frame| pane = app.render(frame).1).unwrap();
        for id in ["first", "second"] {
            let size = app.processes[id].terminal.size();
            assert_eq!(size, TerminalSize::new(pane.width, pane.height), "{id}");
        }
        assert_eq!(app.active_terminal_id.as_deref(), Some("first"));
        app.shutdown().await;
    }

    #[test]
    fn only_worker_panics_are_logged() {
        let panic = "panicked at src/x.rs:1:1:\nboom";
        assert_eq!(worker_panic_message(Some("main"), &panic), None);
        assert_eq!(
            worker_panic_message(Some("fnug-pty-parse"), &panic).as_deref(),
            Some("thread 'fnug-pty-parse' panicked at src/x.rs:1:1:\nboom")
        );
        assert_eq!(
            worker_panic_message(None, &panic).as_deref(),
            Some("thread '<unnamed>' panicked at src/x.rs:1:1:\nboom")
        );
    }
}
