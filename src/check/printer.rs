//! Check mode's output on stderr: one line per command, captured output, and a summary.

use std::collections::HashMap;
use std::io::{IsTerminal, Write};
use std::time::Duration;

use crate::process::ExitInfo;
use crate::runner::{CommandReport, Failure, Outcome, OutputMode, Plan, RunEvent, RunReport};

/// ANSI color helpers — only emit escape codes when stderr is a terminal.
struct Style {
    color: bool,
}

impl Style {
    fn new() -> Self {
        Self {
            color: std::io::stderr().is_terminal(),
        }
    }

    fn style(&self, code: &str, s: &str) -> String {
        if self.color {
            format!("\x1b[{code}m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }

    fn bold(&self, s: &str) -> String {
        self.style("1", s)
    }

    fn green(&self, s: &str) -> String {
        self.style("32", s)
    }

    fn red(&self, s: &str) -> String {
        self.style("31", s)
    }

    fn yellow(&self, s: &str) -> String {
        self.style("33", s)
    }

    fn dim(&self, s: &str) -> String {
        self.style("2", s)
    }
}

fn format_duration(d: Duration) -> String {
    let total_secs = d.as_secs();
    let tenths = d.subsec_millis() / 100;
    if total_secs < 60 {
        format!("{total_secs}.{tenths}s")
    } else {
        let mins = total_secs / 60;
        let secs = total_secs % 60;
        format!("{mins}m {secs}.{tenths}s")
    }
}

/// Prints [`RunEvent`]s as they arrive.
///
/// With streamed output, a command's header goes on its own line before the command writes
/// anything, and its result follows its output. With captured output, a command's result is
/// followed by its output unless it passed and passing output is muted. One command at a time,
/// the header is printed when the command starts and the result ends the same line; with
/// several, the whole line is printed when the command ends.
pub(super) struct Printer {
    sty: Style,
    streaming: bool,
    /// Captured output, one command at a time.
    inline: bool,
    /// The `seq` of the command whose line waits for its result.
    open: Option<usize>,
    mute_success: bool,
    names: HashMap<String, String>,
}

impl Printer {
    pub(super) fn new(plan: &Plan, output: OutputMode, serial: bool, mute_success: bool) -> Self {
        let streaming = output == OutputMode::Inherit;
        Self {
            sty: Style::new(),
            streaming,
            inline: !streaming && serial,
            open: None,
            mute_success,
            names: plan
                .commands
                .iter()
                .map(|c| (c.id().to_string(), c.command.name.clone()))
                .collect(),
        }
    }

    pub(super) fn nothing_selected(&self) {
        eprintln!("{}", self.sty.dim("No commands selected."));
    }

    pub(super) fn event(&mut self, event: &RunEvent) {
        match *event {
            RunEvent::Started { seq, total, cmd } => {
                let header = format!(
                    "{} {}",
                    self.sty.bold(&counter(seq, total)),
                    cmd.command.name
                );
                if self.streaming {
                    eprintln!("{header}");
                } else if self.inline {
                    eprint!("{header} ");
                    self.open = Some(seq);
                }
            }
            RunEvent::Finished {
                seq,
                done,
                total,
                report,
                ..
            } => {
                let status = self.status(report);
                match self.open.take() {
                    Some(open) if open == seq => eprintln!("{status}"),
                    open => {
                        // Another command's line is open; it gets a line of its own when it ends
                        if open.is_some() {
                            eprintln!();
                        }
                        let index = if self.streaming || self.inline {
                            seq
                        } else {
                            done
                        };
                        eprintln!(
                            "{} {} {status}",
                            self.sty.dim(&counter(index, total)),
                            report.name,
                        );
                    }
                }
                self.print_output(report);
            }
        }
    }

    fn status(&self, report: &CommandReport) -> String {
        let sty = &self.sty;
        let text = match &report.outcome {
            Outcome::Passed => sty.green("PASS"),
            Outcome::Failed(failure) => sty.red(&format!("FAIL ({})", describe_failure(failure))),
            Outcome::TimedOut(limit) => {
                sty.red(&format!("TIMEOUT after {}", format_duration(*limit)))
            }
            Outcome::Skipped { cause } => {
                let name = self.names.get(cause).unwrap_or(cause);
                return sty.yellow(&format!("SKIP ({name} failed)"));
            }
            Outcome::Cancelled => sty.yellow("CANCELLED"),
            Outcome::NotRun => sty.dim("NOT RUN"),
        };
        match report.duration {
            Some(duration) => format!("{text} {}", sty.dim(&format_duration(duration))),
            None => text,
        }
    }

    fn print_output(&self, report: &CommandReport) {
        let Some(output) = &report.output else {
            return;
        };
        if output.is_empty() || (self.mute_success && report.outcome == Outcome::Passed) {
            return;
        }
        let mut text = output.text();
        if !text.ends_with('\n') {
            text.push('\n');
        }
        let _ = std::io::stderr().write_all(text.as_bytes());
    }

    /// Print a blank line, then the counts, e.g. `4 commands: 1 passed, 1 failed, 2 skipped`.
    pub(super) fn summary(&self, report: &RunReport) {
        let sty = &self.sty;
        let counts = report.counts();
        let buckets = [
            (counts.passed, "passed", Style::green as Paint),
            (counts.failed, "failed", Style::red),
            (counts.timed_out, "timed out", Style::red),
            (counts.skipped, "skipped", Style::yellow),
            (counts.cancelled, "cancelled", Style::yellow),
            (counts.not_run, "not run", Style::dim),
        ];
        let parts: Vec<String> = buckets
            .into_iter()
            .filter(|(count, _, _)| *count > 0)
            .map(|(count, label, paint)| paint(sty, &format!("{count} {label}")))
            .collect();
        let interrupted = if report.cancelled {
            format!("{} ", sty.yellow("Interrupted."))
        } else {
            String::new()
        };
        eprintln!();
        eprintln!(
            "{interrupted}{} {} {}",
            sty.bold(&format!("{} commands:", counts.total)),
            parts.join(&sty.dim(", ")),
            sty.dim(&format!("({})", format_duration(report.duration)))
        );
    }
}

type Paint = fn(&Style, &str) -> String;

/// `[i/N]`, with `i` padded to the width of `N`.
fn counter(index: usize, total: usize) -> String {
    let width = total.to_string().len();
    format!("[{index:>width$}/{total}]")
}

fn describe_failure(failure: &Failure) -> String {
    match failure {
        Failure::Exit(code) => format!("exit {code}"),
        Failure::Signal(signal) => {
            let exit = ExitInfo {
                code: None,
                signal: Some(*signal),
                stop_requested: false,
            };
            exit.signal_name()
                .map_or_else(|| format!("signal {signal}"), str::to_string)
        }
        Failure::Spawn(message) => message.clone(),
        Failure::Modified(paths) => {
            let paths: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
            format!("modified: {}", paths.join(", "))
        }
    }
}
