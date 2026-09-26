//! The tools' results.

use serde::Serialize;

use crate::runner::{self, CommandReport, Failure, Outcome, RunReport};

#[derive(Serialize)]
pub(super) struct LintInfo {
    pub id: String,
    pub name: String,
    pub cmd: String,
    pub cwd: String,
    pub auto_rules: AutoRules,
    pub depends_on: Vec<String>,
    pub group: String,
    pub selected: bool,
}

#[derive(Serialize)]
pub(super) struct AutoRules {
    pub git: Option<bool>,
    pub watch: Option<bool>,
    pub always: Option<bool>,
    pub check: Option<bool>,
}

#[derive(Serialize)]
pub(super) struct RunResult {
    total: usize,
    passed: usize,
    failed: usize,
    timed_out: usize,
    skipped: usize,
    cancelled: usize,
    not_run: usize,
    duration_ms: u128,
    commands: Vec<CommandRunResult>,
}

#[derive(Serialize)]
struct CommandRunResult {
    name: String,
    id: String,
    /// `passed`, `failed`, `timeout`, `skipped`, `cancelled` or `not_run`.
    status: &'static str,
    exit_code: Option<i32>,
    duration_ms: u128,
    /// stdout and stderr, merged in the order they were written.
    output: String,
}

impl From<&RunReport> for RunResult {
    fn from(report: &RunReport) -> Self {
        let counts = report.counts();
        Self {
            total: counts.total,
            passed: counts.passed,
            failed: counts.failed,
            timed_out: counts.timed_out,
            skipped: counts.skipped,
            cancelled: counts.cancelled,
            not_run: counts.not_run,
            duration_ms: report.duration.as_millis(),
            commands: report.commands.iter().map(CommandRunResult::from).collect(),
        }
    }
}

impl From<&CommandReport> for CommandRunResult {
    fn from(report: &CommandReport) -> Self {
        let captured = report
            .output
            .as_ref()
            .map(runner::CapturedOutput::text)
            .unwrap_or_default();
        let (status, exit_code, output) = match &report.outcome {
            Outcome::Passed => ("passed", Some(0), captured),
            Outcome::Failed(Failure::Exit(code)) => ("failed", Some(*code), captured),
            Outcome::Failed(Failure::Spawn(message)) => ("failed", None, message.clone()),
            Outcome::Failed(_) => ("failed", None, captured),
            Outcome::TimedOut(_) => ("timeout", None, captured),
            Outcome::Skipped { cause } => (
                "skipped",
                None,
                format!("Skipped: dependency '{cause}' failed"),
            ),
            Outcome::Cancelled => ("cancelled", None, captured),
            Outcome::NotRun => ("not_run", None, captured),
        };
        Self {
            name: report.name.clone(),
            id: report.id.clone(),
            status,
            exit_code,
            duration_ms: report.duration.unwrap_or_default().as_millis(),
            output,
        }
    }
}
