//! The git pre-commit hook that runs `fnug check`.
//!
//! fnug owns only the lines between its `# >>> fnug >>>` and `# <<< fnug <<<` fences. The block
//! goes right after the shebang, so an `exec` or `exit` later in the hook can't skip it, and it
//! ends in `|| exit $?`, so it never hides the status of the lines after it.

use std::io;
use std::ops::Range;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};

use git2::{Repository, RepositoryOpenFlags};
use thiserror::Error;

use super::fsutil::write_atomic;

/// Version of the hook block's format. Bump it when the block changes, so [`status`] reports
/// older blocks as [`HookStatus::Outdated`] and setup offers to update them.
pub const HOOK_FORMAT_VERSION: u32 = 1;

const BEGIN: &str = "# >>> fnug >>>";
const END: &str = "# <<< fnug <<<";
const VERSION_PREFIX: &str = "# fnug-hook-version: ";
/// Where a chained hook's original is kept, next to the wrapper that replaces it.
const CHAINED_NAME: &str = "pre-commit.local";
const SHELLS: &[&str] = &["sh", "bash", "dash", "zsh", "ksh", "ash"];

#[derive(Error, Debug)]
pub enum HookError {
    #[error("git repository not found: {0}")]
    NoRepo(#[from] git2::Error),

    #[error("{} is a bare repository, so there is nothing to check before a commit", path.display())]
    BareRepo { path: PathBuf },

    #[error(
        "{} is a {interpreter} script, so fnug can't add itself to it. Run this from the hook yourself, or let `fnug setup` chain the hooks (it renames yours to {CHAINED_NAME} and runs it after fnug):\n\n{snippet}",
        path.display()
    )]
    ForeignHook {
        path: PathBuf,
        interpreter: String,
        snippet: String,
    },

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
    Io(#[from] io::Error),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookStatus {
    /// No fnug block, and no hook or a shell hook that one can be added to.
    NotInstalled,
    /// A block in the current format.
    Installed,
    /// A block written by an older fnug, which installing updates. In a hook that isn't a shell
    /// script, installing goes by [`ForeignPolicy`].
    Outdated,
    /// No fnug block, in a hook that isn't a shell script; see [`ForeignPolicy`].
    Foreign,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ForeignPolicy {
    /// Fail with [`HookError::ForeignHook`].
    #[default]
    Refuse,
    /// Rename the hook to `pre-commit.local`, dropping a legacy fnug block from it, and replace
    /// it with an sh hook that runs fnug and then the original. Removing fnug's hook puts the
    /// original back.
    Chain,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InstallOptions {
    /// Pass `--no-workspace`, for a repository inside another fnug workspace.
    pub no_workspace: bool,
    /// Pass this as `-c`. It is relative to the config's directory, which the hook runs from.
    pub config_file: Option<PathBuf>,
    /// Pass this as `--root`. It is relative to the config's directory.
    pub root_dir: Option<PathBuf>,
    /// What to do with an existing hook that isn't a shell script.
    pub foreign: ForeignPolicy,
    /// fnug binary to run when `fnug` isn't on `PATH`, e.g. [`std::env::current_exe`]. Only
    /// [`HookLocation::Local`] hooks get it: a shared hook must not hold a machine's path.
    pub fallback_exe: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallOutcome {
    Created,
    Updated,
    /// The existing hook was moved to `original` and runs after fnug.
    Chained {
        original: PathBuf,
    },
}

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

/// The hook's arguments to `fnug` for `opts`, as shell words.
fn command_args(opts: &InstallOptions) -> Vec<String> {
    let mut args = Vec::new();
    for (flag, value) in [("-c", &opts.config_file), ("--root", &opts.root_dir)] {
        if let Some(value) = value {
            args.extend([flag.to_string(), sh_quote(&value.to_string_lossy())]);
        }
    }
    args.extend(hook_args(opts.no_workspace).into_iter().map(str::to_string));
    args
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

/// Whether `target`'s hook runs fnug.
#[must_use]
pub fn status(target: &HookTarget) -> HookStatus {
    let Ok(bytes) = std::fs::read(&target.hook_path) else {
        return HookStatus::NotInstalled;
    };
    let Ok(content) = String::from_utf8(bytes) else {
        return HookStatus::Foreign;
    };
    let lines: Vec<&str> = content.split_inclusive('\n').collect();
    if let Some(block) = find_block(&lines) {
        let current = format!("{VERSION_PREFIX}{HOOK_FORMAT_VERSION}");
        return if lines[block].iter().any(|l| l.trim_end() == current) {
            HookStatus::Installed
        } else {
            HookStatus::Outdated
        };
    }
    if find_legacy(&lines).is_some() {
        return HookStatus::Outdated;
    }
    match classify(&content) {
        Script::Shell => HookStatus::NotInstalled,
        Script::Foreign(_) => HookStatus::Foreign,
    }
}

/// Like [`status`], but a block in the current format that differs from the one installing with
/// `opts` writes, say because the config moved, is [`HookStatus::Outdated`].
#[must_use]
pub fn status_with(target: &HookTarget, opts: &InstallOptions) -> HookStatus {
    let status = status(target);
    if status != HookStatus::Installed {
        return status;
    }
    let content = std::fs::read_to_string(&target.hook_path).unwrap_or_default();
    let lines: Vec<&str> = content.split_inclusive('\n').collect();
    let current = find_block(&lines)
        .map(|range| lines[range].concat())
        .unwrap_or_default();
    if current == block_for(target, opts, current.contains(CHAINED_NAME)) {
        HookStatus::Installed
    } else {
        HookStatus::Outdated
    }
}

/// Check if a fnug pre-commit hook, possibly an outdated one, is installed for the repository
/// containing `config_dir`.
#[must_use]
pub fn is_installed(config_dir: &Path) -> bool {
    resolve(config_dir).is_ok_and(|target| {
        matches!(
            status(&target),
            HookStatus::Installed | HookStatus::Outdated
        )
    })
}

/// Add fnug's block to `target`'s hook, or update it in place: a new hook is created, and an
/// existing shell hook gets the block after its shebang.
///
/// # Errors
///
/// Returns `HookError::Husky` or `HookError::GlobalHooksPath` if another tool owns the hooks
/// directory, `HookError::ForeignHook` for a hook that isn't a shell script under
/// [`ForeignPolicy::Refuse`], and `HookError::Io` if the hook can't be read or written. Each
/// refusal carries a snippet to add by hand.
pub fn install_with(
    target: &HookTarget,
    opts: &InstallOptions,
) -> Result<InstallOutcome, HookError> {
    let (plan, outcome) = plan_install(target, opts)?;
    plan.apply()?;
    Ok(outcome)
}

/// Install the hook for the repository containing `config_dir`, refusing hooks that aren't
/// shell scripts.
///
/// # Errors
///
/// Returns the errors of [`resolve`] and [`install_with`].
pub fn install(config_dir: &Path, no_workspace: bool) -> Result<(), HookError> {
    let opts = InstallOptions {
        no_workspace,
        ..InstallOptions::default()
    };
    install_with(&resolve(config_dir)?, &opts).map(drop)
}

/// Remove fnug's block from the hook of the repository containing `config_dir`. A hook left with
/// nothing but its shebang is deleted, and a chained original is put back.
///
/// # Errors
///
/// Returns the errors of [`resolve`], and `HookError::Io` on IO failures.
pub fn remove(config_dir: &Path) -> Result<(), HookError> {
    plan_remove(&resolve(config_dir)?)?.apply()
}

/// Changes to a hook, worked out by [`plan_install`] or [`plan_remove`] without making them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookPlan {
    steps: Vec<Step>,
    note: Option<String>,
}

impl HookPlan {
    /// Whether there is nothing to change.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    /// Something to tell the user before they apply the plan.
    #[must_use]
    pub fn note(&self) -> Option<&str> {
        self.note.as_deref()
    }

    /// Make the changes. If one fails, the ones before it are undone as far as possible.
    ///
    /// # Errors
    ///
    /// Returns `HookError::Io` if a file can't be written, renamed or removed.
    pub fn apply(&self) -> Result<(), HookError> {
        for (done, step) in self.steps.iter().enumerate() {
            if let Err(e) = step.run() {
                for undo in self.steps[..done].iter().rev().filter_map(Step::undo) {
                    let _ = undo.run();
                }
                return Err(e.into());
            }
        }
        Ok(())
    }
}

/// A filesystem change, worked out before any is made.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Step {
    Write {
        path: PathBuf,
        content: String,
        mode: Option<u32>,
        /// What to write back if a later step fails.
        previous: Option<String>,
    },
    Rename {
        from: PathBuf,
        to: PathBuf,
    },
    Remove(PathBuf),
}

impl Step {
    fn run(&self) -> io::Result<()> {
        match self {
            Self::Write {
                path,
                content,
                mode,
                ..
            } => write_atomic(path, content, *mode),
            Self::Rename { from, to } => std::fs::rename(from, to),
            Self::Remove(path) => std::fs::remove_file(path),
        }
    }

    /// The step that reverses this one, if it can be reversed.
    fn undo(&self) -> Option<Self> {
        match self {
            Self::Write {
                path,
                previous: Some(previous),
                ..
            } => Some(Self::Write {
                path: path.clone(),
                content: previous.clone(),
                mode: None,
                previous: None,
            }),
            Self::Rename { from, to } => Some(Self::Rename {
                from: to.clone(),
                to: from.clone(),
            }),
            Self::Write { .. } | Self::Remove(_) => None,
        }
    }
}

/// Work out what [`install_with`] would change, and what that amounts to, without changing it.
///
/// # Errors
///
/// Returns the errors of [`install_with`], except those from writing.
pub fn plan_install(
    target: &HookTarget,
    opts: &InstallOptions,
) -> Result<(HookPlan, InstallOutcome), HookError> {
    let (steps, outcome, note) = install_steps(target, opts)?;
    Ok((HookPlan { steps, note }, outcome))
}

/// Work out what [`remove`] would change in `target`'s hook, without changing it.
///
/// # Errors
///
/// Returns `HookError::Io` if the hook exists but can't be read.
pub fn plan_remove(target: &HookTarget) -> Result<HookPlan, HookError> {
    Ok(HookPlan {
        steps: remove_steps(target)?,
        note: None,
    })
}

/// Steps, what they amount to, and a note for the user.
type Planned = (Vec<Step>, InstallOutcome, Option<String>);

/// fnug's block for `target`'s hook, as installing with `opts` writes it.
fn block_for(target: &HookTarget, opts: &InstallOptions, chain: bool) -> String {
    let local = target.location == HookLocation::Local;
    render_block(&BlockSpec {
        args: &command_args(opts),
        config_rel: &target.config_rel,
        fallback_exe: opts.fallback_exe.as_deref().filter(|_| local),
        required: local,
        chain,
    })
}

fn install_steps(target: &HookTarget, opts: &InstallOptions) -> Result<Planned, HookError> {
    let args = command_args(opts);
    let block = |chain| block_for(target, opts, chain);
    match &target.location {
        HookLocation::Husky { user_hook } => {
            return Err(HookError::Husky {
                user_hook: user_hook.clone(),
                snippet: block(false),
            });
        }
        HookLocation::External => {
            return Err(HookError::GlobalHooksPath {
                path: target.hooks_dir.clone(),
                snippet: block(false),
            });
        }
        HookLocation::Local | HookLocation::Shared => {}
    }

    let path = &target.hook_path;
    let existing = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let content = format!("#!/bin/sh\n{}", block(false));
            return Ok((
                vec![write_step(path, content, 0o755)],
                InstallOutcome::Created,
                None,
            ));
        }
        Err(e) => return Err(e.into()),
    };
    // Git skips hooks that aren't executable
    let mode = std::fs::metadata(path)?.permissions().mode() | 0o111;
    let text = String::from_utf8(existing).ok();
    let interpreter = match &text {
        Some(text) => {
            let lines: Vec<&str> = text.split_inclusive('\n').collect();
            if let Some(range) = find_block(&lines) {
                let chained = lines[range].concat().contains(CHAINED_NAME);
                let content = splice(text, &block(chained));
                return Ok((
                    vec![write_step(path, content, mode)],
                    InstallOutcome::Updated,
                    None,
                ));
            }
            match classify(text) {
                Script::Shell => {
                    let content = splice(text, &block(false));
                    let note = sets_up_path(text).then(|| {
                        format!(
                            "{} sets PATH or sources files, and fnug now runs before that. If fnug needs it, move fnug's block below those lines; updates keep it there.",
                            path.display()
                        )
                    });
                    return Ok((
                        vec![write_step(path, content, mode)],
                        InstallOutcome::Updated,
                        note,
                    ));
                }
                // Even with a legacy block: the block is sh, which this interpreter may not run
                Script::Foreign(interpreter) => interpreter,
            }
        }
        None => "binary".to_string(),
    };
    match opts.foreign {
        ForeignPolicy::Refuse => Err(HookError::ForeignHook {
            path: path.clone(),
            interpreter,
            snippet: in_config_dir(&target.config_rel, &format!("fnug {}", args.join(" "))),
        }),
        ForeignPolicy::Chain => {
            chain_steps(path, text.as_deref(), format!("#!/bin/sh\n{}", block(true)))
        }
    }
}

