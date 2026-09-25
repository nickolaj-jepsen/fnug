//! Exit tracking and signalling for child processes.
//!
//! A [`ProcessHandle`] observes a child's exit without reaping it. Until its owner reaps the
//! zombie, the pid and process group id stay reserved, so signals can never hit a reused pid.

#[cfg(not(unix))]
compile_error!("fnug supports Linux and macOS only");

use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};

use log::{debug, warn};
use parking_lot::{Condvar, Mutex};

/// Signal used to stop a process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopSignal {
    Interrupt,
    Terminate,
    Hangup,
    Kill,
}

impl StopSignal {
    fn raw(self) -> libc::c_int {
        match self {
            StopSignal::Interrupt => libc::SIGINT,
            StopSignal::Terminate => libc::SIGTERM,
            StopSignal::Hangup => libc::SIGHUP,
            StopSignal::Kill => libc::SIGKILL,
        }
    }
}

/// How a process ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExitInfo {
    /// Exit code, if the process exited normally.
    pub code: Option<i32>,
    /// Number of the signal that terminated the process.
    pub signal: Option<i32>,
    /// Whether a stop was requested through the handle before the process exited.
    pub stop_requested: bool,
}

impl ExitInfo {
    /// Whether the process exited with code 0.
    #[must_use]
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }

    /// Name of the terminating signal, such as `"SIGSEGV"`.
    #[must_use]
    pub fn signal_name(&self) -> Option<&'static str> {
        self.signal.and_then(signal_name)
    }

    /// Exit code in shell convention: the exit code, or 128 plus the signal number.
    #[must_use]
    pub fn shell_code(&self) -> u32 {
        match (self.code, self.signal) {
            (Some(code), _) => u32::try_from(code).unwrap_or(1),
            (None, Some(signal)) => 128 + u32::try_from(signal).unwrap_or(0),
            (None, None) => 1,
        }
    }

    /// Short description: `"stopped"`, `"terminated by SIGSEGV"` or `"exit code 3"`.
    #[must_use]
    pub fn describe(&self) -> String {
        if self.stop_requested {
            return "stopped".into();
        }
        match (self.signal, self.signal_name()) {
            (_, Some(name)) => format!("terminated by {name}"),
            (Some(signal), None) => format!("terminated by signal {signal}"),
            (None, None) => format!("exit code {}", self.shell_code()),
        }
    }
}

impl From<std::process::ExitStatus> for ExitInfo {
    fn from(status: std::process::ExitStatus) -> Self {
        use std::os::unix::process::ExitStatusExt;
        Self {
            code: status.code(),
            signal: status.signal(),
            stop_requested: false,
        }
    }
}

fn signal_name(signal: i32) -> Option<&'static str> {
    Some(match signal {
        libc::SIGHUP => "SIGHUP",
        libc::SIGINT => "SIGINT",
        libc::SIGQUIT => "SIGQUIT",
        libc::SIGILL => "SIGILL",
        libc::SIGTRAP => "SIGTRAP",
        libc::SIGABRT => "SIGABRT",
        libc::SIGBUS => "SIGBUS",
        libc::SIGFPE => "SIGFPE",
        libc::SIGKILL => "SIGKILL",
        libc::SIGUSR1 => "SIGUSR1",
        libc::SIGSEGV => "SIGSEGV",
        libc::SIGUSR2 => "SIGUSR2",
        libc::SIGPIPE => "SIGPIPE",
        libc::SIGALRM => "SIGALRM",
        libc::SIGTERM => "SIGTERM",
        libc::SIGXCPU => "SIGXCPU",
        libc::SIGXFSZ => "SIGXFSZ",
        libc::SIGSYS => "SIGSYS",
        _ => return None,
    })
}

/// Which processes a [`ProcessHandle`] signals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalScope {
    /// The process group led by the pid.
    Group,
    /// Only the pid, for children that stay in fnug's process group.
    Process,
}

#[derive(Debug, Default)]
struct State {
    exit: Option<ExitInfo>,
    reaped: bool,
    stop_requested: bool,
}

