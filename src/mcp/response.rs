//! The tools' results.
//!
//! A run's result is a compact JSON summary that lists failures first, followed by a text block
//! with the output of each command that failed or timed out, and with `verbose` also of those
//! that passed or were cancelled, as many as fit the result's size limits.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rmcp::model::{CallToolResult, Content};
use serde::Serialize;

use super::text::{self, Gap};
use crate::commands::command::Command;
use crate::process::ExitInfo;
use crate::runner::{
    CapturedOutput, CommandReport, Failure, Outcome, Plan, PlannedCommand, RunReport, SelectReason,
};
use crate::selectors::{SelectedBy, SelectedCommand};

/// Output a result keeps of one command: a fifth from its start, the rest from its end.
const OUTPUT_PER_COMMAND: usize = 20 * 1024;
/// Size of a result's text blocks together, their headers and omission markers included.
const OUTPUT_TOTAL: usize = 60 * 1024;
/// Output a command's block keeps at least, when it has that much; commands after one that
/// doesn't fit that get no block.
const MIN_OUTPUT: usize = 512;
/// Size of the JSON summary at most; entries at the end of its command list make way.
const SUMMARY_MAX: usize = 16 * 1024;
/// Matched files a result lists per command, and changed files a failure lists.
const MAX_FILES: usize = 5;
/// Ids a message lists.
const MAX_IDS: usize = 10;
/// Warnings a summary lists.
const MAX_WARNINGS: usize = 10;

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
    /// Entries left out at the end of `commands`, to keep the summary within [`SUMMARY_MAX`].
    #[serde(skip_serializing_if = "is_zero")]
    commands_omitted: usize,
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
    /// Bytes of its output left out of its text block, or all of them when it got none.
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
    /// For `run_lint` by an exact id: the other commands with that name, as `(id, group path)`.
    pub also_named: &'a [(String, String)],
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
        also_named,
    } = *run;
    // Reports are in plan order
    let entries: Vec<(&PlannedCommand, &CommandReport)> =
        plan.commands.iter().zip(&report.commands).collect();
    let mut order: Vec<usize> = (0..entries.len()).collect();
    order.sort_by_key(|&i| rank(&entries[i].1.outcome));

    // Failures get the output budget first
    let mut shown = blocks(&entries, &order, OUTPUT_TOTAL, is_failure);
    let failures_left_out: Vec<String> = shown
        .left_out
        .iter()
        .map(|&(i, _)| entries[i].1.id.clone())
        .collect();
    if verbose {
        let left = OUTPUT_TOTAL.saturating_sub(shown.size());
        let extras = blocks(&entries, &order, left, |outcome| {
            matches!(outcome, Outcome::Passed | Outcome::Cancelled)
        });
        shown.blocks.extend(extras.blocks);
        shown.left_out.extend(extras.left_out);
    }
    shown
        .blocks
        .sort_by_key(|block| (rank(&entries[block.index].1.outcome), block.index));

    let mut message = message(scope, plan, report, selectable);
    if let RunScope::Named(target) = scope
        && !also_named.is_empty()
    {
        let others: Vec<String> = also_named
            .iter()
            .map(|(id, group)| format!("{id} ({group})"))
            .collect();
        let _ = write!(
            message,
            " '{target}' is also the name of {}; run_lint takes an id to run one of those \
             instead.",
            list_ids(&others)
        );
    }
    if !failures_left_out.is_empty() {
        let _ = write!(
            message,
            " Output of {} of them is left out to keep the result small: {}; run_lint shows it.",
            failures_left_out.len(),
            list_ids(&failures_left_out)
        );
    }
    let counts = report.counts();
    let mut summary = Summary {
        ok: report.success(),
        message,
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
        warnings: capped_warnings(&plan.warnings),
        commands: order
            .iter()
            .map(|&i| {
                command_summary(
                    entries[i].0,
                    entries[i].1,
                    root,
                    report.cancelled,
                    shown.omitted(i),
                )
            })
            .collect(),
        commands_omitted: 0,
    };

    let mut content = vec![Content::text(summary_json(&mut summary)?)];
    content.extend(
        shown
            .blocks
            .into_iter()
            .map(|block| Content::text(block.text)),
    );
    Ok(CallToolResult::success(content))
}

/// Serialize `summary`, leaving out as few entries at the end of its command list as keep it
/// within [`SUMMARY_MAX`]; its message and `commands_omitted` then say how many.
fn summary_json(summary: &mut Summary) -> Result<String, serde_json::Error> {
    let json = serde_json::to_string(summary)?;
    if json.len() <= SUMMARY_MAX {
        return Ok(json);
    }
    let mut commands = std::mem::take(&mut summary.commands);
    let message = std::mem::take(&mut summary.message);
    let note = |omitted: usize| {
        format!(
            "{message} To keep the result small, the commands list leaves out its last \
             {omitted} entries."
        )
    };
    // Sized for leaving out every entry, the count with the most digits
    summary.message = note(commands.len());
    summary.commands_omitted = commands.len();
    let mut size = serde_json::to_string(summary)?.len();
    let mut kept = 0;
    for command in &commands {
        size += serde_json::to_string(command)?.len() + 1;
        if size > SUMMARY_MAX {
            break;
        }
        kept += 1;
    }
    summary.commands_omitted = commands.len() - kept;
    summary.message = note(summary.commands_omitted);
    commands.truncate(kept);
    summary.commands = commands;
    serde_json::to_string(summary)
}