/// Move the hook at `path`, whose content is `text` unless it isn't UTF-8, aside to be run by
/// `wrapper`, which replaces it. A legacy block is dropped from the original, since the wrapper
/// runs fnug.
fn chain_steps(path: &Path, text: Option<&str>, wrapper: String) -> Result<Planned, HookError> {
    let original = path.with_file_name(CHAINED_NAME);
    if original.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("can't chain hooks: {} already exists", original.display()),
        )
        .into());
    }
    let mut steps = vec![Step::Rename {
        from: path.to_path_buf(),
        to: original.clone(),
    }];
    if let Some(text) = text
        && let Some(stripped) = strip(text)
    {
        steps.push(Step::Write {
            path: original.clone(),
            content: stripped.rest,
            mode: None,
            previous: Some(text.to_string()),
        });
    }
    steps.push(write_step(path, wrapper, 0o755));
    Ok((steps, InstallOutcome::Chained { original }, None))
}

fn write_step(path: &Path, content: String, mode: u32) -> Step {
    Step::Write {
        path: path.to_path_buf(),
        content,
        mode: Some(mode),
        previous: None,
    }
}

/// Whether a hook changes `PATH` or sources files, which fnug's block runs before.
fn sets_up_path(content: &str) -> bool {
    content.lines().map(str::trim_start).any(|line| {
        !line.starts_with('#')
            && (line.contains("PATH=") || line.starts_with(". ") || line.starts_with("source "))
    })
}