#[derive(Debug)]
struct Inner {
    pid: u32,
    raw_pid: libc::pid_t,
    scope: SignalScope,
    state: Mutex<State>,
    cond: Condvar,
}

/// Shared handle to a child process, from spawn until it is reaped.
///
/// One owner thread calls [`wait_exit`](Self::wait_exit) and later
/// [`reap_with`](Self::reap_with); nothing else may wait on the pid. Any clone can signal
/// the process until it is reaped, after which signals are no-ops.
#[derive(Clone, Debug)]
pub struct ProcessHandle(Arc<Inner>);

impl ProcessHandle {
    /// Handle for a child that leads its own process group; signals reach the whole group.
    ///
    /// # Panics
    ///
    /// Panics if `pid` is 0 or does not fit in `pid_t`.
    #[must_use]
    pub fn new(pid: u32) -> Self {
        Self::with_scope(pid, SignalScope::Group)
    }

    /// Handle for a child that shares fnug's process group; signals reach only `pid`.
    ///
    /// # Panics
    ///
    /// Panics if `pid` is 0 or does not fit in `pid_t`.
    #[must_use]
    pub fn new_single(pid: u32) -> Self {
        Self::with_scope(pid, SignalScope::Process)
    }

    fn with_scope(pid: u32, scope: SignalScope) -> Self {
        // pid 0 would make kill/killpg target fnug's own process group
        let raw_pid = libc::pid_t::try_from(pid)
            .ok()
            .filter(|p| *p > 0)
            .expect("pid must be a positive pid_t");
        Self(Arc::new(Inner {
            pid,
            raw_pid,
            scope,
            state: Mutex::new(State::default()),
            cond: Condvar::new(),
        }))
    }

    #[must_use]
    pub fn pid(&self) -> u32 {
        self.0.pid
    }

    #[must_use]
    pub fn scope(&self) -> SignalScope {
        self.0.scope
    }

    /// Block until the process exits and record how it ended, leaving it unreaped.
    ///
    /// # Errors
    ///
    /// Returns the `waitid` error, e.g. `ECHILD` if the pid is not an unreaped child.
    pub fn wait_exit(&self) -> io::Result<ExitInfo> {
        // SAFETY: siginfo_t is plain old data, so all zeroes is a valid value.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        loop {
            // SAFETY: `info` is a valid, exclusively borrowed siginfo_t for the whole call.
            let ret = unsafe {
                libc::waitid(
                    libc::P_PID,
                    self.0.pid,
                    &raw mut info,
                    libc::WEXITED | libc::WNOWAIT,
                )
            };
            if ret == 0 {
                break;
            }
            let err = io::Error::last_os_error();
            if err.kind() != io::ErrorKind::Interrupted {
                return Err(err);
            }
        }
        // SAFETY: a successful WEXITED waitid fills in the SIGCHLD fields.
        let status = unsafe { info.si_status() };
        let (code, signal) = if info.si_code == libc::CLD_EXITED {
            (Some(status), None)
        } else {
            (None, Some(status))
        };

