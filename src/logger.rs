//! The global `log` implementation: an in-memory ring buffer (shown in the TUI's log panel) and
//! an optional log file.

use std::collections::VecDeque;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use log::{Level, LevelFilter, Log, Metadata, Record};
use parking_lot::Mutex;
use thiserror::Error;

const MAX_LOG_ENTRIES: usize = 1000;

#[derive(Debug, Clone)]
pub struct LogEntry {
    pub level: Level,
    pub target: String,
    pub message: String,
    pub timestamp: Instant,
}

/// Thread-safe ring buffer holding the newest log entries.
#[derive(Debug, Clone)]
pub struct LogBuffer {
    entries: Arc<Mutex<VecDeque<LogEntry>>>,
    start: Instant,
}

impl LogBuffer {
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: Arc::new(Mutex::new(VecDeque::with_capacity(MAX_LOG_ENTRIES))),
            start: Instant::now(),
        }
    }

    #[must_use]
    pub fn start(&self) -> Instant {
        self.start
    }

    pub fn push(&self, entry: LogEntry) {
        let mut entries = self.entries.lock();
        if entries.len() >= MAX_LOG_ENTRIES {
            entries.pop_front();
        }
        entries.push_back(entry);
    }

    /// Returns a snapshot of all entries.
    #[must_use]
    pub fn entries(&self) -> Vec<LogEntry> {
        self.entries.lock().iter().cloned().collect()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.lock().len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.lock().is_empty()
    }
}

impl Default for LogBuffer {
    fn default() -> Self {
        Self::new()
    }
}

type Notifier = Arc<dyn Fn() + Send + Sync>;
type NotifierSlot = Arc<Mutex<Option<Notifier>>>;

/// Settings for [`init`].
#[derive(Debug, Clone, Default)]
pub struct LoggerConfig {
    /// Most verbose level recorded. Defaults to `FNUG_LOG`, then info.
    pub level: Option<LevelFilter>,
    /// File to create (truncating it) and write every record to.
    pub file: Option<PathBuf>,
}

#[derive(Error, Debug)]
pub enum LoggerInitError {
    #[error("failed to create log file {}: {source}", path.display())]
    File {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("a logger is already installed")]
    AlreadyInitialized,
}

/// Handle to the logger installed by [`init`].
#[derive(Clone)]
pub struct LoggerHandle {
    buffer: LogBuffer,
    notifier: NotifierSlot,
}

impl LoggerHandle {
    /// The ring buffer every record goes to, including those logged before the TUI started.
    #[must_use]
    pub fn buffer(&self) -> LogBuffer {
        self.buffer.clone()
    }

    /// Call `notify` after each record, replacing any earlier notifier.
    pub fn set_notifier(&self, notify: Box<dyn Fn() + Send + Sync>) {
        *self.notifier.lock() = Some(Arc::from(notify));
    }
}

struct FnugLogger {
    buffer: LogBuffer,
    file: Option<Mutex<File>>,
    filter: LevelFilter,
    start: Instant,
    notifier: NotifierSlot,
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
        self.buffer.push(LogEntry {
            level: record.level(),
            target: record.target().to_string(),
            message: format!("{}", record.args()),
            timestamp: now,
        });

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

        // Cloned out so the lock isn't held while the notifier runs
        let notifier = self.notifier.lock().clone();
        if let Some(notify) = notifier {
            notify();
        }
    }

    fn flush(&self) {
        if let Some(ref file) = self.file {
            let _ = file.lock().flush();
        }
    }
}

fn level_from_env() -> Option<LevelFilter> {
    std::env::var("FNUG_LOG").ok()?.parse().ok()
}

