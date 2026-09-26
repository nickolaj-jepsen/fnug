pub mod fsutil;
pub mod hooks;
pub mod mcp;
pub mod workspace;

use std::fmt;
use std::path::{Path, PathBuf};

use inquire::{Confirm, MultiSelect};
use thiserror::Error;

use crate::selectors::relative_to;
use crate::{LoadOptions, LoadedConfig};
use hooks::{
    ForeignPolicy, HookError, HookPlan, HookStatus, HookTarget, InstallOptions, InstallOutcome,
};
use mcp::{Editor, FileChange};

#[derive(Error, Debug)]
pub enum SetupError {
    #[error("fnug setup requires an interactive terminal")]
    NotInteractive,

    #[error("hook error: {0}")]
    Hook(#[from] hooks::HookError),

    #[error("MCP config error: {0}")]
    Mcp(#[from] mcp::McpError),

    #[error("cancelled")]
    Cancelled,

    #[error("{0}")]
    Prompt(#[from] inquire::InquireError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Feature {
    GitHooks,
    McpServer,
}

const FEATURES: [Feature; 2] = [Feature::GitHooks, Feature::McpServer];

impl fmt::Display for Feature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GitHooks => write!(f, "Git pre-commit hooks"),
            Self::McpServer => write!(f, "MCP server for editors"),
        }
    }
}

/// A repository's pre-commit hook and whether it runs fnug.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RepoHook {
    name: String,
    target: HookTarget,
    /// Against `opts`, so a hook that would change is outdated.
    status: HookStatus,
    /// How to install it; the foreign policy is decided when preparing.
    opts: InstallOptions,
}

impl RepoHook {
    fn is_installed(&self) -> bool {
        matches!(self.status, HookStatus::Installed | HookStatus::Outdated)
    }
}

/// What is set up now.
#[derive(Debug, Default)]
struct Detected {
    has_config: bool,
    /// Where the editors' MCP configs go.
    cwd: PathBuf,
    root_hook: Option<RepoHook>,
    sub_repo_hooks: Vec<RepoHook>,
    /// The editors whose config could be read, and whether it runs fnug.
    editors: Vec<(Editor, bool)>,
}

/// What the user picked.
#[derive(Debug, Default)]
struct Choice {
    features: Vec<Feature>,
    editors: Vec<Editor>,
    /// Indices into [`Detected::sub_repo_hooks`].
    sub_repos: Vec<usize>,
}

/// Indices into [`FEATURES`] to preselect: whatever is installed, and the hook when nothing is
/// and there is a config for it to run.
fn default_features(detected: &Detected) -> Vec<usize> {
    let hook_installed = detected
        .root_hook
        .as_ref()
        .is_some_and(RepoHook::is_installed);
    let mcp_installed = detected.editors.iter().any(|(_, installed)| *installed);
    let mut defaults = Vec::new();
    if hook_installed || (detected.has_config && !mcp_installed) {
        defaults.push(0);
    }
    if mcp_installed {
        defaults.push(1);
    }
    defaults
}

/// A change setup can make.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Action {
    InstallHook { hook: RepoHook },
    RemoveHook { hook: RepoHook },
    InstallMcp { editor: Editor, cwd: PathBuf },
    RemoveMcp { editor: Editor, cwd: PathBuf },
}

/// The hook's path, and the file it links to, which is the one that changes.
fn hook_file(target: &HookTarget) -> String {
    match &target.resolved {
        Some(resolved) => format!("{} -> {}", target.hook_path.display(), resolved.display()),
        None => target.hook_path.display().to_string(),
    }
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InstallHook { hook } if hook.status == HookStatus::Outdated => {
                write!(f, "~ Update pre-commit hook ({})", hook_file(&hook.target))
            }
            Self::InstallHook { hook } => {
                write!(f, "+ Install pre-commit hook ({})", hook_file(&hook.target))
            }
            Self::RemoveHook { hook } => {
                write!(f, "- Remove pre-commit hook ({})", hook_file(&hook.target))
            }
            Self::InstallMcp { editor, cwd } => write!(
                f,
                "+ Configure MCP for {editor} ({})",
                editor.config_path(cwd).display()
            ),
            Self::RemoveMcp { editor, cwd } => write!(
                f,
                "- Remove MCP from {editor} ({})",
                editor.config_path(cwd).display()
            ),
        }
    }
}

