use crate::commands::auto::Auto;
use std::collections::HashMap;
use std::path::PathBuf;

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