fn remove_steps(target: &HookTarget) -> Result<Vec<Step>, HookError> {
    let path = &target.hook_path;
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::InvalidData
            ) =>
        {
            return Ok(Vec::new());
        }
        Err(e) => return Err(e.into()),
    };
    let Some(stripped) = strip(&content) else {
        return Ok(Vec::new());
    };
    let only_shebang = stripped
        .rest
        .lines()
        .all(|l| l.trim().is_empty() || l.starts_with("#!"));
    if !only_shebang {
        return Ok(vec![Step::Write {
            path: path.clone(),
            content: stripped.rest,
            mode: None,
            previous: None,
        }]);
    }
    let original = path.with_file_name(CHAINED_NAME);
    if stripped.chained && original.exists() {
        // Replaces the wrapper in one step, so there is always a hook
        return Ok(vec![Step::Rename {
            from: original,
            to: path.clone(),
        }]);
    }
    Ok(vec![Step::Remove(path.clone())])
}

struct BlockSpec<'a> {
    args: &'a [String],
    /// The config's directory relative to the work tree top, where git runs hooks. Relative,
    /// because linked worktrees share one hook.
    config_rel: &'a Path,
    /// Run this when `fnug` isn't on `PATH`.
    fallback_exe: Option<&'a Path>,
    /// Fail the commit when fnug is missing, instead of warning and going on. Only a hook no one
    /// else runs should block commits of people who don't have fnug.
    required: bool,
    /// Run `pre-commit.local` after fnug passes.
    chain: bool,
}

