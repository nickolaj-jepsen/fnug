//! What a run did: one [`CommandReport`] per planned command.

use std::path::PathBuf;
use std::time::Duration;

use super::output::CapturedOutput;

/// Why a command failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// Exited with this nonzero code.
    Exit(i32),
    /// Terminated by this signal.
    Signal(i32),
    /// Could not be started; the message says why.
    Spawn(String),
    /// Exited 0 but changed these tracked files.
    Modified(Vec<PathBuf>),
}

/// How a planned command ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Passed,
    Failed(Failure),
    /// Killed after running for this long.
    TimedOut(Duration),
    /// Not started because the command with id `cause` failed.
    Skipped {
        cause: String,
    },
    /// Killed because the run was cancelled or stopped by fail-fast.
    Cancelled,
    /// Not started because the run was cancelled or stopped by fail-fast.
    NotRun,
}

/// The result of one planned command.
#[derive(Debug, Clone)]
pub struct CommandReport {
    pub id: String,
    pub name: String,
    pub outcome: Outcome,
    /// How long it ran; `None` if it never started.
    pub duration: Option<Duration>,
    /// Its stdout and stderr, merged, when the run captured output.
    pub output: Option<CapturedOutput>,
}

/// The result of a run.
#[derive(Debug, Clone, Default)]
pub struct RunReport {
    /// One report per planned command, in plan order.
    pub commands: Vec<CommandReport>,
    pub duration: Duration,
    /// Whether the run was cancelled from outside, rather than stopped by fail-fast.
    pub cancelled: bool,
}

/// How many commands ended each way. The buckets add up to `total`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
    pub timed_out: usize,
    pub skipped: usize,
    pub cancelled: usize,
    pub not_run: usize,
}

impl RunReport {
    #[must_use]
    pub fn counts(&self) -> Counts {
        let mut counts = Counts {
            total: self.commands.len(),
            ..Counts::default()
        };
        for command in &self.commands {
            let bucket = match command.outcome {
                Outcome::Passed => &mut counts.passed,
                Outcome::Failed(_) => &mut counts.failed,
                Outcome::TimedOut(_) => &mut counts.timed_out,
                Outcome::Skipped { .. } => &mut counts.skipped,
                Outcome::Cancelled => &mut counts.cancelled,
                Outcome::NotRun => &mut counts.not_run,
            };
            *bucket += 1;
        }
        counts
    }

    /// Whether every planned command passed.
    #[must_use]
    pub fn success(&self) -> bool {
        !self.cancelled && self.commands.iter().all(|c| c.outcome == Outcome::Passed)
    }

    /// Ids of the commands that failed, timed out or were skipped, in plan order: what to run
    /// again after fixing the failures.
    #[must_use]
    pub fn rerun_ids(&self) -> Vec<String> {
        self.commands
            .iter()
            .filter(|c| {
                matches!(
                    c.outcome,
                    Outcome::Failed(_) | Outcome::TimedOut(_) | Outcome::Skipped { .. }
                )
            })
            .map(|c| c.id.clone())
            .collect()
    }

    #[must_use]
    pub fn get(&self, id: &str) -> Option<&CommandReport> {
        self.commands.iter().find(|c| c.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(outcomes: &[(&str, Outcome)]) -> RunReport {
        RunReport {
            commands: outcomes
                .iter()
                .map(|(id, outcome)| CommandReport {
                    id: (*id).to_string(),
                    name: (*id).to_string(),
                    outcome: outcome.clone(),
                    duration: None,
                    output: None,
                })
                .collect(),
            ..RunReport::default()
        }
    }

    #[test]
    fn counts_every_outcome_once() {
        let run = report(&[
            ("a", Outcome::Passed),
            ("b", Outcome::Failed(Failure::Exit(1))),
            ("c", Outcome::Skipped { cause: "b".into() }),
            ("d", Outcome::TimedOut(Duration::from_secs(1))),
            ("e", Outcome::Cancelled),
            ("f", Outcome::NotRun),
        ]);
        assert_eq!(
            run.counts(),
            Counts {
                total: 6,
                passed: 1,
                failed: 1,
                timed_out: 1,
                skipped: 1,
                cancelled: 1,
                not_run: 1,
            }
        );
        assert!(!run.success());
        assert_eq!(run.rerun_ids(), ["b", "c", "d"]);
    }

    #[test]
    fn empty_or_all_passed_is_success() {
        assert!(report(&[]).success());
        assert!(report(&[("a", Outcome::Passed)]).success());
        let cancelled = RunReport {
            cancelled: true,
            ..report(&[("a", Outcome::Passed)])
        };
        assert!(!cancelled.success());
    }
}
