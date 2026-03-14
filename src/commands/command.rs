use std::collections::HashMap;
use std::path::{Path, PathBuf};

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
}

impl Command {
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
