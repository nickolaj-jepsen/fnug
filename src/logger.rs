use std::io::Write;
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use log::{Level, Log, Metadata, Record};
use parking_lot::Mutex;
use tokio::sync::mpsc;

use crate::tui::app::AppEvent;
use crate::tui::log_state::{LogBuffer, LogEntry};

/// Slot for the app event sender, connected after `App` is created.
type EventSlot = Arc<Mutex<Option<mpsc::Sender<AppEvent>>>>;

static EVENT_SLOT: OnceLock<EventSlot> = OnceLock::new();

/// Connect the logger to the app event loop so it can trigger redraws.
pub fn connect_event_sender(tx: mpsc::Sender<AppEvent>) {
    if let Some(slot) = EVENT_SLOT.get() {
        *slot.lock() = Some(tx);
    }
}

struct FnugLogger {
    buffer: LogBuffer,
    file: Option<Mutex<std::fs::File>>,
    filter: log::LevelFilter,
    start: Instant,
}

impl Log for FnugLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= self.filter
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }

        let now = Instant::now();
        let entry = LogEntry {
            level: record.level(),
            target: record.target().to_string(),
            message: format!("{}", record.args()),
            timestamp: now,
        };

        self.buffer.push(entry);

        // Also write to file if configured
        if let Some(ref file) = self.file {
            let elapsed = now.duration_since(self.start).as_secs_f64();
            let _ = writeln!(
                file.lock(),
                "[{elapsed:.3}s] [{}] {} — {}",
                record.level(),
                record.target(),
                record.args()
            );
        }

        // Notify the app to redraw
        if let Some(slot) = EVENT_SLOT.get()
            && let Some(ref tx) = *slot.lock()
        {
            let _ = tx.try_send(AppEvent::LogUpdated);
        }
    }

    fn flush(&self) {
        if let Some(ref file) = self.file {
            let _ = file.lock().flush();
        }
    }
}

/// Initialize the global logger. Must be called once before any logging.
///
/// # Panics
///
/// Panics if called more than once.
pub fn init(buffer: LogBuffer, log_file: Option<std::fs::File>) {
    // Initialize the event slot
    EVENT_SLOT.get_or_init(|| Arc::new(Mutex::new(None)));

    let filter = std::env::var("RUST_LOG")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(log::LevelFilter::Info);

    let logger = FnugLogger {
        buffer,
        file: log_file.map(Mutex::new),
        filter,
        start: Instant::now(),
    };

    log::set_boxed_logger(Box::new(logger)).expect("logger already initialized");
    log::set_max_level(filter);
}

/// Map a log level to a ratatui color for display.
#[must_use]
pub fn level_color(level: Level) -> ratatui::style::Color {
    match level {
        Level::Error => crate::theme::FAILURE,
        Level::Warn => ratatui::style::Color::Yellow,
        Level::Info => ratatui::style::Color::Blue,
        Level::Debug | Level::Trace => ratatui::style::Color::DarkGray,
    }
}