/// The actions that turn what is set up into what the user picked. Deselecting something removes
/// it.
fn plan_actions(detected: &Detected, choice: &Choice) -> Vec<Action> {
    let wants_hooks = choice.features.contains(&Feature::GitHooks);
    let wants_mcp = choice.features.contains(&Feature::McpServer);
    let mut actions = Vec::new();

    let hooks = detected
        .root_hook
        .iter()
        .map(|hook| (hook, wants_hooks))
        .chain(
            detected
                .sub_repo_hooks
                .iter()
                .enumerate()
                .map(|(i, hook)| (hook, wants_hooks && choice.sub_repos.contains(&i))),
        );
    for (hook, wanted) in hooks {
        let hook = hook.clone();
        match (hook.status, wanted) {
            (HookStatus::Installed, true)
            | (HookStatus::NotInstalled | HookStatus::Foreign, false) => {}
            (_, true) => actions.push(Action::InstallHook { hook }),
            (_, false) => actions.push(Action::RemoveHook { hook }),
        }
    }

    for &(editor, installed) in &detected.editors {
        let cwd = detected.cwd.clone();
        match (installed, wants_mcp && choice.editors.contains(&editor)) {
            (false, true) => actions.push(Action::InstallMcp { editor, cwd }),
            (true, false) => actions.push(Action::RemoveMcp { editor, cwd }),
            _ => {}
        }
    }
    actions
}

/// An action with its changes worked out and checked, ready to apply.
struct Prepared {
    action: Action,
    change: Change,
    notes: Vec<String>,
}

enum Change {
    Hook(HookPlan),
    Mcp {
        editor: Editor,
        cwd: PathBuf,
        change: FileChange,
    },
}

impl Action {
    /// Work out the action's changes without making any, or `None` if nothing needs changing.
    fn prepare(&self, foreign: ForeignPolicy) -> Result<Option<Prepared>, SetupError> {
        let mut notes = Vec::new();
        let change = match self {
            Self::InstallHook { hook } => {
                let opts = InstallOptions {
                    foreign,
                    ..hook.opts.clone()
                };
                let (plan, outcome) = hooks::plan_install(&hook.target, &opts)?;
                if let InstallOutcome::Chained { original } = outcome {
                    notes.push(format!(
                        "the existing hook moves to {} and runs after fnug",
                        original.display()
                    ));
                }
                notes.extend(plan.note().map(str::to_string));
                if let Some(config) = opts.config_file.as_ref().filter(|c| c.is_absolute()) {
                    notes.push(format!(
                        "the config is outside the repository, so the hook loads it by its absolute path, {}, in every clone and linked worktree",
                        config.display()
                    ));
                }
                Change::Hook(plan)
            }
            Self::RemoveHook { hook } => Change::Hook(hooks::plan_remove(&hook.target)?),
            Self::InstallMcp { editor, cwd } => {
                let Some(content) = editor.plan_install(cwd, &["mcp".to_string()])? else {
                    return Ok(None);
                };
                // The entry runs plain `fnug`: the file is shared, so no machine's path goes in
                if fsutil::find_on_path("fnug").is_none() {
                    notes.push(format!(
                        "fnug isn't on PATH, so {editor} can't start it until it is"
                    ));
                }
                Change::Mcp {
                    editor: *editor,
                    cwd: cwd.clone(),
                    change: FileChange::Write(content),
                }
            }
            Self::RemoveMcp { editor, cwd } => match editor.plan_remove(cwd)? {
                Some(change) => Change::Mcp {
                    editor: *editor,
                    cwd: cwd.clone(),
                    change,
                },
                None => return Ok(None),
            },
        };
        if let Change::Hook(plan) = &change
            && plan.is_empty()
        {
            return Ok(None);
        }
        Ok(Some(Prepared {
            action: self.clone(),
            change,
            notes,
        }))
    }
}

impl Prepared {
    fn apply(&self) -> Result<(), SetupError> {
        match &self.change {
            Change::Hook(plan) => plan.apply()?,
            Change::Mcp {
                editor,
                cwd,
                change,
            } => editor.apply(cwd, change)?,
        }
        Ok(())
    }
}

/// Where the root hook runs fnug from: `--root`, the tree the commands check, or else the config's
/// directory, where fnug finds the config.
fn root_hook_dir<'a>(
    cwd: &'a Path,
    config: Option<&'a LoadedConfig>,
    load: &LoadOptions,
) -> &'a Path {
    match config {
        Some(c) if load.root_dir.is_some() => &c.cwd,
        Some(c) => c.config_path.parent().unwrap_or(&c.cwd),
        None => cwd,
    }
}

