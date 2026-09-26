//! The headless executor: runs a [`Plan`] as child processes and reports how each command
//! ended.

use std::collections::HashMap;
use std::io;
use std::num::NonZeroUsize;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

use futures::StreamExt;
use futures::stream::FuturesUnordered;
use log::warn;
use tokio::io::AsyncReadExt;
use tokio::net::unix::pipe;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use super::dag::DagState;
use super::output::{CaptureLimits, CapturedOutput};
use super::plan::{Plan, PlannedCommand};
use super::process::{GroupGuard, ShellInvocation, shell_invocation};
use super::report::{CommandReport, Failure, Outcome, RunReport};
use crate::process::{ExitInfo, ProcessHandle, StopSignal};

/// How long a command gets to exit after `SIGTERM` before it is killed.
pub const KILL_GRACE: Duration = Duration::from_secs(3);

/// How long to keep reading output after the shell exits, for background processes still
/// holding the pipe.
const DRAIN_CAP: Duration = Duration::from_secs(2);

/// Where commands' output goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputMode {
    /// Straight to fnug's stdout and stderr, with fnug's stdin. Commands stay in fnug's process
    /// group, so they can use the terminal and get its Ctrl+C; only one runs at a time.
    Inherit,
    /// Into a [`CapturedOutput`] per command, stdout and stderr merged in order. Each command
    /// leads its own process group, which timeouts and cancellation kill as a whole.
    Capture(CaptureLimits),
}

/// How [`execute`] runs a plan.
#[derive(Debug, Clone)]
pub struct ExecOptions {
    /// How many commands may run at once. [`OutputMode::Inherit`] runs one at a time.
    pub jobs: NonZeroUsize,
    /// After the first failure, start nothing new and kill running commands.
    pub fail_fast: bool,
    pub output: OutputMode,
    /// Kill a command that runs longer than this.
    pub default_timeout: Option<Duration>,
    /// Cancelling it kills running commands and starts nothing new.
    pub cancel: CancellationToken,
    /// How long a timed-out or cancelled command gets after `SIGTERM` before `SIGKILL`.
    pub kill_grace: Duration,
}

impl Default for ExecOptions {
    fn default() -> Self {
        Self {
            jobs: NonZeroUsize::MIN,
            fail_fast: false,
            output: OutputMode::Inherit,
            default_timeout: None,
            cancel: CancellationToken::new(),
            kill_grace: KILL_GRACE,
        }
    }
}

/// Progress reported while a plan runs.
#[derive(Debug)]
pub enum RunEvent<'a> {
    /// `cmd` is about to start. `seq` counts commands started or reported so far, from 1.
    Started {
        seq: usize,
        total: usize,
        cmd: &'a PlannedCommand,
    },
    /// `cmd` has a report: it ended, or will never start. `seq` is the number it started with,
    /// or a new one if it never started, and `done` counts reports so far, from 1.
    Finished {
        seq: usize,
        done: usize,
        total: usize,
        cmd: &'a PlannedCommand,
        report: &'a CommandReport,
    },
}

/// Code run around each command that the executor starts.
pub trait ExecHook: Sync {
    /// What `before` hands to `after`.
    type Token: Send;
    /// Runs right before the command starts.
    fn before(&self, cmd: &PlannedCommand) -> Self::Token;
    /// Runs once the command has ended, and may change its outcome.
    fn after(&self, cmd: &PlannedCommand, token: Self::Token, outcome: &mut Outcome);
}

/// An [`ExecHook`] that does nothing.
pub struct NoHook;

impl ExecHook for NoHook {
    type Token = ();
    fn before(&self, _: &PlannedCommand) {}
    fn after(&self, _: &PlannedCommand, (): (), _: &mut Outcome) {}
}

/// How one started command ended.
struct Ended {
    index: usize,
    outcome: Outcome,
    duration: Duration,
    output: Option<CapturedOutput>,
}

