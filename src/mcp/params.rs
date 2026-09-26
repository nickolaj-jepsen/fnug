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
    /// Report `selected` by what changed since the merge base of HEAD and this git revision,
    /// as `run_lints` does with the same parameter.
    #[schemars(default)]
    pub base: Option<String>,
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

#[derive(Debug, Default, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct RunLintsParams {
    /// Stop on first failure instead of running all commands. Useful for quick
    /// feedback when you expect failures.
    #[schemars(default)]
    pub fail_fast: Option<bool>,
    /// Select by what changed since the merge base of HEAD and this git revision, such as
    /// "origin/main": the branch's commits plus uncommitted changes. Without it, only
    /// uncommitted changes count (staged, unstaged and untracked files).
    #[schemars(default)]
    pub base: Option<String>,
    /// Also run selected commands with `auto.check: false`, which are skipped by default.
    #[schemars(default)]
    pub include_manual: Option<bool>,
    /// Also return the output of commands that passed or were cancelled. Output stays capped.
    #[schemars(default)]
    pub verbose: Option<bool>,
}

#[derive(Debug, Default, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct RunAllParams {
    /// Stop on first failure instead of running all commands. Useful for quick
    /// feedback when you expect failures.
    #[schemars(default)]
    pub fail_fast: Option<bool>,
    /// Also run commands with `auto.check: false`, which are skipped by default.
    #[schemars(default)]
    pub include_manual: Option<bool>,
    /// Also return the output of commands that passed or were cancelled. Output stays capped.
    #[schemars(default)]
    pub verbose: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct RunLintParams {
    /// The command id or name to run. Use `list_lints` to discover available
    /// commands. Matches an exact id, or else a unique case-insensitive name.
    pub command: String,
    /// Also return the output of commands that passed or were cancelled. Output stays capped.
    #[schemars(default)]
    pub verbose: Option<bool>,
}
