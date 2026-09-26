//! How a command's latest run looks, decided in one place for the tree, the terminal pane and
//! batch completion.

use std::time::{Duration, Instant};

use crate::process::ExitInfo;
use crate::runner::NodeState;

use super::app::CommandStatus;

/// Where a command's latest run stands
#[derive(Debug, Clone, PartialEq)]
pub struct RunSummary {
    pub status: CommandStatus,
    pub started_at: Option<Instant>,
    /// `None` while the command runs, or if it never started
    pub finished_at: Option<Instant>,
    pub exit: Option<ExitInfo>,
    /// Dependencies a queued command still waits on
    pub waiting_on: Vec<String>,
}

impl Default for RunSummary {
    fn default() -> Self {
        Self {
            status: CommandStatus::Pending,
            started_at: None,
            finished_at: None,
            exit: None,
            waiting_on: Vec::new(),
        }
    }
}

/// A command's latest process, as [`RunSummary::derive`] sees it
pub(super) struct RunRecord<'a> {
    pub status: &'a CommandStatus,
    pub started_at: Instant,
    pub finished_at: Option<Instant>,
    pub exit: Option<&'a ExitInfo>,
    /// Started since the command was last queued, rather than left over from an earlier run
    pub current: bool,
}

impl RunSummary {
    /// Combine the scheduler's state for a command, its latest process and the error that kept
    /// it from running, if any.
    ///
    /// Being queued outranks everything else. A process left over from an earlier run is
    /// ignored once the command was cancelled or skipped before it started again, so an old
    /// pass never shows for a run that didn't happen.
    pub(super) fn derive(
        state: Option<&NodeState>,
        run: Option<RunRecord<'_>>,
        error: Option<&str>,
    ) -> Self {
        match state {
            Some(NodeState::Waiting(deps)) => {
                return Self {
                    status: CommandStatus::WaitingForDeps,
                    waiting_on: deps.clone(),
                    ..Self::default()
                };
            }
            Some(NodeState::Ready) => {
                return Self {
                    status: CommandStatus::WaitingForDeps,
                    ..Self::default()
                };
            }
            _ => {}
        }
        if let Some(message) = error {
            return Self {
                status: CommandStatus::Error(message.to_string()),
                ..Self::default()
            };
        }
        let Some(run) = run else {
            return Self::default();
        };
        let cancelled = matches!(state, Some(NodeState::NotRun | NodeState::Skipped { .. }));
        if cancelled && !run.current {
            return Self::default();
        }
        Self {
            status: run.status.clone(),
            started_at: Some(run.started_at),
            finished_at: run.finished_at,
            exit: run.exit.cloned(),
            waiting_on: Vec::new(),
        }
    }

    /// How long the run took, or has taken so far at `now` while it runs.
    #[must_use]
    pub fn elapsed(&self, now: Instant) -> Option<Duration> {
        let start = self.started_at?;
        let end = match self.status {
            CommandStatus::Running => now,
            _ => self.finished_at?,
        };
        Some(end.saturating_duration_since(start))
    }

    /// What to show after the status icon: a failure's exit code or signal, or `stopped`.
    #[must_use]
    pub fn detail(&self) -> Option<String> {
        match self.status {
            CommandStatus::Failure(code) => Some(
                self.exit
                    .as_ref()
                    .and_then(ExitInfo::signal_name)
                    .map_or_else(|| code.to_string(), str::to_string),
            ),
            CommandStatus::Stopped => Some("stopped".into()),
            _ => None,
        }
    }

    /// Whether the run failed, or couldn't start.
    #[must_use]
    pub fn is_failure(&self) -> bool {
        matches!(
            self.status,
            CommandStatus::Failure(_) | CommandStatus::Error(_)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(status: &CommandStatus, current: bool) -> RunRecord<'_> {
        RunRecord {
            status,
            started_at: Instant::now(),
            finished_at: Some(Instant::now()),
            exit: None,
            current,
        }
    }

    #[test]
    fn queued_outranks_error_and_old_run() {
        let waiting = NodeState::Waiting(vec!["build".into()]);
        let summary = RunSummary::derive(
            Some(&waiting),
            Some(record(&CommandStatus::Success, false)),
            Some("Dependency 'x' failed"),
        );
        assert_eq!(summary.status, CommandStatus::WaitingForDeps);
        assert_eq!(summary.waiting_on, ["build"]);

        let summary = RunSummary::derive(Some(&NodeState::Ready), None, None);
        assert_eq!(summary.status, CommandStatus::WaitingForDeps);
    }

    #[test]
    fn error_outranks_process() {
        let skipped = NodeState::Skipped {
            cause: "build".into(),
        };
        let summary = RunSummary::derive(
            Some(&skipped),
            Some(record(&CommandStatus::Success, false)),
            Some("Dependency 'build' failed"),
        );
        assert_eq!(
            summary.status,
            CommandStatus::Error("Dependency 'build' failed".into())
        );
    }

    #[test]
    fn cancelled_run_hides_old_result() {
        for state in [
            NodeState::NotRun,
            NodeState::Skipped {
                cause: "build".into(),
            },
        ] {
            let summary = RunSummary::derive(
                Some(&state),
                Some(record(&CommandStatus::Success, false)),
                None,
            );
            assert_eq!(summary, RunSummary::default(), "{state:?}");
        }
    }

    #[test]
    fn stopped_run_keeps_its_result() {
        let summary = RunSummary::derive(
            Some(&NodeState::NotRun),
            Some(record(&CommandStatus::Stopped, true)),
            None,
        );
        assert_eq!(summary.status, CommandStatus::Stopped);
        assert_eq!(summary.detail().as_deref(), Some("stopped"));
    }

    #[test]
    fn elapsed_grows_while_running() {
        let start = Instant::now();
        let summary = RunSummary {
            status: CommandStatus::Running,
            started_at: Some(start),
            ..RunSummary::default()
        };
        let now = start + Duration::from_secs(3);
        assert_eq!(summary.elapsed(now), Some(Duration::from_secs(3)));

        let finished = RunSummary {
            status: CommandStatus::Success,
            finished_at: Some(start + Duration::from_millis(250)),
            ..summary
        };
        assert_eq!(finished.elapsed(now), Some(Duration::from_millis(250)));
        assert_eq!(RunSummary::default().elapsed(now), None);
    }

    #[test]
    fn failure_detail_names_signal_or_code() {
        let failed = |code: u32, signal: Option<i32>| RunSummary {
            status: CommandStatus::Failure(code),
            exit: Some(ExitInfo {
                code: signal.is_none().then_some(i32::try_from(code).unwrap()),
                signal,
                stop_requested: false,
            }),
            ..RunSummary::default()
        };
        assert_eq!(failed(101, None).detail().as_deref(), Some("101"));
        assert_eq!(
            failed(139, Some(libc::SIGSEGV)).detail().as_deref(),
            Some("SIGSEGV")
        );
        assert_eq!(RunSummary::default().detail(), None);
    }
}