/// Run `plan`, each command after its dependencies pass, up to `opts.jobs` at a time.
///
/// A failed command's dependents are skipped. Cancelling `opts.cancel`, or a failure with
/// `opts.fail_fast`, kills the running commands and starts no more. Commands without their own
/// cwd run in `cwd`.
///
/// Returns once every started command has exited and been reaped, with one report per planned
/// command.
pub async fn execute<H: ExecHook>(
    plan: &Plan,
    cwd: &Path,
    opts: &ExecOptions,
    hook: &H,
    on_event: &mut (dyn FnMut(RunEvent<'_>) + Send),
) -> RunReport {
    let started_at = Instant::now();
    let total = plan.len();
    let index: HashMap<&str, usize> = plan.ids().enumerate().map(|(i, id)| (id, i)).collect();
    let max_running = match opts.output {
        OutputMode::Inherit => 1,
        OutputMode::Capture(_) => opts.jobs.get(),
    };
    let mut dag = DagState::from_plan(plan);
    let mut board = Board {
        plan,
        reports: (0..total).map(|_| None).collect(),
        seqs: vec![0; total],
        next_seq: 0,
        done: 0,
    };
    // Cancelled on fail-fast, and with `opts.cancel`
    let stop = opts.cancel.child_token();
    let mut stopping = false;
    let mut running = FuturesUnordered::new();

    loop {
        if !stopping && opts.cancel.is_cancelled() {
            stopping = true;
            board.not_run(dag.cancel_pending(), &index, on_event);
        }
        while !stopping && let Some(id) = dag.pop_ready(max_running) {
            let i = index[id.as_str()];
            let seq = board.start(i);
            on_event(RunEvent::Started {
                seq,
                total,
                cmd: &plan.commands[i],
            });
            running.push(run_command(i, &plan.commands[i], cwd, opts, hook, &stop));
        }
        if running.is_empty() {
            break;
        }

        tokio::select! {
            Some(ended) = running.next() => {
                let passed = ended.outcome == Outcome::Passed;
                let id = plan.commands[ended.index].id();
                let skipped = dag.finish(id, passed);
                board.finish(ended, on_event);
                for (id, cause) in skipped {
                    board.report_unstarted(index[id.as_str()], Outcome::Skipped { cause }, on_event);
                }
                if !passed && opts.fail_fast && !stopping {
                    stopping = true;
                    board.not_run(dag.cancel_pending(), &index, on_event);
                    stop.cancel();
                }
            }
            () = opts.cancel.cancelled(), if !stopping => {}
        }
    }

    let commands = board
        .reports
        .into_iter()
        .zip(&plan.commands)
        .map(|(report, cmd)| report.unwrap_or_else(|| unstarted_report(cmd, Outcome::NotRun)))
        .collect();
    RunReport {
        commands,
        duration: started_at.elapsed(),
        cancelled: opts.cancel.is_cancelled(),
    }
}

/// Reports gathered so far, and the numbering of events.
struct Board<'a> {
    plan: &'a Plan,
    reports: Vec<Option<CommandReport>>,
    seqs: Vec<usize>,
    next_seq: usize,
    done: usize,
}

impl Board<'_> {
    fn start(&mut self, i: usize) -> usize {
        self.next_seq += 1;
        self.seqs[i] = self.next_seq;
        self.next_seq
    }

    fn finish(&mut self, ended: Ended, on_event: &mut (dyn FnMut(RunEvent<'_>) + Send)) {
        let cmd = &self.plan.commands[ended.index];
        let report = CommandReport {
            id: cmd.id().to_string(),
            name: cmd.command.name.clone(),
            outcome: ended.outcome,
            duration: Some(ended.duration),
            output: ended.output,
        };
        self.record(ended.index, report, on_event);
    }

    fn report_unstarted(
        &mut self,
        i: usize,
        outcome: Outcome,
        on_event: &mut (dyn FnMut(RunEvent<'_>) + Send),
    ) {
        self.start(i);
        self.record(
            i,
            unstarted_report(&self.plan.commands[i], outcome),
            on_event,
        );
    }

    fn not_run(
        &mut self,
        ids: Vec<String>,
        index: &HashMap<&str, usize>,
        on_event: &mut (dyn FnMut(RunEvent<'_>) + Send),
    ) {
        for id in ids {
            self.report_unstarted(index[id.as_str()], Outcome::NotRun, on_event);
        }
    }

    fn record(
        &mut self,
        i: usize,
        report: CommandReport,
        on_event: &mut (dyn FnMut(RunEvent<'_>) + Send),
    ) {
        self.done += 1;
        let report = self.reports[i].insert(report);
        on_event(RunEvent::Finished {
            seq: self.seqs[i],
            done: self.done,
            total: self.plan.len(),
            cmd: &self.plan.commands[i],
            report,
        });
    }
}

fn unstarted_report(cmd: &PlannedCommand, outcome: Outcome) -> CommandReport {
    CommandReport {
        id: cmd.id().to_string(),
        name: cmd.command.name.clone(),
        outcome,
        duration: None,
        output: None,
    }
}

async fn run_command<H: ExecHook>(
    index: usize,
    cmd: &PlannedCommand,
    cwd: &Path,
    opts: &ExecOptions,
    hook: &H,
    stop: &CancellationToken,
) -> Ended {
    let token = hook.before(cmd);
    let invocation = shell_invocation(&cmd.command, cwd);
    let started = Instant::now();
    let (mut outcome, output) = match Spawned::spawn(&invocation, opts.output) {
        Ok(spawned) => spawned.run(opts, stop).await,
        Err(e) => (Outcome::Failed(Failure::Spawn(e.to_string())), None),
    };
    let duration = started.elapsed();
    hook.after(cmd, token, &mut outcome);
    Ended {
        index,
        outcome,
        duration,
        output,
    }
}

/// A started command.
struct Spawned {
    guard: GroupGuard,
    /// Filled by the waiter thread once the child exits; it still has to be reaped.
    exit: oneshot::Receiver<(io::Result<ExitInfo>, Child)>,
    /// The read end of the merged stdout/stderr pipe, when capturing.
    pipe: Option<(pipe::Receiver, CapturedOutput)>,
}

impl Spawned {
    fn spawn(invocation: &ShellInvocation, output: OutputMode) -> io::Result<Self> {
        if !invocation.cwd.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "working directory {} does not exist",
                    invocation.cwd.display()
                ),
            ));
        }
        let mut command = invocation.command();
        let capture = match output {
            OutputMode::Inherit => None,
            OutputMode::Capture(limits) => {
                let (reader, writer) = io::pipe()?;
                command
                    .stdin(Stdio::null())
                    .stdout(writer.try_clone()?)
                    .stderr(writer)
                    .process_group(0);
                Some((reader, limits))
            }
        };
        let child = command.spawn().map_err(|e| {
            io::Error::new(
                e.kind(),
                format!("failed to spawn {}: {e}", invocation.program),
            )
        })?;
        // The builder holds copies of the pipe's write end; the read end sees EOF only once
        // every copy is closed
        drop(command);

        let handle = if capture.is_some() {
            ProcessHandle::new(child.id())
        } else {
            ProcessHandle::new_single(child.id())
        };
        let guard = GroupGuard::new(handle.clone());
        let pipe = capture
            .map(|(reader, limits)| {
                pipe::Receiver::from_owned_fd(reader.into())
                    .map(|rx| (rx, CapturedOutput::new(limits)))
            })
            .transpose()?;

        // A dedicated thread, since `wait_exit` blocks and must be the only wait on the pid
        let (tx, exit) = oneshot::channel();
        std::thread::Builder::new()
            .name("fnug-wait".into())
            .spawn(move || {
                let exit = handle.wait_exit();
                if let Err((_, mut child)) = tx.send((exit, child)) {
                    let _ = handle.reap_with(|| child.wait());
                }
            })?;

        Ok(Self { guard, exit, pipe })
    }

    /// Wait for the command to exit, killing it on timeout or `stop`, then clean up after it.
    async fn run(
        mut self,
        opts: &ExecOptions,
        stop: &CancellationToken,
    ) -> (Outcome, Option<CapturedOutput>) {
        let group = self.pipe.is_some();
        let timeout = opts.default_timeout;
        let deadline = tokio::time::sleep(timeout.unwrap_or(Duration::MAX));
        tokio::pin!(deadline);
        let mut buf = vec![0; 16 * 1024];
        let mut eof = !group;
        let mut timed_out = false;
        let mut cancelled = false;

        let waited = loop {
            tokio::select! {
                read = read_pipe(self.pipe.as_mut(), &mut buf), if !eof => {
                    eof = self.push_output(read, &buf);
                }
                waited = &mut self.exit => break waited,
                () = &mut deadline, if timeout.is_some() && !timed_out && !cancelled => {
                    timed_out = true;
                    self.stop(opts.kill_grace);
                }
                () = stop.cancelled(), if !timed_out && !cancelled => {
                    cancelled = true;
                    self.stop(opts.kill_grace);
                }
            }
        };

        let exit = match waited {
            Ok((exit, child)) => {
                self.guard.set_exited(child);
                exit
            }
            Err(_) => Err(io::Error::other("the process waiter stopped")),
        };
        if group {
            // Background processes the shell left behind would hold the pipe open. The group
            // leader is not reaped yet, so its group id can't have been reused
            self.signal(StopSignal::Terminate);
            if !eof {
                let drain = async {
                    while !eof {
                        let read = read_pipe(self.pipe.as_mut(), &mut buf).await;
                        eof = self.push_output(read, &buf);
                    }
                };
                let _ = tokio::time::timeout(DRAIN_CAP, drain).await;
            }
            self.signal(StopSignal::Kill);
        }
        let output = self.pipe.take().map(|(_, output)| output);
        self.guard.reap();

        let outcome = match exit {
            Err(e) => Outcome::Failed(Failure::Spawn(format!("lost track of the process: {e}"))),
            Ok(_) if timed_out => Outcome::TimedOut(timeout.unwrap_or_default()),
            Ok(exit) if exit.success() => Outcome::Passed,
            Ok(_) if cancelled || stop.is_cancelled() => Outcome::Cancelled,
            Ok(ExitInfo {
                code: Some(code), ..
            }) => Outcome::Failed(Failure::Exit(code)),
            Ok(ExitInfo {
                signal: Some(signal),
                ..
            }) => Outcome::Failed(Failure::Signal(signal)),
            Ok(_) => Outcome::Failed(Failure::Exit(1)),
        };
        (outcome, output)
    }

    /// Append a read's bytes to the output; returns whether the pipe is done.
    fn push_output(&mut self, read: io::Result<usize>, buf: &[u8]) -> bool {
        match read {
            Ok(0) => true,
            Ok(n) => {
                if let Some((_, output)) = &mut self.pipe {
                    output.push(&buf[..n]);
                }
                false
            }
            Err(e) => {
                warn!("Failed to read command output: {e}");
                true
            }
        }
    }

    fn stop(&self, grace: Duration) {
        let handle = self.guard.handle();
        if let Err(e) = handle.stop(StopSignal::Terminate, grace) {
            warn!("Failed to stop pid {}: {e}", handle.pid());
        }
    }

    fn signal(&self, sig: StopSignal) {
        let handle = self.guard.handle();
        if let Err(e) = handle.signal(sig) {
            warn!("Failed to signal pid {}: {e}", handle.pid());
        }
    }
}

async fn read_pipe(
    pipe: Option<&mut (pipe::Receiver, CapturedOutput)>,
    buf: &mut [u8],
) -> io::Result<usize> {
    match pipe {
        Some((rx, _)) => rx.read(buf).await,
        None => Ok(0),
    }
}