/// Install the global logger.
///
/// # Errors
///
/// Returns `LoggerInitError::File` if the log file can't be created, and
/// `LoggerInitError::AlreadyInitialized` if a logger is already installed.
pub fn init(config: LoggerConfig) -> Result<LoggerHandle, LoggerInitError> {
    let filter = config
        .level
        .or_else(level_from_env)
        .unwrap_or(LevelFilter::Info);
    let file = config
        .file
        .map(|path| File::create(&path).map_err(|source| LoggerInitError::File { path, source }))
        .transpose()?;

    let handle = LoggerHandle {
        buffer: LogBuffer::new(),
        notifier: Arc::default(),
    };
    let logger = FnugLogger {
        buffer: handle.buffer(),
        file: file.map(Mutex::new),
        filter,
        start: Instant::now(),
        notifier: handle.notifier.clone(),
    };
    log::set_boxed_logger(Box::new(logger)).map_err(|_| LoggerInitError::AlreadyInitialized)?;
    log::set_max_level(filter);
    Ok(handle)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    fn logger(filter: LevelFilter) -> FnugLogger {
        FnugLogger {
            buffer: LogBuffer::new(),
            file: None,
            filter,
            start: Instant::now(),
            notifier: NotifierSlot::default(),
        }
    }

    fn log_at(logger: &FnugLogger, level: Level, message: &str) {
        logger.log(
            &Record::builder()
                .args(format_args!("{message}"))
                .level(level)
                .target("test_target")
                .build(),
        );
    }

    fn make_entry(level: Level, msg: &str) -> LogEntry {
        LogEntry {
            level,
            target: "test".to_string(),
            message: msg.to_string(),
            timestamp: Instant::now(),
        }
    }

    #[test]
    fn test_enabled_filters_by_level() {
        let logger = logger(LevelFilter::Warn);
        let enabled = |level| logger.enabled(&Metadata::builder().level(level).build());

        assert!(enabled(Level::Error));
        assert!(enabled(Level::Warn));
        assert!(!enabled(Level::Info));
        assert!(!enabled(Level::Debug));
    }

    #[test]
    fn test_log_writes_to_buffer() {
        let logger = logger(LevelFilter::Debug);
        log_at(&logger, Level::Info, "test message");

        let entries = logger.buffer.entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].message, "test message");
        assert_eq!(entries[0].target, "test_target");
        assert_eq!(entries[0].level, Level::Info);
    }

    #[test]
    fn test_log_respects_filter() {
        let logger = logger(LevelFilter::Warn);
        log_at(&logger, Level::Debug, "debug msg");
        log_at(&logger, Level::Warn, "warn msg");

        assert_eq!(logger.buffer.len(), 1);
        assert_eq!(logger.buffer.entries()[0].message, "warn msg");
    }

    #[test]
    fn test_log_writes_to_file() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.log");
        let logger = FnugLogger {
            file: Some(Mutex::new(File::create(&file_path).unwrap())),
            ..logger(LevelFilter::Debug)
        };

        log_at(&logger, Level::Info, "file log message");
        logger.flush();

        let content = std::fs::read_to_string(&file_path).unwrap();
        assert!(content.contains("file log message"));
        assert!(content.contains("INFO"));
        assert!(content.contains("test_target"));
    }

    #[test]
    fn notifier_runs_after_each_logged_record() {
        let logger = logger(LevelFilter::Info);
        let handle = LoggerHandle {
            buffer: logger.buffer.clone(),
            notifier: logger.notifier.clone(),
        };
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        handle.set_notifier(Box::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        }));

        log_at(&logger, Level::Info, "shown");
        log_at(&logger, Level::Debug, "filtered out");

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(handle.buffer().len(), 1);
    }

    #[test]
    fn test_push_and_retrieve() {
        let buf = LogBuffer::new();
        assert!(buf.is_empty());

        buf.push(make_entry(Level::Info, "hello"));
        buf.push(make_entry(Level::Warn, "world"));

        assert_eq!(buf.len(), 2);
        let entries = buf.entries();
        assert_eq!(entries[0].message, "hello");
        assert_eq!(entries[1].message, "world");
        assert_eq!(entries[0].level, Level::Info);
        assert_eq!(entries[1].level, Level::Warn);
    }

    #[test]
    fn test_ring_buffer_overflow() {
        let buf = LogBuffer::new();
        for i in 0..1500 {
            buf.push(make_entry(Level::Debug, &format!("msg-{i}")));
        }

        assert_eq!(buf.len(), MAX_LOG_ENTRIES);
        let entries = buf.entries();
        // Oldest 500 entries should have been dropped
        assert_eq!(entries[0].message, "msg-500");
        assert_eq!(entries[999].message, "msg-1499");
    }

    #[test]
    fn test_thread_safety() {
        let buf = LogBuffer::new();
        std::thread::scope(|s| {
            for t in 0..4 {
                let buf = &buf;
                s.spawn(move || {
                    for i in 0..100 {
                        buf.push(make_entry(Level::Info, &format!("t{t}-{i}")));
                    }
                });
            }
        });
        assert_eq!(buf.len(), 400);
    }
}
