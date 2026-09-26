//! The tools' results.
//!
//! A run's result is a compact JSON summary that lists failures first, followed by a text block
//! with the output of each command that failed or timed out, and with `verbose` also of those
//! that passed or were cancelled.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rmcp::model::{CallToolResult, Content};
use serde::Serialize;

use super::text::{self, Capped, Gap};
use crate::commands::command::Command;
use crate::process::ExitInfo;
use crate::runner::{
    CapturedOutput, CommandReport, Failure, Outcome, Plan, PlannedCommand, RunReport, SelectReason,
};
use crate::selectors::{SelectedBy, SelectedCommand};

/// Output a result keeps of one command: a fifth from its start, the rest from its end.
const OUTPUT_PER_COMMAND: usize = 20 * 1024;
/// Output a result keeps of all its commands together.
const OUTPUT_TOTAL: usize = 60 * 1024;
/// Matched files a result lists per command.
const MAX_FILES: usize = 5;
/// Ids a message lists.
const MAX_IDS: usize = 10;

/// Which commands a run tool asked for.
#[derive(Debug, Clone)]
pub(super) enum RunScope {
    /// Those the changes select (`run_lints`), compared with `base` if given.
    Changes {
        base: Option<String>,
        include_manual: bool,
    },
    /// Every command (`run_all`).
    All { include_manual: bool },
    /// One command, by id or name (`run_lint`).
    Named(String),
}

#[derive(Serialize)]
pub(super) struct LintInfo {
    id: String,
    name: String,
    cmd: String,
    cwd: String,
    auto_rules: AutoRules,
    depends_on: Vec<String>,
    group: String,
    /// Whether its `auto` rules select it now: `always`, or `git` with a matching change.
    selected: bool,
    /// Whether `run_lints` and `run_all` run it when selected: false for `auto.check: false`.
    runs_in_check: bool,
    /// `always` or `git`, when selected.
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<&'static str>,
    #[serde(flatten)]
    files: MatchedFiles,
}

#[derive(Serialize)]
struct AutoRules {
    git: Option<bool>,
    watch: Option<bool>,
    always: Option<bool>,
    check: Option<bool>,
}

/// The first few changed files that match a command's `auto` rules, and how many there are.
#[derive(Serialize, Default)]
struct MatchedFiles {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    matched_files: Vec<String>,
    #[serde(skip_serializing_if = "is_zero")]
    matched_file_count: usize,
}

impl MatchedFiles {
    /// Up to [`MAX_FILES`] of `files`, relative to `root` when inside it.
    fn new(files: &[PathBuf], root: &Path) -> Self {
        Self {
            matched_files: files
                .iter()
                .take(MAX_FILES)
                .map(|file| {
                    file.strip_prefix(root)
                        .unwrap_or(file)
                        .display()
                        .to_string()
                })
                .collect(),
            matched_file_count: files.len(),
        }
    }
}

/// How `list_lints` describes a command, given what the changes select.
pub(super) fn lint_info(
    cmd: &Command,
    group: String,
    selected: Option<&SelectedCommand>,
    root: &Path,
) -> LintInfo {
    LintInfo {
        id: cmd.id.clone(),
        name: cmd.name.clone(),
        cmd: cmd.cmd.clone(),
        cwd: cmd.cwd.display().to_string(),
        auto_rules: AutoRules {
            git: cmd.auto.git,
            watch: cmd.auto.watch,
            always: cmd.auto.always,
            check: cmd.auto.check,
        },
        depends_on: cmd.depends_on.clone(),
        group,
        selected: selected.is_some(),
        runs_in_check: cmd.auto.check != Some(false),
        reason: selected.map(|s| match s.by {
            SelectedBy::Always => "always",
            SelectedBy::Git => "git",
        }),
        files: selected.map_or_else(MatchedFiles::default, |s| MatchedFiles::new(&s.files, root)),
    }
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
    /// How long the run waited for another to finish first.
    #[serde(skip_serializing_if = "Option::is_none")]
    queued_ms: Option<u128>,
    /// Distinct changed files git selection found, for `run_lints`.
    #[serde(skip_serializing_if = "Option::is_none")]
    changed_files: Option<usize>,
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
    /// `requested`, `all`, `always`, `git`, or `dependency of <ids>`.
    reason: String,
    #[serde(flatten)]
    files: MatchedFiles,
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

fn is_zero<T: Default + PartialEq>(n: &T) -> bool {
    *n == T::default()
}

/// A finished run, and what it was asked for.
pub(super) struct Run<'a> {
    pub scope: &'a RunScope,
    pub plan: &'a Plan,
    pub report: &'a RunReport,
    /// The config's directory; matched files are listed relative to it.
    pub root: &'a Path,
    pub verbose: bool,
    /// How long it waited for another run to finish, if it had to.
    pub queued: Option<Duration>,
    /// Whether any command has `auto.git` or `auto.always`, the rules `run_lints` selects by.
    pub selectable: bool,
}