/// How the root hook, running fnug from [`root_hook_dir`] in the work tree `workdir`, loads the
/// config the way `load` did.
fn root_hook_options(
    config: Option<&LoadedConfig>,
    load: &LoadOptions,
    workdir: &Path,
) -> InstallOptions {
    let no_workspace = load.no_workspace;
    let Some(c) = config else {
        return InstallOptions {
            no_workspace,
            ..InstallOptions::default()
        };
    };
    let config_file = match (&load.config, &load.root_dir) {
        (None, _) => None,
        (Some(_), None) => c.config_path.file_name().map(PathBuf::from),
        // Relative where it can be, since linked worktrees share the hook
        (Some(_), Some(_)) if c.config_path.starts_with(workdir) => {
            Some(relative_to(&c.config_path, &c.cwd))
        }
        (Some(_), Some(_)) => Some(c.config_path.clone()),
    };
    InstallOptions {
        no_workspace,
        config_file,
        root_dir: load.root_dir.as_ref().map(|_| PathBuf::from(".")),
        ..InstallOptions::default()
    }
}

/// Find the hooks and editor configs, logging why any of them can't be set up.
fn detect(cwd: &Path, config: Option<&LoadedConfig>, load: &LoadOptions) -> Detected {
    if config.is_none() {
        log::warn!(
            "no fnug config loaded. The pre-commit hook and the MCP server run the commands in one, so add a .fnug.yaml before relying on them."
        );
    }
    let fallback_exe = std::env::current_exe().ok();
    let repo_hook = |name: String, dir: &Path, opts: &dyn Fn(&HookTarget) -> InstallOptions| {
        let target = hooks::resolve(dir)
            .inspect_err(|e| log::warn!("can't set up a git hook for {}: {e}", dir.display()))
            .ok()?;
        let opts = InstallOptions {
            fallback_exe: fallback_exe.clone(),
            ..opts(&target)
        };
        Some(RepoHook {
            name,
            status: hooks::status_with(&target, &opts),
            target,
            opts,
        })
    };
    let hook_dir = root_hook_dir(cwd, config, load);
    let root_hook = repo_hook(String::new(), hook_dir, &|target| {
        root_hook_options(config, load, &target.workdir)
    });
    let sub_opts = InstallOptions {
        no_workspace: true,
        ..InstallOptions::default()
    };
    let sub_repo_hooks = config
        .map(|c| workspace::find_sub_repos(hook_dir, &c.root))
        .unwrap_or_default()
        .into_iter()
        .filter_map(|sub| repo_hook(sub.name, &sub.path, &|_| sub_opts.clone()))
        .collect();
    let editors = Editor::ALL
        .into_iter()
        .filter_map(|editor| match editor.status(cwd) {
            Ok(installed) => Some((editor, installed)),
            Err(e) => {
                log::warn!("leaving {editor}'s MCP config alone: {e}");
                None
            }
        })
        .collect();
    Detected {
        has_config: config.is_some(),
        cwd: cwd.to_path_buf(),
        root_hook,
        sub_repo_hooks,
        editors,
    }
}

fn prompt(detected: &Detected) -> Result<Choice, SetupError> {
    let features = MultiSelect::new("What would you like to set up?", FEATURES.to_vec())
        .with_default(&default_features(detected))
        .prompt()?;

    let editors = if features.contains(&Feature::McpServer) && !detected.editors.is_empty() {
        let options = detected.editors.iter().map(|(e, _)| *e).collect();
        let installed = indices(detected.editors.iter().map(|(_, i)| *i));
        MultiSelect::new("Which editors?", options)
            .with_default(&installed)
            .prompt()?
    } else {
        Vec::new()
    };

    let hooks = &detected.sub_repo_hooks;
    let sub_repos = if features.contains(&Feature::GitHooks) && !hooks.is_empty() {
        let mut defaults = indices(hooks.iter().map(RepoHook::is_installed));
        if defaults.is_empty() {
            defaults = (0..hooks.len()).collect();
        }
        let names = hooks.iter().map(|h| h.name.clone()).collect();
        MultiSelect::new("Which sub-repos to install hooks in?", names)
            .with_default(&defaults)
            .raw_prompt()?
            .into_iter()
            .map(|option| option.index)
            .collect()
    } else {
        Vec::new()
    };

    Ok(Choice {
        features,
        editors,
        sub_repos,
    })
}

fn indices(flags: impl Iterator<Item = bool>) -> Vec<usize> {
    flags
        .enumerate()
        .filter_map(|(i, set)| set.then_some(i))
        .collect()
}

