use std::path::Path;

use thiserror::Error;

#[derive(Error, Debug)]
pub enum HookError {
    #[error("git repository not found: {0}")]
    NoRepo(#[from] git2::Error),

    #[error("failed to write hook: {0}")]
    Io(#[from] std::io::Error),
}

const HOOK_MARKER: &str = "# fnug";

fn fnug_lines(no_workspace: bool) -> String {
    let no_ws = if no_workspace { " --no-workspace" } else { "" };
    format!("{HOOK_MARKER}\nfnug check --fail-fast --mute-success{no_ws}")
}

/// Check if a fnug pre-commit hook is installed in the given repo.
#[must_use]
pub fn is_installed(repo_path: &Path) -> bool {
    let hook_path = repo_path.join(".git/hooks/pre-commit");
    std::fs::read_to_string(hook_path).is_ok_and(|content| content.contains(HOOK_MARKER))
}

/// Install a git pre-commit hook that runs `fnug check`.
///
/// If a pre-commit hook already exists, fnug lines are appended (or replaced
/// if already present). If no hook exists, a new one is created.
///
/// # Errors
///
/// Returns `HookError` if the git repo can't be found or the hook can't be written.
pub fn install(repo_path: &Path, no_workspace: bool) -> Result<(), HookError> {
    let repo = git2::Repository::discover(repo_path)?;
    let hooks_dir = repo.path().join("hooks");
    let hook_path = hooks_dir.join("pre-commit");

    std::fs::create_dir_all(&hooks_dir)?;

    let new_fnug = fnug_lines(no_workspace);

    let content = if hook_path.exists() {
        let existing = std::fs::read_to_string(&hook_path)?;
        if existing.contains(HOOK_MARKER) {
            // Replace existing fnug block
            let replaced = strip_fnug_lines(&existing);
            format!("{replaced}\n{new_fnug}\n")
        } else {
            // Append to existing hook
            let trimmed = existing.trim_end();
            format!("{trimmed}\n\n{new_fnug}\n")
        }
    } else {
        format!("#!/bin/sh\n{new_fnug}\n")
    };

    std::fs::write(&hook_path, content)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook_path, std::fs::Permissions::from_mode(0o755))?;
    }

    Ok(())
}

/// Remove the fnug pre-commit hook.
///
/// If the hook file only contains fnug content, it is deleted entirely.
/// If mixed with other content, only fnug lines are removed.
///
/// # Errors
///
/// Returns `HookError` on IO failures.
pub fn remove(repo_path: &Path) -> Result<(), HookError> {
    let repo = git2::Repository::discover(repo_path)?;
    let hook_path = repo.path().join("hooks/pre-commit");

    if !hook_path.exists() {
        return Ok(());
    }

    let content = std::fs::read_to_string(&hook_path)?;
    if !content.contains(HOOK_MARKER) {
        return Ok(());
    }

    let remaining = strip_fnug_lines(&content);

    // If only the shebang (or nothing meaningful) remains, delete the file
    let has_content = remaining
        .lines()
        .any(|l| !l.trim().is_empty() && !l.starts_with("#!"));

    if has_content {
        std::fs::write(&hook_path, format!("{}\n", remaining.trim_end()))?;
    } else {
        std::fs::remove_file(&hook_path)?;
    }

    Ok(())
}

/// Remove fnug marker and the command line following it from hook content.
fn strip_fnug_lines(content: &str) -> String {
    let lines: Vec<&str> = content
        .lines()
        .scan(false, |skip_next, line| {
            if std::mem::take(skip_next) {
                return Some(None);
            }
            if line.contains(HOOK_MARKER) {
                *skip_next = true;
                return Some(None);
            }
            Some(Some(line))
        })
        .flatten()
        .collect();
    lines.join("\n")
}