/// fnug's fenced block, ending in a newline.
fn render_block(spec: &BlockSpec) -> String {
    let find = match spec.fallback_exe {
        Some(exe) => format!(
            "fnug_bin=$(command -v fnug) || fnug_bin={}",
            sh_quote(&exe.to_string_lossy())
        ),
        // The `||` keeps a hook run with `sh -e` going when fnug is missing
        None => "fnug_bin=$(command -v fnug) || fnug_bin=".to_string(),
    };
    let run = format!("\"$fnug_bin\" {}", spec.args.join(" "));
    let mut lines = vec![
        BEGIN.to_string(),
        format!("{VERSION_PREFIX}{HOOK_FORMAT_VERSION}"),
        find,
        "if [ -x \"$fnug_bin\" ]; then".to_string(),
    ];
    if spec.config_rel.as_os_str().is_empty() {
        lines.push(format!("  {run} || exit $?"));
    } else {
        lines.extend([
            "  (".to_string(),
            "    # git passes a GIT_INDEX_FILE relative to the top; keep it valid after the cd"
                .to_string(),
            "    case ${GIT_INDEX_FILE-} in ''|/*) ;; *) GIT_INDEX_FILE=$PWD/$GIT_INDEX_FILE; export GIT_INDEX_FILE ;; esac".to_string(),
            format!(
                "    cd -- {} && exec {run}",
                sh_quote(&spec.config_rel.to_string_lossy())
            ),
            "  ) || exit $?".to_string(),
        ]);
    }
    lines.push("else".to_string());
    if spec.required {
        lines.push("  echo \"fnug not found. Install fnug, or run 'fnug setup' to remove this hook; 'git commit --no-verify' skips it once.\" >&2".to_string());
        lines.push("  exit 1".to_string());
    } else {
        lines.push(
            "  echo \"fnug not found, so its pre-commit checks were skipped\" >&2".to_string(),
        );
    }
    lines.push("fi".to_string());
    if spec.chain {
        lines.push(format!(
            "fnug_chained=\"$(dirname -- \"$0\")/{CHAINED_NAME}\""
        ));
        lines.push(
            "if [ -x \"$fnug_chained\" ]; then exec \"$fnug_chained\" \"$@\"; fi".to_string(),
        );
    }
    lines.push(END.to_string());
    lines.join("\n") + "\n"
}