/// Prepare every action, so refusals and unreadable files show up before anything changes. A
/// hook that isn't a shell script is chained if the user agrees.
fn prepare_all(actions: &[Action]) -> Result<Vec<Prepared>, SetupError> {
    let mut prepared = Vec::new();
    for action in actions {
        let result = match action.prepare(ForeignPolicy::Refuse) {
            Err(SetupError::Hook(HookError::ForeignHook {
                path, interpreter, ..
            })) if confirm_chain(&path, &interpreter)? => action.prepare(ForeignPolicy::Chain),
            result => result,
        };
        match result {
            Ok(Some(ready)) => prepared.push(ready),
            Ok(None) => {}
            Err(e) => println!("Skipping \"{action}\": {e}"),
        }
    }
    Ok(prepared)
}

fn confirm_chain(hook: &Path, interpreter: &str) -> Result<bool, SetupError> {
    let question = format!(
        "{} is a {interpreter} script. Move it to pre-commit.local and run it after fnug?",
        hook.display()
    );
    Ok(Confirm::new(&question).with_default(false).prompt()?)
}

/// Run the interactive setup wizard: it asks what to set up, shows every change it will make,
/// and makes them only once the user confirms. `load` is how `config` was loaded; the pre-commit
/// hook passes the same `-c`, `--root` and `--no-workspace`.
///
/// # Errors
///
/// Returns `SetupError` on prompt failures, IO errors, or if not run in a terminal.
pub fn run(
    cwd: &Path,
    config: Option<&LoadedConfig>,
    load: &LoadOptions,
) -> Result<(), SetupError> {
    if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        return Err(SetupError::NotInteractive);
    }

    let detected = detect(cwd, config, load);
    let choice = prompt(&detected)?;
    let prepared = prepare_all(&plan_actions(&detected, &choice))?;

    if prepared.is_empty() {
        println!("Nothing to change.");
        return Ok(());
    }
    println!("\nChanges:");
    for ready in &prepared {
        println!("  {}", ready.action);
        for note in &ready.notes {
            println!("      note: {note}");
        }
    }
    println!();
    if !Confirm::new("Apply?").with_default(false).prompt()? {
        return Err(SetupError::Cancelled);
    }

    for ready in &prepared {
        ready.apply()?;
    }
    println!("Done!");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hook(name: &str, status: HookStatus) -> RepoHook {
        let dir = PathBuf::from("/repo").join(name);
        RepoHook {
            name: name.to_string(),
            target: HookTarget {
                hooks_dir: dir.join(".git/hooks"),
                hook_path: dir.join(".git/hooks/pre-commit"),
                resolved: None,
                workdir: dir,
                location: hooks::HookLocation::Local,
                config_rel: PathBuf::new(),
            },
            status,
            opts: InstallOptions {
                no_workspace: !name.is_empty(),
                ..InstallOptions::default()
            },
        }
    }

    fn detected(hook_status: HookStatus, editors: &[(Editor, bool)]) -> Detected {
        Detected {
            has_config: true,
            cwd: PathBuf::from("/repo"),
            root_hook: Some(hook("", hook_status)),
            sub_repo_hooks: Vec::new(),
            editors: editors.to_vec(),
        }
    }

    #[test]
    fn default_features_follow_what_is_installed() {
        let cases = [
            (HookStatus::NotInstalled, false, true, vec![0]),
            (HookStatus::NotInstalled, false, false, vec![]),
            (HookStatus::Installed, false, false, vec![0]),
            (HookStatus::Outdated, true, true, vec![0, 1]),
            (HookStatus::NotInstalled, true, true, vec![1]),
            (HookStatus::Foreign, false, true, vec![0]),
        ];
        for (status, mcp, has_config, expected) in cases {
            let detected = Detected {
                has_config,
                ..detected(status, &[(Editor::VsCode, mcp)])
            };
            assert_eq!(
                default_features(&detected),
                expected,
                "{status:?}, mcp {mcp}, config {has_config}"
            );
        }
    }

    #[test]
    fn empty_selection_removes_everything_installed() {
        let detected = Detected {
            sub_repo_hooks: vec![
                hook("lib", HookStatus::Outdated),
                hook("docs", HookStatus::NotInstalled),
            ],
            ..detected(
                HookStatus::Installed,
                &[(Editor::ClaudeCode, true), (Editor::VsCode, false)],
            )
        };

        let actions = plan_actions(&detected, &Choice::default());

        assert_eq!(
            actions,
            [
                Action::RemoveHook {
                    hook: hook("", HookStatus::Installed)
                },
                Action::RemoveHook {
                    hook: hook("lib", HookStatus::Outdated)
                },
                Action::RemoveMcp {
                    editor: Editor::ClaudeCode,
                    cwd: PathBuf::from("/repo")
                },
            ]
        );
    }

    #[test]
    fn kept_selection_installs_missing_and_updates_outdated() {
        let detected = Detected {
            sub_repo_hooks: vec![
                hook("lib", HookStatus::Installed),
                hook("docs", HookStatus::NotInstalled),
            ],
            ..detected(HookStatus::Outdated, &[(Editor::Cursor, false)])
        };
        let choice = Choice {
            features: FEATURES.to_vec(),
            editors: vec![Editor::Cursor],
            sub_repos: vec![0, 1],
        };

        let actions = plan_actions(&detected, &choice);

        assert_eq!(
            actions,
            [
                Action::InstallHook {
                    hook: hook("", HookStatus::Outdated)
                },
                Action::InstallHook {
                    hook: hook("docs", HookStatus::NotInstalled)
                },
                Action::InstallMcp {
                    editor: Editor::Cursor,
                    cwd: PathBuf::from("/repo")
                },
            ]
        );
        assert!(actions[0].to_string().starts_with("~ Update"));
    }

    #[test]
    fn linked_hook_shows_the_file_that_changes() {
        let mut linked = hook("", HookStatus::NotInstalled);
        linked.target.resolved = Some(PathBuf::from("/repo/scripts/pre-commit"));
        assert_eq!(
            Action::InstallHook { hook: linked }.to_string(),
            "+ Install pre-commit hook (/repo/.git/hooks/pre-commit -> /repo/scripts/pre-commit)"
        );
    }

    #[test]
    fn prepare_finds_refusals_before_anything_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let path = Editor::VsCode.config_path(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{ not json").unwrap();
        let action = Action::InstallMcp {
            editor: Editor::VsCode,
            cwd: dir.path().to_path_buf(),
        };

        let prepared = prepare_all(std::slice::from_ref(&action))
            .map(|prepared| prepared.len())
            .unwrap();
        assert_eq!(prepared, 0, "the unreadable config is skipped");
        assert!(matches!(
            action.prepare(ForeignPolicy::Refuse),
            Err(SetupError::Mcp(mcp::McpError::Parse { .. }))
        ));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ not json");
    }

    /// Every warning logged by this test binary so far.
    fn logged_warnings() -> &'static std::sync::Mutex<Vec<String>> {
        struct Capture;
        impl log::Log for Capture {
            fn enabled(&self, metadata: &log::Metadata) -> bool {
                metadata.level() <= log::Level::Warn
            }
            fn log(&self, record: &log::Record) {
                if self.enabled(record.metadata()) {
                    // A failed assert holding the lock mustn't fail every later test that logs
                    WARNINGS
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push(record.args().to_string());
                }
            }
            fn flush(&self) {}
        }
        static WARNINGS: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
        static INIT: std::sync::Once = std::sync::Once::new();
        INIT.call_once(|| {
            log::set_logger(&Capture).unwrap();
            log::set_max_level(log::LevelFilter::Warn);
        });
        &WARNINGS
    }

    #[test]
    fn detect_logs_what_it_cant_set_up() {
        let warnings = logged_warnings();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(Editor::ClaudeCode.config_path(dir.path()), "{ not json").unwrap();

        let detected = detect(dir.path(), None, &LoadOptions::default());

        assert!(detected.root_hook.is_none());
        let shown = dir.path().display().to_string();
        let warnings = warnings
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let logged = |text: &str| warnings.iter().any(|w| w.contains(text));
        assert!(logged("no fnug config loaded"), "{warnings:?}");
        assert!(
            logged(&format!("can't set up a git hook for {shown}")),
            "{warnings:?}"
        );
        assert!(
            logged(&format!(
                "leaving Claude Code's MCP config alone: failed to parse {shown}"
            )),
            "{warnings:?}"
        );
    }

    /// A git repository at `dir` whose hooks are in `.git/hooks`, whatever the global config says.
    fn init_repo(dir: &Path) {
        let repo = git2::Repository::init(dir).unwrap();
        repo.config()
            .unwrap()
            .set_str("core.hooksPath", ".git/hooks")
            .unwrap();
    }

    /// What the hook `detect` found loads, run like git runs it: from the top of the work tree.
    fn hook_loads(hook: &RepoHook) -> LoadedConfig {
        crate::load(&LoadOptions {
            start_dir: Some(hook.target.workdir.join(&hook.target.config_rel)),
            config: hook.opts.config_file.clone(),
            root_dir: hook.opts.root_dir.clone(),
            no_workspace: hook.opts.no_workspace,
            ..LoadOptions::default()
        })
        .unwrap()
    }

    #[test]
    fn root_hook_is_the_roots_repository() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().canonicalize().unwrap();
        // The config in its own repository, and one in a directory of the checked repository
        let (shared, project) = (base.join("shared"), base.join("project"));
        for repo in [&shared, &project] {
            init_repo(repo);
        }
        std::fs::create_dir_all(project.join("cfg")).unwrap();
        std::fs::create_dir_all(project.join("app")).unwrap();
        for config in [shared.join("ci.yaml"), project.join("cfg/ci.yaml")] {
            std::fs::write(&config, "name: ci\ncommands: []\n").unwrap();
        }

        for (config, root, config_arg, config_rel) in [
            (
                shared.join("ci.yaml"),
                project.clone(),
                shared.join("ci.yaml"),
                "",
            ),
            (
                project.join("cfg/ci.yaml"),
                project.join("app"),
                PathBuf::from("../cfg/ci.yaml"),
                "app",
            ),
        ] {
            let load = LoadOptions {
                config: Some(config.clone()),
                root_dir: Some(root.clone()),
                ..LoadOptions::default()
            };
            let loaded = crate::load(&load).unwrap();

            let hook = detect(&loaded.cwd, Some(&loaded), &load).root_hook.unwrap();

            assert_eq!(hook.target.hook_path, project.join(".git/hooks/pre-commit"));
            assert_eq!(hook.target.config_rel, Path::new(config_rel));
            assert_eq!(hook.opts.config_file, Some(config_arg.clone()));
            assert_eq!(hook.opts.root_dir.as_deref(), Some(Path::new(".")));
            let loads = hook_loads(&hook);
            assert_eq!((loads.config_path, loads.cwd), (config, root));
            let notes = Action::InstallHook { hook }
                .prepare(ForeignPolicy::Refuse)
                .unwrap()
                .unwrap()
                .notes;
            assert_eq!(
                notes.iter().any(|n| n.contains("absolute path")),
                config_arg.is_absolute(),
                "{notes:?}"
            );
        }
    }

    #[test]
    fn root_hook_loads_the_config_like_setup_did() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::create_dir(root.join("app")).unwrap();
        std::fs::write(root.join("app/ci.yaml"), "name: ci\ncommands: []\n").unwrap();
        std::fs::write(root.join("app/.fnug.yaml"), "name: app\ncommands: []\n").unwrap();

        let pinned = LoadOptions {
            config: Some(root.join("app/ci.yaml")),
            root_dir: Some(root.clone()),
            no_workspace: true,
            ..LoadOptions::default()
        };
        let loaded = crate::load(&pinned).unwrap();
        assert_eq!(root_hook_dir(&root, Some(&loaded), &pinned), root);
        let opts = root_hook_options(Some(&loaded), &pinned, &root);
        assert_eq!(opts.config_file.as_deref(), Some(Path::new("app/ci.yaml")));
        assert_eq!(opts.root_dir.as_deref(), Some(Path::new(".")));
        assert!(opts.no_workspace);

        let config_only = LoadOptions {
            root_dir: None,
            no_workspace: false,
            ..pinned
        };
        let loaded = crate::load(&config_only).unwrap();
        assert_eq!(
            root_hook_dir(&root, Some(&loaded), &config_only),
            root.join("app")
        );
        let opts = root_hook_options(Some(&loaded), &config_only, &root);
        assert_eq!(opts.config_file.as_deref(), Some(Path::new("ci.yaml")));
        assert_eq!(opts.root_dir, None);

        let found = LoadOptions {
            start_dir: Some(root.join("app")),
            ..LoadOptions::default()
        };
        let loaded = crate::load(&found).unwrap();
        assert_eq!(
            root_hook_options(Some(&loaded), &found, &root),
            InstallOptions::default()
        );

        let root_only = LoadOptions {
            root_dir: Some(root.join("app")),
            ..found
        };
        let loaded = crate::load(&root_only).unwrap();
        let opts = root_hook_options(Some(&loaded), &root_only, &root);
        assert_eq!(opts.config_file, None);
        assert_eq!(opts.root_dir.as_deref(), Some(Path::new(".")));
    }
}
