//! Short-lived messages in the toolbar, for things the user would otherwise only find in the
//! log panel.

use std::time::{Duration, Instant};

use crate::selectors::watch::WatchReport;

/// How much a status message matters, which sets its colour and how long it shows
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusLevel {
    Info,
    Warn,
    Error,
}

impl StatusLevel {
    fn lifetime(self) -> Duration {
        match self {
            StatusLevel::Error => Duration::from_secs(8),
            StatusLevel::Info | StatusLevel::Warn => Duration::from_secs(4),
        }
    }
}

/// A message shown in place of the toolbar's shortcuts until it expires
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusMessage {
    pub text: String,
    pub level: StatusLevel,
    pub expires_at: Instant,
}

impl StatusMessage {
    /// A message shown from `now` for as long as its level asks.
    #[must_use]
    pub fn new(text: String, level: StatusLevel, now: Instant) -> Self {
        Self {
            text,
            level,
            expires_at: now + level.lifetime(),
        }
    }
}

/// One line on what the file watcher couldn't watch, or `None` if it watches everything.
#[must_use]
pub fn watch_problems(report: &WatchReport) -> Option<String> {
    let plural = |n: usize| if n == 1 { "" } else { "s" };
    let mut problems = Vec::new();
    if !report.missing.is_empty() {
        let n = report.missing.len();
        problems.push(format!("{n} missing path{}", plural(n)));
    }
    if !report.failed.is_empty() {
        let n = report.failed.len();
        problems.push(format!("{n} unreadable path{}", plural(n)));
    }
    if report.limit_reached {
        problems.push("out of file watches".to_string());
    }
    if problems.is_empty() {
        return None;
    }
    Some(format!(
        "Not watching every file: {} (see logs)",
        problems.join(", ")
    ))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn errors_show_longer() {
        let now = Instant::now();
        let info = StatusMessage::new("copied".into(), StatusLevel::Info, now);
        let error = StatusMessage::new("failed".into(), StatusLevel::Error, now);
        assert_eq!(info.expires_at, now + Duration::from_secs(4));
        assert_eq!(error.expires_at, now + Duration::from_secs(8));
    }

    #[test]
    fn watch_problems_summarises_report() {
        let mut report = WatchReport::default();
        assert_eq!(watch_problems(&report), None);

        report.missing = vec![PathBuf::from("/a"), PathBuf::from("/b")];
        report.failed = vec![(PathBuf::from("/c"), "permission denied".into())];
        report.limit_reached = true;
        assert_eq!(
            watch_problems(&report).as_deref(),
            Some(
                "Not watching every file: 2 missing paths, 1 unreadable path, \
                 out of file watches (see logs)"
            )
        );
    }
}