/// The result of a run: the summary, then a text block per command whose output is shown.
///
/// # Errors
///
/// Returns an error if the summary can't be serialized.
pub(super) fn run_result(run: &Run) -> Result<CallToolResult, serde_json::Error> {
    let Run {
        scope,
        plan,
        report,
        root,
        verbose,
        queued,
        selectable,
    } = *run;
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
        message: message(scope, plan, report, selectable),
        total: counts.total,
        passed: counts.passed,
        failed: counts.failed,
        timed_out: counts.timed_out,
        skipped: counts.skipped,
        cancelled: counts.cancelled,
        not_run: counts.not_run,
        duration_ms: report.duration.as_millis(),
        queued_ms: queued.map(|q| q.as_millis()),
        changed_files: matches!(scope, RunScope::Changes { .. }).then_some(plan.changed_files),
        warnings: plan.warnings.clone(),
        commands: order
            .iter()
            .map(|&i| {
                let truncated = shown
                    .iter()
                    .find(|(j, _)| *j == i)
                    .map_or(0, |(_, capped)| capped.omitted);
                command_summary(
                    entries[i].0,
                    entries[i].1,
                    root,
                    report.cancelled,
                    truncated,
                )
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
    root: &Path,
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
        reason: match &planned.reason {
            SelectReason::Requested => "requested".into(),
            SelectReason::All => "all".into(),
            SelectReason::Always => "always".into(),
            SelectReason::Git => "git".into(),
            SelectReason::Dependency { of } => format!("dependency of {}", of.join(", ")),
        },
        files: planned
            .files
            .as_deref()
            .map_or_else(MatchedFiles::default, |files| {
                MatchedFiles::new(files, root)
            }),
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

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// Up to [`MAX_IDS`] of `ids`, comma-separated.
fn list_ids(ids: &[String]) -> String {
    let mut list = ids[..ids.len().min(MAX_IDS)].join(", ");
    if ids.len() > MAX_IDS {
        let _ = write!(list, " and {} more", ids.len() - MAX_IDS);
    }
    list
}

/// Why a run has no commands, and what to try instead.
fn nothing_selected(scope: &RunScope, plan: &Plan, selectable: bool) -> String {
    let manual = &plan.excluded_manual;
    match scope {
        RunScope::Changes { .. } if !selectable => "No command has auto.git or auto.always \
            set, so run_lints never selects any. run_all runs every command, and run_lint \
            runs one."
            .into(),
        RunScope::Changes { base, .. } => {
            let changed = plan.changed_files;
            let mut message = match changed {
                0 => "No changed files".to_string(),
                n => format!("{n} changed file{}", plural(n)),
            };
            if let Some(base) = base {
                let _ = write!(message, " since the merge base with {base}");
            }
            if !manual.is_empty() {
                let _ = write!(
                    message,
                    "; the only commands they select have auto.check: false: {}. Set \
                     include_manual to run them, or run one with run_lint.",
                    list_ids(manual)
                );
            } else if changed == 0 {
                message.push_str(", so no command was selected.");
            } else {
                message.push_str(", but no command's auto rules match them.");
            }
            message.push_str(" run_all runs every command");
            if base.is_none() {
                message.push_str(
                    ", and base (such as \"origin/main\") selects by the changes since a \
                     branch point instead of uncommitted changes only",
                );
            }
            message.push('.');
            message
        }
        RunScope::All { .. } if !manual.is_empty() => format!(
            "Every command has auto.check: false: {}. Set include_manual to run them, or run \
             one with run_lint.",
            list_ids(manual)
        ),
        RunScope::All { .. } | RunScope::Named(_) => "The config has no commands.".into(),
    }
}

/// One sentence or two on how the run went, for the summary.
fn message(scope: &RunScope, plan: &Plan, report: &RunReport, selectable: bool) -> String {
    let counts = report.counts();
    if counts.total == 0 {
        return nothing_selected(scope, plan, selectable);
    }
    let mut message = outcome_message(report);
    if !plan.excluded_manual.is_empty() {
        let _ = write!(
            message,
            " Not run because of auto.check: false: {} (set include_manual to run them).",
            list_ids(&plan.excluded_manual)
        );
    }
    message
}

fn outcome_message(report: &RunReport) -> String {
    let counts = report.counts();
    if report.cancelled {
        return "The run was cancelled: running commands were stopped and the rest not started."
            .into();
    }
    if report.success() {
        return format!("{} command{} passed.", counts.total, plural(counts.total));
    }
    let failed: Vec<String> = report
        .commands
        .iter()
        .filter(|c| is_failure(&c.outcome))
        .map(|c| c.id.clone())
        .collect();
    let mut message = format!(
        "{} of {} command{} failed or timed out: {}.",
        failed.len(),
        counts.total,
        plural(counts.total),
        list_ids(&failed)
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
