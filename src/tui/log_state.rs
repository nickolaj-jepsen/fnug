use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Instant;

use log::Level;
use parking_lot::Mutex;

const MAX_LOG_ENTRIES: usize = 1000;

#[derive(Debug, Clone)]
pub struct LogEntry {
    pub level: Level,
    pub target: String,
    pub message: String,
    pub timestamp: Instant,
}

/// Thread-safe ring buffer for log entries.
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
