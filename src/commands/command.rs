use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::commands::auto::Auto;

/// A single executable task with its configuration and automation rules
#[derive(Debug, Clone, Default)]
pub struct Command {
    pub id: String,
    pub name: String,
    pub cmd: String,
    pub cwd: PathBuf,
    pub auto: Auto,
    pub env: HashMap<String, String>,
    pub depends_on: Vec<String>,
    pub scrollback: Option<usize>,
    /// How long a headless run lets the command run before killing it. `Some(Duration::ZERO)`
    /// means no limit, even when the run has a default timeout.
    pub timeout: Option<Duration>,
    /// Whether a headless run keeps other commands from running alongside it; see
    /// [`is_exclusive`](Self::is_exclusive).
    pub exclusive: Option<bool>,
}

impl Command {
    /// Whether a headless run starts the command only when nothing else runs, and starts
    /// nothing else until it ends.
    #[must_use]
    pub fn is_exclusive(&self) -> bool {
        self.exclusive == Some(true)
    }

    /// Returns the effective working directory for this command,
    /// falling back to the given path when `cwd` is empty.
    #[must_use]
    pub fn effective_cwd<'a>(&'a self, fallback: &'a Path) -> &'a Path {
        if self.cwd.as_os_str().is_empty() {
            fallback
        } else {
            &self.cwd
        }
    }
}
