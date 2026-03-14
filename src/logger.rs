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
pub fn init(
    buffer: LogBuffer,
    log_file: Option<std::fs::File>,
    log_level: Option<log::LevelFilter>,
) {
    // Initialize the event slot
    EVENT_SLOT.get_or_init(|| Arc::new(Mutex::new(None)));

    let filter = log_level.unwrap_or_else(|| {
        std::env::var("FNUG_LOG")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(log::LevelFilter::Info)
    });

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

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;

    #[test]
    fn test_level_color_mapping() {
        assert_eq!(level_color(Level::Error), crate::theme::FAILURE);
        assert_eq!(level_color(Level::Warn), Color::Yellow);
        assert_eq!(level_color(Level::Info), Color::Blue);
        assert_eq!(level_color(Level::Debug), Color::DarkGray);
        assert_eq!(level_color(Level::Trace), Color::DarkGray);
    }

    #[test]
    fn test_enabled_filters_by_level() {
        let logger = FnugLogger {
            buffer: LogBuffer::new(),
            file: None,
            filter: log::LevelFilter::Warn,
            start: Instant::now(),
        };

        let error_meta = Metadata::builder()
            .level(Level::Error)
            .target("test")
            .build();
        let warn_meta = Metadata::builder()
            .level(Level::Warn)
            .target("test")
            .build();
        let info_meta = Metadata::builder()
            .level(Level::Info)
            .target("test")
            .build();
        let debug_meta = Metadata::builder()
            .level(Level::Debug)
            .target("test")
            .build();

        assert!(logger.enabled(&error_meta));
        assert!(logger.enabled(&warn_meta));
        assert!(!logger.enabled(&info_meta));
        assert!(!logger.enabled(&debug_meta));
    }

    #[test]
    fn test_log_writes_to_buffer() {
        let buffer = LogBuffer::new();
        let logger = FnugLogger {
            buffer: buffer.clone(),
            file: None,
            filter: log::LevelFilter::Debug,
            start: Instant::now(),
        };

        let record = Record::builder()
            .args(format_args!("test message"))
            .level(Level::Info)
            .target("test_target")
            .build();

        logger.log(&record);

        assert_eq!(buffer.len(), 1);
        let entries = buffer.entries();
        assert_eq!(entries[0].message, "test message");
        assert_eq!(entries[0].target, "test_target");
        assert_eq!(entries[0].level, Level::Info);
    }

    #[test]
    fn test_log_respects_filter() {
        let buffer = LogBuffer::new();
        let logger = FnugLogger {
            buffer: buffer.clone(),
            file: None,
            filter: log::LevelFilter::Warn,
            start: Instant::now(),
        };

        let debug_record = Record::builder()
            .args(format_args!("debug msg"))
            .level(Level::Debug)
            .target("test")
            .build();

        let warn_record = Record::builder()
            .args(format_args!("warn msg"))
            .level(Level::Warn)
            .target("test")
            .build();

        logger.log(&debug_record);
        logger.log(&warn_record);

        assert_eq!(buffer.len(), 1);
        assert_eq!(buffer.entries()[0].message, "warn msg");
    }

    #[test]
    fn test_log_writes_to_file() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.log");
        let file = std::fs::File::create(&file_path).unwrap();

        let logger = FnugLogger {
            buffer: LogBuffer::new(),
            file: Some(Mutex::new(file)),
            filter: log::LevelFilter::Debug,
            start: Instant::now(),
        };

        let record = Record::builder()
            .args(format_args!("file log message"))
            .level(Level::Info)
            .target("test_target")
            .build();

        logger.log(&record);
        logger.flush();

        let content = std::fs::read_to_string(&file_path).unwrap();
        assert!(content.contains("file log message"));
        assert!(content.contains("INFO"));
        assert!(content.contains("test_target"));
    }
}
