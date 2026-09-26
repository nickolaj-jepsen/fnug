//! The tools' results.
//!
//! A run's result is a compact JSON summary that lists failures first, followed by a text block
//! with the output of each command that failed or timed out, and with `verbose` also of those
//! that passed or were cancelled.

use std::fmt::Write as _;

use rmcp::model::{CallToolResult, Content};
use serde::Serialize;

use super::text::{self, Capped, Gap};
use crate::process::ExitInfo;
use crate::runner::{
    CapturedOutput, CommandReport, Failure, Outcome, Plan, PlannedCommand, RunReport,
};

/// Output a result keeps of one command: a fifth from its start, the rest from its end.
const OUTPUT_PER_COMMAND: usize = 20 * 1024;
/// Output a result keeps of all its commands together.
const OUTPUT_TOTAL: usize = 60 * 1024;

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
struct Summary {
    ok: bool,
    message: String,
    total: usize,
    passed: usize,
    failed: usize,
    timed_out: usize,
    skipped: usize,
    cancelled: usize,
    not_run: usize,
    duration_ms: u128,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<String>,
    commands: Vec<CommandSummary>,
}

#[derive(Serialize)]
struct CommandSummary {
    id: String,
    name: String,
    group: String,
    cmd: String,
    /// `passed`, `failed`, `timeout`, `skipped`, `cancelled` or `not_run`.
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    signal: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    duration_ms: Option<u128>,
    /// Why it failed to start, timed out or didn't run.
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
    /// Bytes of output the command wrote.
    #[serde(skip_serializing_if = "Option::is_none")]
    output_bytes: Option<u64>,
    /// Bytes of its output left out of its text block.
    #[serde(skip_serializing_if = "is_zero")]
    truncated_bytes: u64,
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde passes a reference
fn is_zero(n: &u64) -> bool {
    *n == 0
}

/// The result of a run: the summary, then a text block per command whose output is shown.
///
/// # Errors
///
/// Returns an error if the summary can't be serialized.
pub(super) fn run_result(
    plan: &Plan,
    report: &RunReport,
    verbose: bool,
) -> Result<CallToolResult, serde_json::Error> {
    // Reports are in plan order
    let entries: Vec<(&PlannedCommand, &CommandReport)> =
        plan.commands.iter().zip(&report.commands).collect();
    let mut order: Vec<usize> = (0..entries.len()).collect();
    order.sort_by_key(|&i| rank(&entries[i].1.outcome));

    // Failures get the output budget first
    let failures = shown_outputs(&entries, &order, OUTPUT_TOTAL, is_failure);
    let left = OUTPUT_TOTAL.saturating_sub(failures.iter().map(|(_, c)| c.text.len()).sum());
    let extras = if verbose {
        shown_outputs(&entries, &order, left, |outcome| {
            matches!(outcome, Outcome::Passed | Outcome::Cancelled)
        })
    } else {
        Vec::new()
    };
    let mut shown: Vec<(usize, Capped)> = failures.into_iter().chain(extras).collect();
    shown.sort_by_key(|(i, _)| (rank(&entries[*i].1.outcome), *i));

    let counts = report.counts();
    let summary = Summary {
        ok: report.success(),
        message: message(report),
        total: counts.total,
        passed: counts.passed,
        failed: counts.failed,
        timed_out: counts.timed_out,
        skipped: counts.skipped,
        cancelled: counts.cancelled,
        not_run: counts.not_run,
        duration_ms: report.duration.as_millis(),
        warnings: plan.warnings.clone(),
        commands: order
            .iter()
            .map(|&i| {
                let truncated = shown
                    .iter()
                    .find(|(j, _)| *j == i)
                    .map_or(0, |(_, capped)| capped.omitted);
                command_summary(entries[i].0, entries[i].1, report.cancelled, truncated)
            })
            .collect(),
    };

    let mut content = vec![Content::text(serde_json::to_string(&summary)?)];
    content.extend(
        shown
            .iter()
            .map(|(i, capped)| Content::text(block(entries[*i].1, capped))),
    );
    Ok(CallToolResult::success(content))
}

/// Where a command goes in the result: failures first, passes last.
fn rank(outcome: &Outcome) -> u8 {
    match outcome {
        Outcome::Failed(_) | Outcome::TimedOut(_) => 0,
        Outcome::Cancelled => 1,
        Outcome::Skipped { .. } => 2,
        Outcome::NotRun => 3,
        Outcome::Passed => 4,
    }
}

fn is_failure(outcome: &Outcome) -> bool {
    matches!(outcome, Outcome::Failed(_) | Outcome::TimedOut(_))
}

/// The cleaned output of each command, in `order`, whose outcome `show` picks, capped to share
/// `total` bytes.
fn shown_outputs(
    entries: &[(&PlannedCommand, &CommandReport)],
    order: &[usize],
    total: usize,
    show: impl Fn(&Outcome) -> bool,
) -> Vec<(usize, Capped)> {
    let picked: Vec<(usize, (String, Option<Gap>))> = order
        .iter()
        .filter(|&&i| show(&entries[i].1.outcome))
        .map(|&i| {
            let output = entries[i].1.output.as_ref();
            (i, output.map(text::clean_captured).unwrap_or_default())
        })
        .collect();
    let lengths: Vec<usize> = picked.iter().map(|(_, (text, _))| text.len()).collect();
    let budgets = text::budgets(&lengths, OUTPUT_PER_COMMAND, total);
    picked
        .into_iter()
        .zip(budgets)
        .map(|((i, (output, gap)), budget)| (i, text::cap(&output, gap, budget)))
        .collect()
}

fn command_summary(
    planned: &PlannedCommand,
    report: &CommandReport,
    run_cancelled: bool,
    truncated_bytes: u64,
) -> CommandSummary {
    let (status, exit_code, signal) = match &report.outcome {
        Outcome::Passed => ("passed", Some(0), None),
        Outcome::Failed(Failure::Exit(code)) => ("failed", Some(*code), None),
        Outcome::Failed(Failure::Signal(signal)) => ("failed", None, Some(signal_name(*signal))),
        Outcome::Failed(_) => ("failed", None, None),
        Outcome::TimedOut(_) => ("timeout", None, None),
        Outcome::Skipped { .. } => ("skipped", None, None),
        Outcome::Cancelled => ("cancelled", None, None),
        Outcome::NotRun => ("not_run", None, None),
    };
    let detail = match &report.outcome {
        Outcome::Failed(Failure::Spawn(message)) => Some(message.clone()),
        Outcome::Failed(Failure::Modified(_)) | Outcome::TimedOut(_) | Outcome::Skipped { .. } => {
            Some(describe(&report.outcome))
        }
        Outcome::NotRun if run_cancelled => Some("not started: the run was cancelled".into()),
        Outcome::NotRun => Some("not started: fail_fast stopped the run".into()),
        _ => None,
    };
    CommandSummary {
        id: report.id.clone(),
        name: report.name.clone(),
        group: planned.group_path.clone(),
        cmd: planned.command.cmd.clone(),
        status,
        exit_code,
        signal,
        duration_ms: report.duration.map(|d| d.as_millis()),
        detail,
        output_bytes: report.output.as_ref().map(CapturedOutput::total_bytes),
        truncated_bytes,
    }
}

fn signal_name(signal: i32) -> String {
    let exit = ExitInfo {
        code: None,
        signal: Some(signal),
        stop_requested: false,
    };
    exit.signal_name()
        .map_or_else(|| format!("signal {signal}"), str::to_string)
}

/// How a command ended, as a phrase such as `failed with exit code 3`.
fn describe(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Passed => "passed".into(),
        Outcome::Failed(Failure::Exit(code)) => format!("failed with exit code {code}"),
        Outcome::Failed(Failure::Signal(signal)) => {
            format!("failed: killed by {}", signal_name(*signal))
        }
        Outcome::Failed(Failure::Spawn(_)) => "failed to start".into(),
        Outcome::Failed(Failure::Modified(files)) => {
            let files: Vec<String> = files.iter().map(|f| f.display().to_string()).collect();
            format!("failed: it changed tracked files: {}", files.join(", "))
        }
        Outcome::TimedOut(limit) => {
            format!("timed out after {}", humantime::format_duration(*limit))
        }
        Outcome::Skipped { cause } => format!("not run because {cause} failed"),
        Outcome::Cancelled => "cancelled".into(),
        Outcome::NotRun => "not run".into(),
    }
}

