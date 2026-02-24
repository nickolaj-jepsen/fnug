use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use std::time::Instant;

use thiserror::Error;

use crate::commands::command::Command;
use crate::commands::group::CommandGroup;
use crate::selectors::{self, SelectorError};

#[derive(Error, Debug)]
pub enum CheckError {
    #[error("selector error: {0}")]
    Selector(#[from] SelectorError),
}

/// Result of executing a single command with captured output.
pub(crate) struct CommandResult {
    pub success: bool,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub duration: std::time::Duration,
}

/// Expand selected commands to include all transitive dependencies.
pub(crate) fn expand_dependencies<'a>(
    selected: &[&'a Command],
    all_commands: &'a [Command],
) -> Vec<&'a Command> {
    let cmd_by_id: HashMap<&str, &Command> =
        all_commands.iter().map(|c| (c.id.as_str(), c)).collect();

    let mut selected_ids: HashSet<String> = selected.iter().map(|c| c.id.clone()).collect();
    let mut queue: VecDeque<String> = selected_ids.iter().cloned().collect();
    while let Some(id) = queue.pop_front() {
        if let Some(cmd) = cmd_by_id.get(id.as_str()) {
            for dep in &cmd.depends_on {
                if selected_ids.insert(dep.clone()) {
                    queue.push_back(dep.clone());
                }
            }
        }
    }

    all_commands
        .iter()
        .filter(|c| selected_ids.contains(&c.id))
        .collect()
}

/// Execute a single command, capturing stdout and stderr.
pub(crate) fn execute_command(cmd: &Command, cwd: &Path) -> CommandResult {
    let cmd_cwd = if cmd.cwd.as_os_str().is_empty() {
        cwd
    } else {
        &cmd.cwd
    };

    let start = Instant::now();
    let output = ProcessCommand::new("sh")
        .arg("-c")
        .arg(&cmd.cmd)
        .current_dir(cmd_cwd)
        .envs(&cmd.env)
        .output();
    let duration = start.elapsed();

    match output {
        Ok(o) => CommandResult {
            success: o.status.success(),
            exit_code: o.status.code(),
            stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
            duration,
        },
        Err(e) => CommandResult {
            success: false,
            exit_code: None,
            stdout: String::new(),
            stderr: e.to_string(),
            duration,
        },
    }
}

/// Result of a headless check run, carrying state for TUI handoff.
pub struct CheckResult {
    pub exit_code: i32,
    /// All command IDs that were selected (including expanded deps).
    pub selected_ids: HashSet<String>,
    /// Command IDs that failed or were skipped due to a dependency failure.
    pub failed_ids: HashSet<String>,
}

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

fn format_duration(d: std::time::Duration) -> String {
    let total_secs = d.as_secs();
    let millis = d.subsec_millis();
    if total_secs < 60 {
        let tenths = millis / 100;
        format!("{total_secs}.{tenths}s")
    } else {
        let mins = total_secs / 60;
        let secs = total_secs % 60;
        let tenths = millis / 100;
        format!("{mins}m {secs}.{tenths}s")
    }
}

