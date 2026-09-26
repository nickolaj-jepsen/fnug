//! Why a command is selected, shown in its empty terminal pane.

use std::fmt::Write;
use std::path::{Path, PathBuf};

use crate::selectors::watch::WatchMatch;

/// How many changed files a line names before it says how many more there are
const NAMED_FILES: usize = 1;

/// What selected a command
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectionReason {
    /// `auto.always`
    Always,
    /// Uncommitted changes to these files
    Git(Vec<PathBuf>),
    /// These files changed while fnug watched
    Watch(Vec<PathBuf>),
    /// It didn't pass in the `fnug check` that opened the TUI
    CheckFailed,
    /// The user selected it
    Manual,
}

impl SelectionReason {
    /// One line on why the command is selected, naming files relative to `cwd`.
    #[must_use]
    pub fn describe(&self, cwd: &Path) -> String {
        match self {
            SelectionReason::Always => "Always selected".into(),
            SelectionReason::Git(files) if files.is_empty() => "Selected by git changes".into(),
            SelectionReason::Git(files) => format!("Selected by git: {}", list_files(files, cwd)),
            SelectionReason::Watch(files) if files.is_empty() => "Selected by a file change".into(),
            SelectionReason::Watch(files) => {
                format!("Selected by a change to {}", list_files(files, cwd))
            }
            SelectionReason::CheckFailed => "Failed in fnug check".into(),
            SelectionReason::Manual => "Selected by you".into(),
        }
    }
}

/// `path` relative to `cwd` when it is inside it
fn relative(path: &Path, cwd: &Path) -> String {
    path.strip_prefix(cwd).unwrap_or(path).display().to_string()
}

/// "src/lib.rs, +2 more"
fn list_files(files: &[PathBuf], cwd: &Path) -> String {
    let mut text = files
        .iter()
        .take(NAMED_FILES)
        .map(|f| relative(f, cwd))
        .collect::<Vec<_>>()
        .join(", ");
    if files.len() > NAMED_FILES {
        let _ = write!(text, ", +{} more", files.len() - NAMED_FILES);
    }
    text
}

/// The toolbar message for a watcher event that selected `names`, the matched commands'
/// names in order.
#[must_use]
pub fn watch_status(matches: &[WatchMatch], names: &[String], cwd: &Path) -> String {
    let file = matches
        .iter()
        .flat_map(|m| &m.files)
        .next()
        .map_or_else(|| "Files".to_string(), |f| relative(f, cwd));
    let mut selected = names.iter().take(2).cloned().collect::<Vec<_>>().join(", ");
    if names.len() > 2 {
        let _ = write!(selected, " and {} more", names.len() - 2);
    }
    format!("{file} changed: selected {selected}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describe_names_files_relative_to_cwd() {
        let cwd = Path::new("/repo");
        let git = SelectionReason::Git(vec![
            "/repo/src/lib.rs".into(),
            "/repo/src/main.rs".into(),
            "/elsewhere/x.rs".into(),
        ]);
        assert_eq!(git.describe(cwd), "Selected by git: src/lib.rs, +2 more");
        assert_eq!(
            SelectionReason::Watch(vec!["/elsewhere/x.rs".into()]).describe(cwd),
            "Selected by a change to /elsewhere/x.rs"
        );
        assert_eq!(
            SelectionReason::Git(vec![]).describe(cwd),
            "Selected by git changes"
        );
    }

    #[test]
    fn watch_status_names_file_and_commands() {
        let cwd = Path::new("/repo");
        let matches = [WatchMatch {
            id: "lint".into(),
            files: vec!["/repo/src/lib.rs".into()],
        }];
        let names = ["lint", "test", "fmt"].map(String::from);
        assert_eq!(
            watch_status(&matches, &names, cwd),
            "src/lib.rs changed: selected lint, test and 1 more"
        );
        assert_eq!(
            watch_status(&[], &names[..1], cwd),
            "Files changed: selected lint"
        );
    }
}