/// `command`, run from `config_rel` when that isn't the top.
fn in_config_dir(config_rel: &Path, command: &str) -> String {
    if config_rel.as_os_str().is_empty() {
        command.to_string()
    } else {
        format!(
            "cd -- {} && {command}",
            sh_quote(&config_rel.to_string_lossy())
        )
    }
}

/// `value` as a single-quoted shell word.
fn sh_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

/// What runs a hook file, going by its shebang.
#[derive(Debug, PartialEq, Eq)]
enum Script {
    /// A POSIX-style shell, or no shebang, which git runs with `sh`.
    Shell,
    Foreign(String),
}

fn classify(content: &str) -> Script {
    let Some(shebang) = content.lines().next().and_then(|l| l.strip_prefix("#!")) else {
        return Script::Shell;
    };
    let basename = |path: &str| path.rsplit('/').next().unwrap_or(path).to_string();
    let mut words = shebang.split_whitespace();
    let mut program = words.next().map(basename);
    if program.as_deref() == Some("env") {
        // Skip `env`'s options and variable assignments, e.g. `env -S` or `env FOO=1`
        program = words
            .find(|w| !w.starts_with('-') && !w.contains('='))
            .map(basename);
    }
    match program {
        Some(program) if !SHELLS.contains(&program.as_str()) => Script::Foreign(program),
        _ => Script::Shell,
    }
}

