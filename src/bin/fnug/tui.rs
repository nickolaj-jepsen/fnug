use std::fmt::Display;
use std::io;
use std::path::PathBuf;
use std::process::ExitCode;

use crossterm::event::{DisableMouseCapture, EnableMouseCapture, Event, EventStream};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use futures::StreamExt;
use log::{debug, error, info, warn};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use fnug::check::CheckResult;
use fnug::commands::group::CommandGroup;
use fnug::logger::LoggerHandle;
use fnug::selectors::watch::{WatchError, WatchReport, watch_commands};
use fnug::tui::app::{App, AppEvent};

/// Start a file watcher that forwards watch events to the app event channel.
/// The watcher setup (inotify registration) runs on a blocking thread to avoid
/// stalling TUI startup when watching many/large directories.
fn start_file_watcher(
    config: &CommandGroup,
    event_tx: tokio::sync::mpsc::Sender<AppEvent>,
) -> tokio::task::JoinHandle<()> {
    let all_commands: Vec<_> = config.all_commands().into_iter().cloned().collect();

    tokio::spawn(async move {
        // Setup watcher on a blocking thread (registering inotify watches for
        // large directory trees is slow and would block the TUI event loop).
        let result = tokio::task::spawn_blocking(move || watch_commands(all_commands)).await;

        let mut handle = match result {
            Ok(Ok(handle)) => {
                log_watch_report(&handle.report);
                handle
            }
            Ok(Err(WatchError::NoWatchableCommands)) => {
                debug!("File watcher not started: no command uses auto.watch");
                return;
            }
            Ok(Err(e)) => {
                if let WatchError::NothingWatched(report) = &e {
                    log_watch_report(report);
                }
                warn!("File watcher not started: {e}");
                return;
            }
            Err(e) => {
                warn!("File watcher task failed: {e}");
                return;
            }
        };

        // Forward watch events to app. The handle keeps watching while this scope lives.
        while let Some(matches) = handle.events.recv().await {
            if event_tx
                .send(AppEvent::WatcherTriggered(matches))
                .await
                .is_err()
            {
                break;
            }
        }
    })
}

fn log_watch_report(report: &WatchReport) {
    for path in &report.missing {
        warn!("Not watching {}: it does not exist", path.display());
    }
    for (path, error) in &report.failed {
        warn!("Could not watch {}: {error}", path.display());
    }
    if report.limit_reached {
        warn!(
            "Ran out of file watches, so some directories are not watched; \
             on Linux, raise fs.inotify.max_user_watches"
        );
    }
    if !report.roots.is_empty() {
        info!(
            "File watcher started: {} paths, {} directories",
            report.roots.len(),
            report.watched_dirs
        );
    }
}

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

pub async fn run(
    config: CommandGroup,
    cwd: PathBuf,
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

    // Log lines would corrupt the TUI; the log panel shows them instead
    logger.set_stderr(false);
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Create app
    let mut app = App::new(config.clone(), cwd, logger.buffer());
    if let Some(ref result) = check_result {
        let initial_area = ratatui::layout::Rect::new(0, 0, 80, 24);
        app.apply_check_result(result, initial_area);
    } else {
        app.apply_always_selection();
        app.spawn_git_selection();
    }

    let log_tx = app.event_tx.clone();
    logger.set_notifier(Box::new(move || {
        let _ = log_tx.try_send(AppEvent::LogUpdated);
    }));

    let file_watcher_handle = start_file_watcher(&config, app.event_tx.clone());

    // Main event loop
    let result = run_event_loop(&mut terminal, &mut app).await;
    file_watcher_handle.abort();
    if let Err(e) = &result {
        // Only to the log panel's buffer and the log file; stderr gets it once, below
        error!("Application error: {e}");
    }

    // Give the terminal back before waiting for commands to stop
    let restored = restore_terminal(&mut terminal);
    // So problems stopping the commands reach stderr
    logger.set_stderr(true);
    let running = app
        .processes
        .values()
        .filter(|p| p.terminal.is_running())
        .count();
    if running > 0 {
        eprintln!("Stopping {running} running command(s)…");
    }
    app.shutdown().await;
    restored?;

    if let Err(e) = result {
        eprintln!("Error: {e}");
        return Ok(ExitCode::FAILURE);
    }

    Ok(ExitCode::SUCCESS)
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> io::Result<()> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()
}

async fn run_event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::time::Duration;

    let mut event_stream = EventStream::new();
    let mut tree_area = ratatui::layout::Rect::default();
    let mut terminal_area = ratatui::layout::Rect::default();
    let mut needs_render = true;
    let mut pending_resize = false;

    // Frame rate limiter: ~60 FPS max
    let mut render_tick = tokio::time::interval(Duration::from_millis(16));
    render_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        if needs_render {
            app.clear_terminal_dirty();
            terminal.draw(|frame| {
                let (ta, term_a) = app.render(frame);
                tree_area = ta;
                terminal_area = term_a;
            })?;
            needs_render = false;

            // Resize PTYs after render so terminal_area reflects the new size
            if pending_resize {
                pending_resize = false;
                app.resize_terminals(terminal_area);
            }
        }

        if app.should_quit {
            break;
        }

        // Wait for events
        tokio::select! {
            // Periodic tick to check for dirty terminals
            _ = render_tick.tick() => {
                if app.any_terminal_dirty() {
                    needs_render = true;
                }
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
                        pending_resize = true;
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
                needs_render = true;
                if let Some(app_event) = maybe_app_event {
                    app.handle_app_event(app_event);
                }
            }
            // Defense-in-depth: handle Ctrl+C even if crossterm misses it
            _ = tokio::signal::ctrl_c() => {
                debug!("Received Ctrl+C signal");
                break;
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::worker_panic_message;

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
