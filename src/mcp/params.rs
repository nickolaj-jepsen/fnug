//! The tools' parameters.

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct ListLintsParams {
    /// Filter by group name (case-insensitive substring match). Groups organize
    /// commands hierarchically, e.g. "tests", "lints".
    #[schemars(default)]
    pub group: Option<String>,
    /// Filter by auto-selection type: "git" (selected by changed files), "watch"
    /// (selected by file watcher), "always" (always runs), or "none" (manual only).
    #[schemars(default)]
    pub auto_type: Option<String>,
    /// Filter by command name or id (case-insensitive substring match).
    #[schemars(default)]
    pub name: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct FailFastParams {
    /// Stop on first failure instead of running all commands. Useful for quick
    /// feedback when you expect failures.
    #[schemars(default)]
    pub fail_fast: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct RunLintParams {
    /// The command name or id to run. Use `list_lints` to discover available
    /// commands. Matches by exact id or case-insensitive name.
    pub command: String,
}