/// Up to [`MAX_WARNINGS`] of `warnings`, then how many more there are.
fn capped_warnings(warnings: &[String]) -> Vec<String> {
    let mut capped: Vec<String> = warnings.iter().take(MAX_WARNINGS).cloned().collect();
    if warnings.len() > MAX_WARNINGS {
        capped.push(format!(
            "… and {} more warnings",
            warnings.len() - MAX_WARNINGS
        ));
    }
    capped
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

/// A command's text block.
struct Block {
    /// Into the run's entries.
    index: usize,
    text: String,
    /// Bytes of the command's output left out of `text`.
    omitted: u64,
}

/// The text blocks of some of a run's commands, and those left out for want of room.
#[derive(Default)]
struct Shown {
    blocks: Vec<Block>,
    /// Entry indices of commands without a block, with the size of their output.
    left_out: Vec<(usize, u64)>,
}

impl Shown {
    fn size(&self) -> usize {
        self.blocks.iter().map(|block| block.text.len()).sum()
    }

    /// Bytes of entry `index`'s output left out of the result.
    fn omitted(&self, index: usize) -> u64 {
        if let Some(block) = self.blocks.iter().find(|block| block.index == index) {
            return block.omitted;
        }
        self.left_out
            .iter()
            .find(|(i, _)| *i == index)
            .map_or(0, |(_, bytes)| *bytes)
    }
}

/// The blocks of the commands in `order` whose outcome `show` picks, together at most `total`
/// bytes: each holds a header and at most [`OUTPUT_PER_COMMAND`] of cleaned output, with a
/// marker line where some is left out. Once a command's block can't get [`MIN_OUTPUT`] of its
/// output, it and the commands after it get none.
fn blocks(
    entries: &[(&PlannedCommand, &CommandReport)],
    order: &[usize],
    total: usize,
    show: impl Fn(&Outcome) -> bool,
) -> Shown {
    struct Picked {
        index: usize,
        header: String,
        output: String,
        gap: Option<Gap>,
        /// The header, and room for a marker line unless there is no output at all.
        fixed: usize,
    }
    let picked = order
        .iter()
        .filter(|&&i| show(&entries[i].1.outcome))
        .map(|&i| {
            let report = entries[i].1;
            let (output, gap) = report
                .output
                .as_ref()
                .map(text::clean_captured)
                .unwrap_or_default();
            let empty = output.is_empty() && gap.is_none();
            let header = header(report, empty);
            let marker = if empty {
                0
            } else {
                text::marker_room(&output, gap)
            };
            Picked {
                index: i,
                fixed: header.len() + marker,
                header,
                output,
                gap,
            }
        });

    let mut shown = Shown::default();
    let mut kept: Vec<Picked> = Vec::new();
    let mut floor = 0;
    for p in picked {
        floor += p.fixed + p.output.len().min(MIN_OUTPUT);
        if floor > total || !shown.left_out.is_empty() {
            let bytes = p.output.len() as u64 + p.gap.map_or(0, |gap| gap.bytes);
            shown.left_out.push((p.index, bytes));
        } else {
            kept.push(p);
        }
    }

    let fixed: usize = kept.iter().map(|p| p.fixed).sum();
    let lengths: Vec<usize> = kept.iter().map(|p| p.output.len()).collect();
    let budgets = text::budgets(&lengths, OUTPUT_PER_COMMAND, total - fixed);
    shown.blocks = kept
        .into_iter()
        .zip(budgets)
        .map(|(p, budget)| {
            let capped = text::cap(&p.output, p.gap, budget);
            Block {
                index: p.index,
                text: p.header + &capped.text,
                omitted: capped.omitted,
            }
        })
        .collect();
    shown
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
            let mut list: Vec<String> = files
                .iter()
                .take(MAX_FILES)
                .map(|f| f.display().to_string())
                .collect();
            if files.len() > MAX_FILES {
                list.push(format!("and {} more", files.len() - MAX_FILES));
            }
            format!("failed: it changed tracked files: {}", list.join(", "))
        }
        Outcome::TimedOut(limit) => {
            format!("timed out after {}", humantime::format_duration(*limit))
        }
        Outcome::Skipped { cause } => format!("not run because {cause} failed"),
        Outcome::Cancelled => "cancelled".into(),
        Outcome::NotRun => "not run".into(),
    }
}

/// The start of a command's text block: a `###` line saying how it ended, then why it failed to
/// start, or `(no output)` if it has `no_output`.
fn header(report: &CommandReport, no_output: bool) -> String {
    let mut header = if report.name == report.id {
        format!("### {}", report.id)
    } else {
        format!("### {} ({})", report.name, report.id)
    };
    let _ = write!(header, ": {}", describe(&report.outcome));
    if let Some(duration) = report.duration {
        let _ = write!(header, " ({:.1}s)", duration.as_secs_f64());
    }
    header.push('\n');
    if let Outcome::Failed(Failure::Spawn(message)) = &report.outcome {
        let _ = writeln!(header, "{message}");
    } else if no_output {
        header.push_str("(no output)\n");
    }
    header
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