/// Run all selected commands headlessly and report results.
///
/// # Errors
///
/// Returns `CheckError::Selector` if git-based command selection fails.
#[allow(clippy::too_many_lines)]
pub fn run(
    config: &CommandGroup,
    cwd: &PathBuf,
    fail_fast: bool,
    mute_success: bool,
) -> Result<CheckResult, CheckError> {
    let sty = Style::new();
    let all_commands: Vec<Command> = config.all_commands().into_iter().cloned().collect();
    let selected = selectors::get_selected_commands(all_commands.clone())?;

    if selected.is_empty() {
        eprintln!("{}", sty.dim("No commands selected."));
        return Ok(CheckResult {
            exit_code: 0,
            selected_ids: HashSet::new(),
            failed_ids: HashSet::new(),
        });
    }

    // Expand dependencies and topologically sort
    let selected_refs: Vec<&Command> = selected.iter().collect();
    let commands_to_run = expand_dependencies(&selected_refs, &all_commands);
    let selected_ids: HashSet<String> = commands_to_run.iter().map(|c| c.id.clone()).collect();
    let ordered = topo_sort(&commands_to_run);

    // Execute sequentially
    let total = ordered.len();
    let total_start = Instant::now();
    let mut passed = 0usize;
    let mut skipped = 0usize;
    let mut failed_ids: HashSet<String> = HashSet::new();
    let counter_width = total.to_string().len();

    for (i, cmd) in ordered.iter().enumerate() {
        let idx = i + 1;
        let prefix = format!("[{idx:>counter_width$}/{total}]");

        // Skip if a dependency failed
        let dep_failed = cmd
            .depends_on
            .iter()
            .any(|dep: &String| failed_ids.contains(dep.as_str()));
        if dep_failed {
            eprintln!(
                "{} {} {}",
                sty.dim(&prefix),
                cmd.name,
                sty.yellow("SKIP (dependency failed)")
            );
            failed_ids.insert(cmd.id.clone());
            skipped += 1;
            continue;
        }

        eprint!("{} {} ", sty.bold(&prefix), cmd.name);
        let _ = std::io::stderr().flush();

        let cmd_cwd = if cmd.cwd.as_os_str().is_empty() {
            cwd
        } else {
            &cmd.cwd
        };

        let success;

        if mute_success {
            let result = execute_command(cmd, cmd_cwd);

            if result.success {
                eprintln!(
                    "{} {}",
                    sty.green("PASS"),
                    sty.dim(&format_duration(result.duration))
                );
                success = true;
            } else {
                eprintln!(
                    "{} {}",
                    sty.red("FAIL"),
                    sty.dim(&format_duration(result.duration))
                );
                let _ = std::io::stderr().write_all(result.stdout.as_bytes());
                let _ = std::io::stderr().write_all(result.stderr.as_bytes());
                success = false;
            }
        } else {
            let start = Instant::now();
            let status = ProcessCommand::new("sh")
                .arg("-c")
                .arg(&cmd.cmd)
                .current_dir(cmd_cwd)
                .envs(&cmd.env)
                .status();
            let elapsed = start.elapsed();

            match status {
                Ok(s) if s.success() => {
                    eprintln!(
                        "{} {}",
                        sty.green("PASS"),
                        sty.dim(&format_duration(elapsed))
                    );
                    success = true;
                }
                _ => {
                    eprintln!("{} {}", sty.red("FAIL"), sty.dim(&format_duration(elapsed)));
                    success = false;
                }
            }
        }

        if success {
            passed += 1;
        } else {
            failed_ids.insert(cmd.id.clone());
            if fail_fast {
                eprintln!();
                print_summary(
                    &sty,
                    passed,
                    failed_ids.len(),
                    skipped,
                    total,
                    total_start.elapsed(),
                );
                return Ok(CheckResult {
                    exit_code: 1,
                    selected_ids,
                    failed_ids,
                });
            }
        }
    }

    eprintln!();
    print_summary(
        &sty,
        passed,
        failed_ids.len(),
        skipped,
        total,
        total_start.elapsed(),
    );
    let exit_code = i32::from(!failed_ids.is_empty());
    Ok(CheckResult {
        exit_code,
        selected_ids,
        failed_ids,
    })
}

fn print_summary(
    sty: &Style,
    passed: usize,
    failed: usize,
    skipped: usize,
    total: usize,
    elapsed: std::time::Duration,
) {
    let mut parts = Vec::new();
    if passed > 0 {
        parts.push(sty.green(&format!("{passed} passed")));
    }
    if failed > 0 {
        parts.push(sty.red(&format!("{failed} failed")));
    }
    if skipped > 0 {
        parts.push(sty.yellow(&format!("{skipped} skipped")));
    }

    eprintln!(
        "{} {} {}",
        sty.bold(&format!("{total} commands:")),
        parts.join(&sty.dim(", ")),
        sty.dim(&format!("({})", format_duration(elapsed)))
    );
}

/// Topological sort using Kahn's algorithm.
/// Commands with no dependencies come first.
pub(crate) fn topo_sort<'a>(commands: &[&'a Command]) -> Vec<&'a Command> {
    let ids: HashSet<&str> = commands.iter().map(|c| c.id.as_str()).collect();

    // in-degree: count of deps that are in our set
    let mut in_degree: HashMap<&str, usize> = HashMap::new();
    let mut dependents: HashMap<&str, Vec<&str>> = HashMap::new();

    for cmd in commands {
        let deg = cmd
            .depends_on
            .iter()
            .filter(|d| ids.contains(d.as_str()))
            .count();
        in_degree.insert(&cmd.id, deg);
        for dep in &cmd.depends_on {
            if ids.contains(dep.as_str()) {
                dependents.entry(dep.as_str()).or_default().push(&cmd.id);
            }
        }
    }

    let cmd_map: HashMap<&str, &&Command> = commands.iter().map(|c| (c.id.as_str(), c)).collect();

    // Seed queue in input order for stable sorting
    let mut queue: VecDeque<&str> = commands
        .iter()
        .filter(|c| in_degree.get(c.id.as_str()) == Some(&0))
        .map(|c| c.id.as_str())
        .collect();

    let mut result = Vec::with_capacity(commands.len());
    while let Some(id) = queue.pop_front() {
        if let Some(cmd) = cmd_map.get(id) {
            result.push(**cmd);
        }
        if let Some(deps) = dependents.get(id) {
            for &dep_id in deps {
                if let Some(deg) = in_degree.get_mut(dep_id) {
                    *deg -= 1;
                    if *deg == 0 {
                        queue.push_back(dep_id);
                    }
                }
            }
        }
    }

    result
}
