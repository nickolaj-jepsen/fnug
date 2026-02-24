use std::path::{Path, PathBuf};

use thiserror::Error;

#[derive(Error, Debug)]
pub enum InitHooksError {
    #[error("git repository not found: {0}")]
    NoRepo(#[from] git2::Error),

    #[error("pre-commit hook already exists at {0} (use --force to overwrite)")]
    HookExists(PathBuf),

    #[error("failed to write hook: {0}")]
    Io(#[from] std::io::Error),
}

const HOOK_CONTENTS: &str = "\
#!/bin/sh
# Installed by fnug init-hooks
fnug check --fail-fast --mute-success
";

/// Install a git pre-commit hook that runs `fnug check`.
///
/// # Errors
///
/// Returns `InitHooksError::NoRepo` if no git repository is found,
/// `InitHooksError::HookExists` if a hook already exists (unless `force` is set),
/// or `InitHooksError::Io` on write failure.
pub fn run(cwd: &Path, force: bool) -> Result<(), InitHooksError> {
    let repo = git2::Repository::discover(cwd)?;
    let git_dir = repo.path(); // .git/
    let hooks_dir = git_dir.join("hooks");
    let hook_path = hooks_dir.join("pre-commit");

    if hook_path.exists() && !force {
        return Err(InitHooksError::HookExists(hook_path));
    }

    std::fs::create_dir_all(&hooks_dir)?;
    std::fs::write(&hook_path, HOOK_CONTENTS)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(&hook_path, perms)?;
    }

    println!("Installed pre-commit hook at {}", hook_path.display());
    Ok(())
}
