use std::io;
use std::io::IsTerminal;
use std::process::ExitCode;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use crossterm::event::{DisableMouseCapture, EnableMouseCapture, Event, EventStream};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use futures::StreamExt;
use log::{debug, error, info, warn};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use fnug::load_config;
use fnug::selectors::watch::watch_commands;
use fnug::tui::app::{App, AppEvent};
use fnug::tui::log_state::LogBuffer;

#[derive(Parser, Debug)]
#[command(name = "fnug", about = "TUI command runner based on git changes")]
struct Cli {
    /// Path to config file (auto-detected if not specified)
    #[arg(short, long)]
    config: Option<String>,

    /// Log file path (enables file logging in addition to TUI log panel)
    #[arg(long)]
    log_file: Option<String>,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Run selected commands headlessly (useful for pre-commit hooks)
    Check {
        /// Stop on first failure
        #[arg(long)]
        fail_fast: bool,

        /// Never prompt to open the TUI on failure
        #[arg(long)]
        no_tui: bool,

        /// Suppress stdout/stderr for commands that pass
        #[arg(long)]
        mute_success: bool,
    },
    /// Install a git pre-commit hook that runs `fnug check`
    InitHooks {
        /// Overwrite existing hook
        #[arg(long)]
        force: bool,
    },
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("Error: {e}");
            ExitCode::FAILURE
        }
    }
}

#[tokio::main]
#[allow(clippy::too_many_lines)]
async fn run() -> Result<ExitCode, Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    // Load config
    let (config, cwd, config_path) = load_config(cli.config.as_deref())?;

    // Handle subcommands that don't need TUI setup
    let check_result = match cli.command {
        Some(Commands::Check {
            fail_fast,
            no_tui,
            mute_success,
        }) => {
            let result = fnug::check::run(&config, &cwd, fail_fast, mute_success)?;
            if result.exit_code == 0 {
                return Ok(ExitCode::SUCCESS);
            }
            // On failure in an interactive terminal, offer to open the TUI
            if !no_tui && std::io::stdin().is_terminal() && std::io::stderr().is_terminal() {
                eprint!("Open TUI to investigate? [y/N] ");
                let _ = std::io::Write::flush(&mut std::io::stderr());
                let mut answer = String::new();
                std::io::stdin().read_line(&mut answer)?;
                if answer.trim().eq_ignore_ascii_case("y") {
                    Some(result)
                } else {
                    return Ok(ExitCode::FAILURE);
                }
            } else {
                return Ok(ExitCode::FAILURE);
            }
        }
        Some(Commands::InitHooks { force }) => {
            fnug::init_hooks::run(&cwd, force)?;
            return Ok(ExitCode::SUCCESS);
        }
        None => None,
    };

    // Initialize the log buffer and custom logger
    let log_buffer = LogBuffer::new();
    let log_file = cli
        .log_file
        .as_ref()
        .map(std::fs::File::create)
        .transpose()?;
    fnug::logger::init(log_buffer.clone(), log_file);

    // Install panic hook that restores the terminal before printing the panic
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture);
        original_hook(panic_info);
    }));

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Create app
    let mut app = App::new(config.clone(), cwd, config_path.clone(), log_buffer);
    if let Some(ref result) = check_result {
        // Carry over check state: select failed commands and auto-start them
        let initial_area = ratatui::layout::Rect::new(0, 0, 80, 24);
        app.apply_check_result(result, initial_area);
    } else {
        app.run_git_selection();
    }

    // Connect the logger to the app's event channel for redraw notifications
    fnug::logger::connect_event_sender(app.event_tx.clone());

    // Start config file watcher for hot-reload
    let config_watcher_handle = {
        use notify::{EventKind, RecursiveMode, Watcher};
        use std::time::Instant;

        let event_tx = app.event_tx.clone();
        let watched_path = config_path;
        let last_reload = Arc::new(parking_lot::Mutex::new(Instant::now()));

        match notify::recommended_watcher(move |res: Result<notify::Event, notify::Error>| {
            if let Ok(event) = res
                && matches!(event.kind, EventKind::Modify(_) | EventKind::Create(_))
            {
                // Manual debounce: skip events within 1s of last reload
                let mut last = last_reload.lock();
                if last.elapsed().as_secs() >= 1 {
                    *last = Instant::now();
                    let _ = event_tx.blocking_send(AppEvent::ConfigChanged);
                }
            }
        }) {
            Ok(mut watcher) => {
                if let Err(e) = watcher.watch(&watched_path, RecursiveMode::NonRecursive) {
                    warn!("Config file watcher not started: {e}");
                    None
                } else {
                    info!("Config file watcher started for {}", watched_path.display());
                    // Keep watcher alive by boxing it
                    Some(Box::new(watcher))
                }
            }
            Err(e) => {
                warn!("Config file watcher not started: {e}");
                None
            }
        }
    };

    // Start file watcher
    let all_commands: Vec<_> = config.all_commands().into_iter().cloned().collect();
    let event_tx = app.event_tx.clone();
    let watcher_handle = match watch_commands(all_commands) {
        Ok((mut watcher_rx, _watcher)) => {
            info!("File watcher started");
            Some(tokio::spawn(async move {
                while let Some(commands) = watcher_rx.recv().await {
                    if event_tx
                        .send(AppEvent::WatcherTriggered(commands))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            }))
        }
        Err(e) => {
            warn!("File watcher not started: {e}");
            None
        }
    };

    // Main event loop
    let result = run_app(&mut terminal, &mut app).await;

    // Shutdown: kill processes, abort tasks
    app.shutdown();
    drop(config_watcher_handle);
    if let Some(handle) = watcher_handle {
        handle.abort();
    }

    // Cleanup terminal
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    if let Err(e) = result {
        error!("Application error: {e}");
        eprintln!("Error: {e}");
    }

    Ok(ExitCode::SUCCESS)
}

async fn run_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::time::Duration;

    let mut event_stream = EventStream::new();
    let mut tree_area = ratatui::layout::Rect::default();
    let mut terminal_area = ratatui::layout::Rect::default();
    let mut needs_render = true;

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
                        app.resize_terminals(terminal_area);
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
