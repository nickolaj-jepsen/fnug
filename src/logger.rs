//! The global `log` implementation: an in-memory ring buffer (shown in the TUI's log panel), an
//! optional log file, and stderr until the TUI takes over the terminal. Nothing is written to
//! stdout, which `fnug mcp` uses for the protocol.

use std::collections::VecDeque;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
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
    /// Most verbose level recorded. Defaults to `FNUG_LOG`, then info for the buffer and file
    /// and warn for stderr.
    pub level: Option<LevelFilter>,
    /// File to create (truncating it) and write every record to.
    pub file: Option<PathBuf>,
    /// Whether records also go to stderr until [`LoggerHandle::set_stderr`] turns it off.
    pub stderr: bool,
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
    stderr: Arc<AtomicBool>,
}

impl LoggerHandle {
    /// The ring buffer every record goes to, including those logged before the TUI started.
    #[must_use]
    pub fn buffer(&self) -> LogBuffer {
        self.buffer.clone()
    }

    /// Call `notify` after each record that reaches the buffer, replacing any earlier notifier.
    pub fn set_notifier(&self, notify: Box<dyn Fn() + Send + Sync>) {
        *self.notifier.lock() = Some(Arc::from(notify));
    }

    /// Start or stop writing records to stderr. Turn it off before a TUI takes over the terminal.
    pub fn set_stderr(&self, on: bool) {
        self.stderr.store(on, Ordering::Relaxed);
    }
}

struct FnugLogger {
    buffer: LogBuffer,
    file: Option<Mutex<File>>,
    filter: LevelFilter,
    start: Instant,
    notifier: NotifierSlot,
    stderr: Mutex<Box<dyn Write + Send>>,
    stderr_on: Arc<AtomicBool>,
    stderr_filter: LevelFilter,
}

impl FnugLogger {
    fn to_stderr(&self, level: Level) -> bool {
        level <= self.stderr_filter && self.stderr_on.load(Ordering::Relaxed)
    }

    fn write_stderr(&self, record: &Record) {
        let mut stderr = self.stderr.lock();
        let _ = match record.level() {
            Level::Error => writeln!(stderr, "error: {}", record.args()),
            Level::Warn => writeln!(stderr, "warning: {}", record.args()),
            level => writeln!(stderr, "[{level} {}] {}", record.target(), record.args()),
        };
    }
}

impl Log for FnugLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= self.filter || self.to_stderr(metadata.level())
    }

    fn log(&self, record: &Record) {
        if self.to_stderr(record.level()) {
            self.write_stderr(record);
        }
        if record.level() > self.filter {
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
        let _ = self.stderr.lock().flush();
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
    let level = config.level.or_else(level_from_env);
    let filter = level.unwrap_or(LevelFilter::Info);
    let stderr_filter = level.unwrap_or(LevelFilter::Warn);
    let file = config
        .file
        .map(|path| File::create(&path).map_err(|source| LoggerInitError::File { path, source }))
        .transpose()?;

    let handle = LoggerHandle {
        buffer: LogBuffer::new(),
        notifier: Arc::default(),
        stderr: Arc::new(AtomicBool::new(config.stderr)),
    };
    let logger = FnugLogger {
        buffer: handle.buffer(),
        file: file.map(Mutex::new),
        filter,
        start: Instant::now(),
        notifier: handle.notifier.clone(),
        stderr: Mutex::new(Box::new(std::io::stderr())),
        stderr_on: handle.stderr.clone(),
        stderr_filter,
    };
    log::set_boxed_logger(Box::new(logger)).map_err(|_| LoggerInitError::AlreadyInitialized)?;
    log::set_max_level(filter.max(stderr_filter));
    Ok(handle)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[derive(Clone, Default)]
    struct Captured(Arc<Mutex<Vec<u8>>>);

    impl Write for Captured {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Captured {
        fn text(&self) -> String {
            String::from_utf8(self.0.lock().clone()).unwrap()
        }
    }

    fn logger(filter: LevelFilter) -> FnugLogger {
        FnugLogger {
            buffer: LogBuffer::new(),
            file: None,
            filter,
            start: Instant::now(),
            notifier: NotifierSlot::default(),
            stderr: Mutex::new(Box::new(std::io::sink())),
            stderr_on: Arc::new(AtomicBool::new(false)),
            stderr_filter: LevelFilter::Off,
        }
    }

    fn logger_with_stderr(
        filter: LevelFilter,
        stderr_filter: LevelFilter,
    ) -> (FnugLogger, Captured) {
        let captured = Captured::default();
        let logger = FnugLogger {
            stderr: Mutex::new(Box::new(captured.clone())),
            stderr_on: Arc::new(AtomicBool::new(true)),
            stderr_filter,
            ..logger(filter)
        };
        (logger, captured)
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
    fn stderr_uses_its_own_threshold_and_format() {
        let (logger, stderr) = logger_with_stderr(LevelFilter::Info, LevelFilter::Warn);
        log_at(&logger, Level::Error, "broken");
        log_at(&logger, Level::Warn, "careful");
        log_at(&logger, Level::Info, "fyi");

        assert_eq!(stderr.text(), "error: broken\nwarning: careful\n");
        assert_eq!(logger.buffer.len(), 3);

        let (logger, stderr) = logger_with_stderr(LevelFilter::Info, LevelFilter::Debug);
        log_at(&logger, Level::Debug, "detail");
        assert_eq!(stderr.text(), "[DEBUG test_target] detail\n");
        assert!(
            logger.buffer.is_empty(),
            "debug is below the buffer's level"
        );
    }

    #[test]
    fn stderr_off_still_fills_the_buffer() {
        let (logger, stderr) = logger_with_stderr(LevelFilter::Info, LevelFilter::Warn);
        let handle = LoggerHandle {
            buffer: logger.buffer.clone(),
            notifier: logger.notifier.clone(),
            stderr: logger.stderr_on.clone(),
        };
        handle.set_stderr(false);
        log_at(&logger, Level::Warn, "hidden from stderr");

        assert_eq!(stderr.text(), "");
        assert_eq!(handle.buffer().entries()[0].message, "hidden from stderr");
    }

    #[test]
    fn notifier_runs_after_each_logged_record() {
        let logger = logger(LevelFilter::Info);
        let handle = LoggerHandle {
            buffer: logger.buffer.clone(),
            notifier: logger.notifier.clone(),
            stderr: logger.stderr_on.clone(),
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
