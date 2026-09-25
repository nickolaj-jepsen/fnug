use std::path::{Component, Path, PathBuf};

use git2::{Repository, RepositoryOpenFlags};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum HookError {
    #[error("git repository not found: {0}")]
    NoRepo(#[from] git2::Error),

    #[error("{} is a bare repository, so there is nothing to check before a commit", path.display())]
    BareRepo { path: PathBuf },

    #[error(
        "husky manages this repository's git hooks, so fnug won't install its own. Add this to {}:\n\n{snippet}",
        user_hook.display()
    )]
    Husky { user_hook: PathBuf, snippet: String },

    #[error(
        "core.hooksPath points outside this repository, to {}, which other repositories may share, so fnug won't write there. To run fnug for this repository, add this to that pre-commit hook:\n\n{snippet}",
        path.display()
    )]
    GlobalHooksPath { path: PathBuf, snippet: String },

    #[error("failed to write hook: {0}")]
    Io(#[from] std::io::Error),
}

/// Where a repository's pre-commit hook lives, as git resolves it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookTarget {
    /// Top of the work tree, where git runs hooks.
    pub workdir: PathBuf,
    /// The directory git reads hooks from.
    pub hooks_dir: PathBuf,
    /// The file fnug edits: `hooks_dir/pre-commit`, or husky's user hook.
    pub hook_path: PathBuf,
    pub location: HookLocation,
    /// The config's directory relative to `workdir`; empty when the config is at the top.
    pub config_rel: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookLocation {
    /// The repository's own hooks directory, inside its git dir.
    Local,
    /// A `core.hooksPath` inside the work tree, usually committed and shared with every clone.
    Shared,
    /// A husky-managed `core.hooksPath`; its hooks run `user_hook`.
    Husky { user_hook: PathBuf },
    /// A `core.hooksPath` outside the work tree, such as a dispatcher set in the global config.
    External,
}

const HOOK_MARKER: &str = "# fnug";

/// Arguments the pre-commit hook passes to `fnug`.
///
/// Global flags come before the subcommand so the hook also parses with
/// fnug versions where they weren't global yet.
#[must_use]
pub fn hook_args(no_workspace: bool) -> Vec<&'static str> {
    let mut args = Vec::with_capacity(4);
    if no_workspace {
        args.push("--no-workspace");
    }
    args.extend(["check", "--fail-fast", "--mute-success"]);
    args
}

fn fnug_lines(no_workspace: bool) -> String {
    let args = hook_args(no_workspace).join(" ");
    format!("{HOOK_MARKER}\nfnug {args}")
}

/// Resolve the pre-commit hook of the repository containing `config_dir` the way git does:
/// `core.hooksPath` (relative to the work tree top) if set, otherwise the hooks directory in the
/// common git dir, which linked worktrees share.
///
/// # Errors
///
/// Returns `HookError::NoRepo` if `config_dir` isn't in a git repository or its config can't be
/// read, and `HookError::BareRepo` if the repository has no work tree.
pub fn resolve(config_dir: &Path) -> Result<HookTarget, HookError> {
    // Not `Repository::discover`: it reopens the gitdir, so a `.git` file without
    // `core.worktree` (as `git init --separate-git-dir` writes) gets the wrong work tree.
    let repo = Repository::open_ext(config_dir, RepositoryOpenFlags::CROSS_FS, &[] as &[&Path])?;
    let Some(workdir) = repo.workdir() else {
        return Err(HookError::BareRepo {
            path: repo.path().to_path_buf(),
        });
    };
    let workdir = normalize(workdir);
    let git_dir = normalize(repo.path());
    let common_dir = normalize(repo.commondir());
    let config_rel = normalize(config_dir)
        .strip_prefix(&workdir)
        .map(Path::to_path_buf)
        .unwrap_or_default();

    let hooks_dir = match repo.config()?.get_path("core.hooksPath") {
        Ok(path) => normalize(&workdir.join(path)),
        Err(e) if e.code() == git2::ErrorCode::NotFound => common_dir.join("hooks"),
        Err(e) => return Err(e.into()),
    };
    let location = if hooks_dir.starts_with(&common_dir) || hooks_dir.starts_with(&git_dir) {
        HookLocation::Local
    } else if let Ok(rel) = hooks_dir.strip_prefix(&workdir) {
        // husky 9 sets `.husky/_`, husky 5 to 8 `.husky`
        if rel == Path::new(".husky/_") || rel == Path::new(".husky") {
            HookLocation::Husky {
                user_hook: workdir.join(".husky/pre-commit"),
            }
        } else {
            HookLocation::Shared
        }
    } else {
        HookLocation::External
    };
    let hook_path = match &location {
        HookLocation::Husky { user_hook } => user_hook.clone(),
        _ => hooks_dir.join("pre-commit"),
    };
    Ok(HookTarget {
        workdir,
        hooks_dir,
        hook_path,
        location,
        config_rel,
    })
}

/// `path` made absolute with `.` and `..` resolved, and its longest existing ancestor
/// canonicalized, so it compares equal to the paths git reports.
fn normalize(path: &Path) -> PathBuf {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut lexical = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                lexical.pop();
            }
            other => lexical.push(other),
        }
    }
    let mut missing = Vec::new();
    let mut existing = lexical.as_path();
    loop {
        if let Ok(mut canonical) = existing.canonicalize() {
            canonical.extend(missing.iter().rev());
            return canonical;
        }
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(name)) => {
                missing.push(name);
                existing = parent;
            }
            _ => return lexical,
        }
    }
}

/// Check if a fnug pre-commit hook is installed for the repository containing `config_dir`.
#[must_use]
pub fn is_installed(config_dir: &Path) -> bool {
    resolve(config_dir).is_ok_and(|target| {
        std::fs::read_to_string(target.hook_path).is_ok_and(|content| content.contains(HOOK_MARKER))
    })
}

/// Install a git pre-commit hook that runs `fnug check`.
///
/// If a pre-commit hook already exists, fnug lines are appended (or replaced
/// if already present). If no hook exists, a new one is created.
///
/// # Errors
///
/// Returns the errors of [`resolve`], `HookError::Husky` or `HookError::GlobalHooksPath` if
/// another tool owns the hooks directory, and `HookError::Io` if the hook can't be written.
pub fn install(config_dir: &Path, no_workspace: bool) -> Result<(), HookError> {
    let target = resolve(config_dir)?;
    let new_fnug = fnug_lines(no_workspace);
    match target.location {
        HookLocation::Husky { user_hook } => {
            return Err(HookError::Husky {
                user_hook,
                snippet: new_fnug,
            });
        }
        HookLocation::External => {
            return Err(HookError::GlobalHooksPath {
                path: target.hooks_dir,
                snippet: new_fnug,
            });
        }
        HookLocation::Local | HookLocation::Shared => {}
    }
    let hook_path = target.hook_path;
    std::fs::create_dir_all(&target.hooks_dir)?;

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
/// Returns the errors of [`resolve`], and `HookError::Io` on IO failures.
pub fn remove(config_dir: &Path) -> Result<(), HookError> {
    let hook_path = resolve(config_dir)?.hook_path;

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
