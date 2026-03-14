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

#[cfg(test)]
mod tests {
    use super::*;

    fn make_entry(level: Level, msg: &str) -> LogEntry {
        LogEntry {
            level,
            target: "test".to_string(),
            message: msg.to_string(),
            timestamp: Instant::now(),
        }
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