/// A command's text block: a `###` header saying how it ended, then its output.
fn block(report: &CommandReport, output: &Capped) -> String {
    let mut block = if report.name == report.id {
        format!("### {}", report.id)
    } else {
        format!("### {} ({})", report.name, report.id)
    };
    let _ = write!(block, ": {}", describe(&report.outcome));
    if let Some(duration) = report.duration {
        let _ = write!(block, " ({:.1}s)", duration.as_secs_f64());
    }
    block.push('\n');
    if let Outcome::Failed(Failure::Spawn(message)) = &report.outcome {
        let _ = writeln!(block, "{message}");
    } else if output.text.is_empty() {
        block.push_str("(no output)\n");
    }
    block.push_str(&output.text);
    block
}

/// One sentence or two on how the run went, for the summary.
fn message(report: &RunReport) -> String {
    let counts = report.counts();
    let plural = |n: usize| if n == 1 { "" } else { "s" };
    if counts.total == 0 {
        return "No commands were selected.".into();
    }
    if report.cancelled {
        return "The run was cancelled: running commands were stopped and the rest not started."
            .into();
    }
    if report.success() {
        return format!("{} command{} passed.", counts.total, plural(counts.total));
    }
    let failed: Vec<&str> = report
        .commands
        .iter()
        .filter(|c| is_failure(&c.outcome))
        .map(|c| c.id.as_str())
        .collect();
    let mut message = format!(
        "{} of {} command{} failed or timed out: {}.",
        failed.len(),
        counts.total,
        plural(counts.total),
        failed.join(", ")
    );
    if counts.skipped > 0 {
        let _ = write!(
            message,
            " {} more didn't run because a dependency failed.",
            counts.skipped
        );
    }
    if counts.cancelled + counts.not_run > 0 {
        message.push_str(" fail_fast stopped the rest.");
    }
    message.push_str(" Their output follows; after fixing, rerun each one with run_lint.");
    message
}
