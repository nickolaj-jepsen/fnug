//! The tools' parameters.

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct ListLintsParams {
    /// Filter by group name (case-insensitive substring match). Groups organize
    /// commands hierarchically, e.g. "tests", "lints".
    #[schemars(default)]
    pub group: Option<String>,
    /// Filter by auto-selection type: "git" (selected by changed files), "watch"
    /// (selected by file watcher), "always" (always runs), or "none" (manual only).
    #[schemars(default)]
    pub auto_type: Option<AutoType>,
    /// Filter by command name or id (case-insensitive substring match).
    #[schemars(default)]
    pub name: Option<String>,
}

/// An `auto` rule that selects commands. Variants carry no docs, so the schema lists the values
/// as a plain `enum`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub(super) enum AutoType {
    Git,
    Watch,
    Always,
    None,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct FailFastParams {
    /// Stop on first failure instead of running all commands. Useful for quick
    /// feedback when you expect failures.
    #[schemars(default)]
    pub fail_fast: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct RunLintParams {
    /// The command id or name to run. Use `list_lints` to discover available
    /// commands. Matches an exact id, or else a unique case-insensitive name.
    pub command: String,
}
