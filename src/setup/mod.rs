pub mod hooks;
pub mod mcp;
pub mod workspace;

use std::fmt;
use std::path::{Path, PathBuf};

use inquire::MultiSelect;
use thiserror::Error;

use crate::commands::group::CommandGroup;
use mcp::Editor;

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

/// An action to be taken during setup.
enum Action {
    InstallHook { path: PathBuf, no_workspace: bool },
    RemoveHook { path: PathBuf },
    InstallMcp { editor: Editor, cwd: PathBuf },
    RemoveMcp { editor: Editor, cwd: PathBuf },
}

impl Action {
    fn execute(&self) -> Result<(), SetupError> {
        match self {
            Self::InstallHook { path, no_workspace } => {
                hooks::install(path, *no_workspace)?;
                println!("Installed pre-commit hook in {}", path.display());
            }
            Self::RemoveHook { path } => {
                hooks::remove(path)?;
                println!("Removed pre-commit hook from {}", path.display());
            }
            Self::InstallMcp { editor, cwd } => {
                editor.install(cwd)?;
                println!("Configured MCP for {editor}");
            }
            Self::RemoveMcp { editor, cwd } => {
                editor.remove(cwd)?;
                println!("Removed MCP from {editor}");
            }
        }
        Ok(())
    }
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InstallHook { path, .. } => {
                write!(
                    f,
                    "  + Install pre-commit hook ({})",
                    path.join(".git/hooks/pre-commit").display()
                )
            }
            Self::RemoveHook { path, .. } => {
                write!(
                    f,
                    "  - Remove pre-commit hook ({})",
                    path.join(".git/hooks/pre-commit").display()
                )
            }
            Self::InstallMcp { editor, cwd } => {
                write!(
                    f,
                    "  + Configure MCP for {editor} ({})",
                    editor.config_path(cwd).display()
                )
            }
            Self::RemoveMcp { editor, cwd } => {
                write!(
                    f,
                    "  - Remove MCP from {editor} ({})",
                    editor.config_path(cwd).display()
                )
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Feature {
    GitHooks,
    McpServer,
}

impl fmt::Display for Feature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GitHooks => write!(f, "Git pre-commit hooks"),
            Self::McpServer => write!(f, "MCP server for editors"),
        }
    }
}

/// Prompt the user for which features/editors/sub-repos they want, then build
/// the list of actions to apply.
#[allow(clippy::too_many_lines)]
fn gather_actions(cwd: &Path, config: Option<&CommandGroup>) -> Result<Vec<Action>, SetupError> {
    // Detect current state
    let hooks_installed = hooks::is_installed(cwd);
    let editors_installed: Vec<(Editor, bool)> = Editor::ALL
        .iter()
        .map(|&e| (e, e.is_installed(cwd)))
        .collect();
    let any_mcp_installed = editors_installed.iter().any(|(_, installed)| *installed);

    // Select features
    let features = vec![Feature::GitHooks, Feature::McpServer];
    let mut default_indices: Vec<usize> = Vec::new();
    if hooks_installed {
        default_indices.push(0);
    }
    if any_mcp_installed {
        default_indices.push(1);
    }
    if default_indices.is_empty() {
        default_indices.push(0);
    }

    let selected_features = MultiSelect::new("What would you like to set up?", features)
        .with_default(&default_indices)
        .prompt()?;

    if selected_features.is_empty() {
        println!("Nothing selected.");
        return Ok(vec![]);
    }

    let wants_hooks = selected_features.contains(&Feature::GitHooks);
    let wants_mcp = selected_features.contains(&Feature::McpServer);

    // Select editors for MCP
    let selected_editors = if wants_mcp {
        let editor_options: Vec<Editor> = Editor::ALL.to_vec();
        let default_editor_indices: Vec<usize> = editors_installed
            .iter()
            .enumerate()
            .filter(|(_, (_, installed))| *installed)
            .map(|(i, _)| i)
            .collect();

        MultiSelect::new("Which editors?", editor_options)
            .with_default(&default_editor_indices)
            .prompt()?
    } else {
        vec![]
    };

    // Find sub-repos for hook installation
    let sub_repos = if wants_hooks {
        config
            .map(|c| workspace::find_sub_repos(cwd, c))
            .unwrap_or_default()
    } else {
        vec![]
    };

    let selected_sub_repo_indices = if sub_repos.is_empty() {
        vec![]
    } else {
        let sub_repo_names: Vec<String> = sub_repos.iter().map(|r| r.name.clone()).collect();
        let mut defaults: Vec<usize> = sub_repos
            .iter()
            .enumerate()
            .filter(|(_, r)| hooks::is_installed(&r.path))
            .map(|(i, _)| i)
            .collect();
        if defaults.is_empty() {
            defaults = (0..sub_repos.len()).collect();
        }

        MultiSelect::new("Which sub-repos to install hooks in?", sub_repo_names)
            .with_default(&defaults)
            .prompt()?
            .iter()
            .filter_map(|name| sub_repos.iter().position(|r| &r.name == name))
            .collect::<Vec<_>>()
    };

    // Build action list (diff desired vs current state)
    let mut actions: Vec<Action> = Vec::new();

    // Root hook
    if wants_hooks && !hooks_installed {
        actions.push(Action::InstallHook {
            path: cwd.to_path_buf(),
            no_workspace: false,
        });
    } else if !wants_hooks && hooks_installed {
        actions.push(Action::RemoveHook {
            path: cwd.to_path_buf(),
        });
    }

    // Sub-repo hooks
    for (i, sub_repo) in sub_repos.iter().enumerate() {
        let wanted = selected_sub_repo_indices.contains(&i);
        let installed = hooks::is_installed(&sub_repo.path);
        if wanted && !installed {
            actions.push(Action::InstallHook {
                path: sub_repo.path.clone(),
                no_workspace: true,
            });
        } else if !wanted && installed {
            actions.push(Action::RemoveHook {
                path: sub_repo.path.clone(),
            });
        }
    }

    // MCP editors
    for &editor in &Editor::ALL {
        let wanted = selected_editors.contains(&editor);
        let installed = editor.is_installed(cwd);
        if wanted && !installed {
            actions.push(Action::InstallMcp {
                editor,
                cwd: cwd.to_path_buf(),
            });
        } else if !wanted && installed {
            actions.push(Action::RemoveMcp {
                editor,
                cwd: cwd.to_path_buf(),
            });
        }
    }

    Ok(actions)
}

/// Run the interactive setup wizard.
///
/// # Errors
///
/// Returns `SetupError` on prompt failures, IO errors, or if not run in a terminal.
pub fn run(cwd: &Path, config: Option<&CommandGroup>) -> Result<(), SetupError> {
    if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        return Err(SetupError::NotInteractive);
    }

    let actions = gather_actions(cwd, config)?;

    if actions.is_empty() {
        println!("Everything is already configured. No changes needed.");
        return Ok(());
    }

    // Display and confirm
    println!("\nChanges:");
    for action in &actions {
        println!("{action}");
    }
    println!();

    let confirmed = inquire::Confirm::new("Apply?")
        .with_default(false)
        .prompt()?;

    if !confirmed {
        return Err(SetupError::Cancelled);
    }

    for action in &actions {
        action.execute()?;
    }
    println!("\nDone!");
    Ok(())
}