/// Line range of the first fenced block, fences included.
fn find_block(lines: &[&str]) -> Option<Range<usize>> {
    let start = lines.iter().position(|l| l.trim_end() == BEGIN)?;
    let len = lines[start..].iter().position(|l| l.trim_end() == END)?;
    Some(start..start + len + 1)
}

/// Line range of the unfenced block fnug 0.1.0-alpha.13 and earlier appended: a `# fnug` line
/// followed by its `fnug … check …` line.
fn find_legacy(lines: &[&str]) -> Option<Range<usize>> {
    let is_run_line = |line: &str| {
        let mut words = line.split_whitespace();
        words.next() == Some("fnug") && words.any(|w| w == "check")
    };
    lines
        .windows(2)
        .position(|pair| pair[0].trim() == "# fnug" && is_run_line(pair[1]))
        .map(|i| i..i + 2)
}

/// Remove a legacy block, and the blank lines it leaves at the end of the hook.
fn remove_legacy(lines: &mut Vec<&str>) -> bool {
    let Some(range) = find_legacy(lines) else {
        return false;
    };
    let at_end = range.end == lines.len();
    lines.drain(range);
    while at_end && lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    true
}

/// `existing` with `block` in place of its fenced block, or after its shebang if it has none.
/// A legacy block is removed.
fn splice(existing: &str, block: &str) -> String {
    let mut lines: Vec<&str> = existing.split_inclusive('\n').collect();
    remove_legacy(&mut lines);
    let at = if let Some(range) = find_block(&lines) {
        let at = range.start;
        lines.drain(range);
        at
    } else {
        usize::from(lines.first().is_some_and(|l| l.starts_with("#!")))
    };
    let mut out = lines[..at].concat();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(block);
    out.push_str(&lines[at..].concat());
    out
}

struct Stripped {
    rest: String,
    /// The removed block ran a chained original hook.
    chained: bool,
}

