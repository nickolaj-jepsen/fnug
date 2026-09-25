//! Matching a changed file against a command's `auto.path` and `auto.regex` rules, shared by
//! the git selector and the watcher.

use std::path::{Path, PathBuf};

use crate::commands::command::Command;

/// `path` relative to `base`, climbing out of `base` with `..` where needed. Both must be
/// absolute and free of `.` and `..` components, as canonical paths are.
#[must_use]
pub fn relative_to(path: &Path, base: &Path) -> PathBuf {
    let mut path_parts = path.components().peekable();
    let mut base_parts = base.components().peekable();
    while path_parts.peek().is_some() && path_parts.peek() == base_parts.peek() {
        path_parts.next();
        base_parts.next();
    }
    base_parts
        .map(|_| Path::new(".."))
        .chain(path_parts.map(|part| Path::new(part.as_os_str())))
        .collect()
}

/// The string `auto.regex` patterns are matched against: `path` relative to the command's
/// `cwd`, such as `src/main.rs` or `../shared/lib.rs`. An empty `cwd` leaves `path` as is.
#[must_use]
pub fn match_subject(path: &Path, cwd: &Path) -> String {
    if cwd.as_os_str().is_empty() {
        return path.to_string_lossy().into_owned();
    }
    relative_to(path, cwd).to_string_lossy().into_owned()
}

/// Whether a changed `file` selects `cmd` through `prefix`, one of its `auto.path` entries:
/// `file` must be `prefix` or under it, and match one of the command's regexes if it has any.
pub(crate) fn command_matches(cmd: &Command, prefix: &Path, file: &Path) -> bool {
    if !file.starts_with(prefix) {
        return false;
    }
    let regexes = cmd.auto.regexes();
    if regexes.is_empty() {
        return true;
    }
    let subject = match_subject(file, &cmd.cwd);
    regexes.iter().any(|pattern| pattern.is_match(&subject))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_to_descends_and_climbs() {
        let base = Path::new("/repo/app");
        assert_eq!(
            relative_to(Path::new("/repo/app/src/main.rs"), base),
            Path::new("src/main.rs")
        );
        assert_eq!(
            relative_to(Path::new("/repo/shared/lib.rs"), base),
            Path::new("../shared/lib.rs")
        );
        assert_eq!(
            relative_to(Path::new("/other/x"), base),
            Path::new("../../other/x")
        );
        assert_eq!(relative_to(base, base), Path::new(""));
    }

    #[test]
    fn relative_to_compares_whole_components() {
        assert_eq!(
            relative_to(Path::new("/repo/app2/x"), Path::new("/repo/app")),
            Path::new("../app2/x")
        );
    }

    #[test]
    fn match_subject_without_cwd_is_the_path() {
        assert_eq!(
            match_subject(Path::new("/repo/src/a.rs"), Path::new("")),
            "/repo/src/a.rs"
        );
        assert_eq!(
            match_subject(Path::new("/repo/src/a.rs"), Path::new("/repo")),
            "src/a.rs"
        );
    }
}
