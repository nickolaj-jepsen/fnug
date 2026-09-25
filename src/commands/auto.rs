use crate::config_file::{ConfigError, parse_regexes};
use regex_cache::LazyRegex;
use std::path::PathBuf;

/// Automation rules that determine when commands should execute
///
/// `None` fields are unset and inherit from the parent group. For `path` and `regex`, an empty
/// `Some` is set: it stops inheritance, meaning "the node's cwd" and "any file" respectively.
#[derive(Default, Debug, Clone)]
pub struct Auto {
    pub watch: Option<bool>,
    pub git: Option<bool>,
    pub path: Option<Vec<PathBuf>>,
    pub regex: Option<Vec<LazyRegex>>,
    pub always: Option<bool>,
    pub check: Option<bool>,
}

impl Auto {
    /// Path prefixes a changed file must fall under.
    #[must_use]
    pub fn paths(&self) -> &[PathBuf] {
        self.path.as_deref().unwrap_or_default()
    }

    /// Patterns a changed file must match; empty means any file matches.
    #[must_use]
    pub fn regexes(&self) -> &[LazyRegex] {
        self.regex.as_deref().unwrap_or_default()
    }

    /// Build rules from plain values. An empty `path` or `regex` leaves that field unset.
    ///
    /// # Errors
    ///
    /// Returns `ConfigError::Regex` if any regex pattern is invalid.
    pub fn create(
        watch: Option<bool>,
        git: Option<bool>,
        path: Vec<PathBuf>,
        regex: Vec<String>,
        always: Option<bool>,
        check: Option<bool>,
    ) -> Result<Self, ConfigError> {
        let regex = if regex.is_empty() {
            None
        } else {
            Some(parse_regexes(regex)?)
        };
        Ok(Auto {
            watch,
            git,
            path: (!path.is_empty()).then_some(path),
            always,
            regex,
            check,
        })
    }
}