/// `content` without fnug's fenced and legacy blocks, or `None` if it has neither.
fn strip(content: &str) -> Option<Stripped> {
    let mut lines: Vec<&str> = content.split_inclusive('\n').collect();
    let block = find_block(&lines).map(|range| lines.drain(range).collect::<String>());
    let legacy = remove_legacy(&mut lines);
    if block.is_none() && !legacy {
        return None;
    }
    Some(Stripped {
        rest: lines.concat(),
        chained: block.is_some_and(|b| b.contains(CHAINED_NAME)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const BLOCK: &str = "# >>> fnug >>>\nX\n# <<< fnug <<<\n";

    #[test]
    fn classify_by_shebang() {
        for shell in [
            "",
            "echo hi\n",
            "#!/bin/sh\n",
            "#!/bin/bash -e\n",
            "#!/usr/bin/env bash\n",
            "#!/usr/bin/env -S zsh -f\n",
            "#!/usr/bin/dash",
        ] {
            assert_eq!(classify(shell), Script::Shell, "{shell:?}");
        }
        assert_eq!(
            classify("#!/usr/bin/env python3\n"),
            Script::Foreign("python3".into())
        );
        assert_eq!(
            classify("#!/usr/bin/env FOO=1 node\n"),
            Script::Foreign("node".into())
        );
        assert_eq!(
            classify("#!/usr/bin/perl -w\n"),
            Script::Foreign("perl".into())
        );
    }

    #[test]
    fn splice_inserts_after_the_shebang() {
        assert_eq!(
            splice("#!/bin/sh\nrest\n", BLOCK),
            format!("#!/bin/sh\n{BLOCK}rest\n")
        );
        assert_eq!(splice("rest\n", BLOCK), format!("{BLOCK}rest\n"));
        assert_eq!(splice("#!/bin/sh", BLOCK), format!("#!/bin/sh\n{BLOCK}"));
        assert_eq!(splice("", BLOCK), BLOCK);
    }

    #[test]
    fn splice_replaces_the_block_in_place() {
        let existing = "#!/bin/sh\nfirst\n# >>> fnug >>>\nold\n# <<< fnug <<<\nlast\n";
        assert_eq!(
            splice(existing, BLOCK),
            format!("#!/bin/sh\nfirst\n{BLOCK}last\n")
        );
    }

    #[test]
    fn splice_drops_a_legacy_block() {
        let existing = "#!/bin/sh\nnpx lint-staged\n\n# fnug\nfnug check --fail-fast\n";
        assert_eq!(
            splice(existing, BLOCK),
            format!("#!/bin/sh\n{BLOCK}npx lint-staged\n")
        );
    }

    #[test]
    fn strip_undoes_splice() {
        for original in [
            "#!/bin/sh\nrest\n",
            "rest",
            "#!/bin/sh\n",
            "#!/usr/bin/env bash\r\nx\r\n",
        ] {
            let stripped = strip(&splice(original, BLOCK)).unwrap();
            assert_eq!(stripped.rest, original);
            assert!(!stripped.chained);
        }
    }

    #[test]
    fn strip_leaves_unfenced_mentions_alone() {
        assert!(strip("#!/bin/sh\n# fnug runs in CI\ncargo fmt --check\n").is_none());
        assert!(strip("#!/bin/sh\n# >>> fnug >>>\nno end fence\n").is_none());
    }

    #[test]
    fn failed_chain_puts_the_hook_back() {
        for original in [
            "#!/usr/bin/env python3\nexit(0)\n",
            "#!/usr/bin/env python3\n\n# fnug\nfnug check --fail-fast\n",
        ] {
            let dir = tempfile::tempdir().unwrap();
            let hook = dir.path().join("pre-commit");
            let local = hook.with_file_name(CHAINED_NAME);
            std::fs::write(&hook, original).unwrap();
            let (mut steps, ..) = chain_steps(&hook, Some(original), "wrapper".into()).unwrap();
            // Fails, because the parent directory is a file by then
            *steps.last_mut().unwrap() = write_step(&local.join("x"), String::new(), 0o755);

            let plan = HookPlan { steps, note: None };
            assert!(plan.apply().is_err());
            assert_eq!(std::fs::read_to_string(&hook).unwrap(), original);
            assert!(!local.exists());
        }
    }

    #[test]
    fn sh_quote_escapes_single_quotes() {
        assert_eq!(sh_quote("app"), "'app'");
        assert_eq!(sh_quote("it's"), r"'it'\''s'");
    }

    #[test]
    fn render_block_is_fenced_and_versioned() {
        let args = command_args(&InstallOptions {
            no_workspace: true,
            ..InstallOptions::default()
        });
        let shared = render_block(&BlockSpec {
            args: &args,
            config_rel: Path::new(""),
            fallback_exe: None,
            required: false,
            chain: false,
        });
        let lines: Vec<&str> = shared.lines().collect();
        assert_eq!(lines[0], BEGIN);
        assert_eq!(lines[1], format!("{VERSION_PREFIX}{HOOK_FORMAT_VERSION}"));
        assert_eq!(lines.last(), Some(&END));
        assert!(
            shared.contains(
                "  \"$fnug_bin\" --no-workspace check --fail-fast --mute-success || exit $?\n"
            ),
            "{shared}"
        );
        assert!(!shared.contains("exit 1"), "{shared}");

        let local = render_block(&BlockSpec {
            args: &args,
            config_rel: Path::new("app"),
            fallback_exe: Some(Path::new("/opt/it's/fnug")),
            required: true,
            chain: false,
        });
        assert!(
            local.contains(r"|| fnug_bin='/opt/it'\''s/fnug'"),
            "{local}"
        );
        assert!(
            local.contains("cd -- 'app' && exec \"$fnug_bin\""),
            "{local}"
        );
        assert!(local.contains("  exit 1\n"), "{local}");
    }
}
