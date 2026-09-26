//! Starting a command's shell, and making sure it doesn't outlive its run.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Child;

use log::warn;

use crate::commands::command::Command;
use crate::process::ProcessHandle;

/// How to start a command: `program` with `args` in `cwd`, with `env` added to fnug's own
/// environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellInvocation {
    pub program: &'static str,
    pub args: [OsString; 2],
    pub cwd: PathBuf,
    /// Sorted by name.
    pub env: Vec<(OsString, OsString)>,
}

impl ShellInvocation {
    /// A process builder for the invocation.
    #[must_use]
    pub fn command(&self) -> std::process::Command {
        let mut command = std::process::Command::new(self.program);
        command
            .args(&self.args)
            .current_dir(&self.cwd)
            .envs(self.env.iter().map(|(k, v)| (k, v)));
        command
    }
}

/// Run `cmd.cmd` with `sh -c` in the command's cwd, or in `fallback_cwd` when it has none.
#[must_use]
pub fn shell_invocation(cmd: &Command, fallback_cwd: &Path) -> ShellInvocation {
    let mut env: Vec<(OsString, OsString)> =
        cmd.env.iter().map(|(k, v)| (k.into(), v.into())).collect();
    env.sort();
    ShellInvocation {
        program: "sh",
        args: ["-c".into(), cmd.cmd.clone().into()],
        cwd: cmd.effective_cwd(fallback_cwd).to_path_buf(),
        env,
    }
}

/// Kills a child (its process group, for a group handle) when dropped, unless disarmed, and
/// reaps it if it holds the exited [`Child`]. Covers panics, dropped futures and runtime
/// shutdown.
pub(crate) struct GroupGuard {
    handle: ProcessHandle,
    child: Option<Child>,
    armed: bool,
}

impl GroupGuard {
    pub(crate) fn new(handle: ProcessHandle) -> Self {
        Self {
            handle,
            child: None,
            armed: true,
        }
    }

    pub(crate) fn handle(&self) -> &ProcessHandle {
        &self.handle
    }

    /// Hand over the child once [`ProcessHandle::wait_exit`] has seen it exit.
    pub(crate) fn set_exited(&mut self, child: Child) {
        self.child = Some(child);
    }

    /// Reap the exited child without killing anything.
    pub(crate) fn reap(mut self) {
        self.armed = false;
        self.release();
    }

    fn release(&mut self) {
        if let Some(mut child) = self.child.take()
            && let Err(e) = self.handle.reap_with(|| child.wait())
        {
            warn!("Failed to reap pid {}: {e}", self.handle.pid());
        }
    }
}

impl Drop for GroupGuard {
    fn drop(&mut self) {
        if self.armed
            && let Err(e) = self.handle.force_kill()
        {
            warn!("Failed to kill pid {}: {e}", self.handle.pid());
        }
        self.release();
    }
}