        let mut state = self.0.state.lock();
        let exit = ExitInfo {
            code,
            signal,
            stop_requested: state.stop_requested,
        };
        state.exit = Some(exit.clone());
        self.0.cond.notify_all();
        Ok(exit)
    }

    /// Mark the process reaped, then run `reap` (the owner's `wait`) to release the zombie.
    ///
    /// Call only after [`wait_exit`](Self::wait_exit) returned.
    pub fn reap_with<R>(&self, reap: impl FnOnce() -> R) -> R {
        {
            let mut state = self.0.state.lock();
            state.reaped = true;
            self.0.cond.notify_all();
        }
        reap()
    }

    /// Send `sig` to the process group, or to the process for [`new_single`](Self::new_single).
    ///
    /// Returns whether the signal was sent: `Ok(false)` once the process is reaped or when
    /// nothing is left to signal.
    ///
    /// # Errors
    ///
    /// Returns the `kill`/`killpg` error, except `ESRCH`.
    pub fn signal(&self, sig: StopSignal) -> io::Result<bool> {
        let state = self.0.state.lock();
        self.signal_locked(&state, sig)
    }

    // The caller holds the state lock, so `reap_with` cannot release the pid mid-call.
    fn signal_locked(&self, state: &State, sig: StopSignal) -> io::Result<bool> {
        if state.reaped {
            return Ok(false);
        }
        // SAFETY: plain syscalls on a pid we still own (running, or a zombie we haven't reaped).
        let ret = unsafe {
            match self.0.scope {
                SignalScope::Group => libc::killpg(self.0.raw_pid, sig.raw()),
                SignalScope::Process => libc::kill(self.0.raw_pid, sig.raw()),
            }
        };
        if ret == 0 {
            return Ok(true);
        }
        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::ESRCH) {
            Ok(false)
        } else {
            Err(err)
        }
    }

    /// Send `sig`, then `SIGKILL` if the process is still unreaped after `grace`.
    ///
    /// A stop sent before the process exits is recorded in its [`ExitInfo::stop_requested`].
    /// Returns whether `sig` was sent.
    ///
    /// # Errors
    ///
    /// Returns the error from sending `sig`.
    pub fn stop(&self, sig: StopSignal, grace: Duration) -> io::Result<bool> {
        let sent = {
            let mut state = self.0.state.lock();
            let sent = self.signal_locked(&state, sig)?;
            if sent && state.exit.is_none() {
                state.stop_requested = true;
            }
            sent
        };
        if sent && sig != StopSignal::Kill {
            self.escalate_after(grace);
        }
        Ok(sent)
    }

    /// Send `SIGKILL` now, recording a stop if the process has not exited.
    ///
    /// # Errors
    ///
    /// Returns the error from sending the signal.
    pub fn force_kill(&self) -> io::Result<bool> {
        self.stop(StopSignal::Kill, Duration::ZERO)
    }

    fn escalate_after(&self, grace: Duration) {
        let handle = self.clone();
        let spawned = std::thread::Builder::new()
            .name("fnug-kill".into())
            .spawn(move || {
                if handle.wait_reaped(grace) {
                    return;
                }
                debug!("pid {} survived {grace:?}, sending SIGKILL", handle.pid());
                if let Err(e) = handle.signal(StopSignal::Kill) {
                    warn!("Failed to kill pid {}: {e}", handle.pid());
                }
            });
        if let Err(e) = spawned {
            warn!(
                "Failed to start kill escalation for pid {}: {e}",
                self.pid()
            );
        }
    }

    /// How the process ended, once [`wait_exit`](Self::wait_exit) has observed it.
    #[must_use]
    pub fn exit_info(&self) -> Option<ExitInfo> {
        self.0.state.lock().exit.clone()
    }

    /// Block until the exit is observed or `timeout` passes.
    #[must_use]
    pub fn wait_timeout(&self, timeout: Duration) -> Option<ExitInfo> {
        let deadline = Instant::now() + timeout;
        let mut state = self.0.state.lock();
        while state.exit.is_none() && !self.0.cond.wait_until(&mut state, deadline).timed_out() {}
        state.exit.clone()
    }

    fn wait_reaped(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut state = self.0.state.lock();
        while !state.reaped && !self.0.cond.wait_until(&mut state, deadline).timed_out() {}
        state.reaped
    }

    #[must_use]
    pub fn is_reaped(&self) -> bool {
        self.0.state.lock().reaped
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::process::CommandExt;
    use std::path::Path;
    use std::process::Command;
    use std::thread::JoinHandle;
    use std::time::Duration;

    use super::{ExitInfo, ProcessHandle, SignalScope, StopSignal};
    use crate::pty::test_util::wait_until;

    const TIMEOUT: Duration = Duration::from_secs(5);

    /// Kills the process when dropped, so a failed assertion doesn't leak it.
    struct Spawned {
        handle: ProcessHandle,
        waiter: Option<JoinHandle<std::io::Result<ExitInfo>>>,
    }

    impl Spawned {
        fn join(mut self) -> ExitInfo {
            self.waiter.take().unwrap().join().unwrap().unwrap()
        }
    }

    impl Drop for Spawned {
        fn drop(&mut self) {
            let _ = self.handle.force_kill();
        }
    }

    /// Run `script` under `sh` in `dir`; the waiter thread records the exit and then reaps.
    fn spawn(script: &str, dir: &Path, group: bool) -> Spawned {
        let mut command = Command::new("sh");
        command.args(["-c", script]).current_dir(dir);
        if group {
            command.process_group(0);
        }
        let mut child = command.spawn().unwrap();
        let handle = if group {
            ProcessHandle::new(child.id())
        } else {
            ProcessHandle::new_single(child.id())
        };
        let owner = handle.clone();
        let waiter = std::thread::spawn(move || {
            let exit = owner.wait_exit();
            owner.reap_with(|| child.wait()).unwrap();
            exit
        });
        Spawned {
            handle,
            waiter: Some(waiter),
        }
    }

    #[test]
    fn exit_info_code_and_signal() {
        let dir = tempfile::tempdir().unwrap();

        let exit = spawn("exit 3", dir.path(), true).join();
        assert_eq!(exit.code, Some(3));
        assert_eq!(exit.signal, None);
        assert!(!exit.stop_requested);
        assert_eq!(exit.shell_code(), 3);
        assert_eq!(exit.describe(), "exit code 3");

        let exit = spawn("ulimit -c 0; kill -SEGV $$", dir.path(), true).join();
        assert_eq!(exit.code, None);
        assert_eq!(exit.signal, Some(libc::SIGSEGV));
        assert_eq!(exit.signal_name(), Some("SIGSEGV"));
        assert_eq!(exit.shell_code(), 128 + libc::SIGSEGV.unsigned_abs());
        assert_eq!(exit.describe(), "terminated by SIGSEGV");
    }

    #[test]
    fn exit_info_from_exit_status() {
        let status = Command::new("sh").args(["-c", "exit 7"]).status().unwrap();
        let exit = ExitInfo::from(status);
        assert_eq!((exit.code, exit.signal), (Some(7), None));
        assert!(!exit.success());
    }

    #[test]
    fn stop_escalates_to_sigkill() {
        let dir = tempfile::tempdir().unwrap();
        let spawned = spawn(
            "trap '' INT HUP TERM; touch ready; sleep 30",
            dir.path(),
            true,
        );
        assert!(wait_until(TIMEOUT, || dir.path().join("ready").exists()));

        let sent = spawned
            .handle
            .stop(StopSignal::Interrupt, Duration::from_millis(300))
            .unwrap();
        assert!(sent);

        let exit = spawned.handle.wait_timeout(Duration::from_secs(3));
        let exit = exit.expect("process survived SIGKILL escalation");
        assert_eq!(exit.signal, Some(libc::SIGKILL));
        assert!(exit.stop_requested);
        assert_eq!(exit.describe(), "stopped");
    }

    #[test]
    fn signal_after_reap_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let spawned = spawn("true", dir.path(), true);
        let handle = spawned.handle.clone();
        let exit = spawned.join();
        assert!(exit.success());
        assert!(handle.is_reaped());

        assert!(!handle.signal(StopSignal::Interrupt).unwrap());
        assert!(!handle.stop(StopSignal::Interrupt, Duration::ZERO).unwrap());
        assert!(!handle.force_kill().unwrap());
    }

    #[test]
    fn single_scope_signals_only_the_pid() {
        let dir = tempfile::tempdir().unwrap();
        // Stays in the test's process group, so a killpg on its pid would find no group
        let spawned = spawn("touch ready; exec sleep 30", dir.path(), false);
        assert_eq!(spawned.handle.scope(), SignalScope::Process);
        assert!(wait_until(TIMEOUT, || dir.path().join("ready").exists()));

        let sent = spawned
            .handle
            .stop(StopSignal::Terminate, Duration::from_secs(30))
            .unwrap();
        assert!(sent, "signal was not delivered to the pid");

        let exit = spawned.handle.wait_timeout(Duration::from_secs(3));
        assert_eq!(exit.and_then(|e| e.signal), Some(libc::SIGTERM));
    }
}
