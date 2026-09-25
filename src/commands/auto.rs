use crate::config_file::{ConfigError, parse_regexes};
use regex_cache::LazyRegex;
use std::path::PathBuf;

/// Automation rules that determine when commands should execute
#[derive(Default, Debug, Clone)]
pub struct Auto {
    pub watch: Option<bool>,
    pub git: Option<bool>,
    pub path: Vec<PathBuf>,
    pub regex: Vec<LazyRegex>,
    pub always: Option<bool>,
    pub check: Option<bool>,
}

impl Auto {
    /// Path prefixes a changed file must fall under.
    #[must_use]
    pub fn paths(&self) -> &[PathBuf] {
        &self.path
    }

    /// Patterns a changed file must match; empty means any file matches.
    #[must_use]
    pub fn regexes(&self) -> &[LazyRegex] {
        &self.regex
    }

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
        Ok(Auto {
            watch,
            git,
            path,
            always,
            regex: parse_regexes(regex)?,
            check,
        })
    }
}
