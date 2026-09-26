//! `fnug init`: create a config for the tooling a project uses.

mod detect;
mod render;

use std::io;
use std::path::{Path, PathBuf};

use inquire::MultiSelect;
use inquire::list_option::ListOption;
use log::warn;
use thiserror::Error;

use crate::config_file::{ConfigCommandGroup, WorkspaceConfig, find_config_in_dir};
use crate::setup::fsutil;

pub use detect::{Proposal, detect};
pub use render::render;

/// Why a config couldn't be created.
#[derive(Error, Debug)]
pub enum InitError {
    #[error("{0} already exists; run `fnug init --force` to replace it")]
    Exists(PathBuf),

    #[error("unable to create a config in {path}: {source}")]
    Dir {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("unable to write {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("unable to write the config as YAML: {0}")]
    Yaml(#[from] serde_yaml::Error),

    #[error("unable to write the config as JSON: {0}")]
    Json(#[from] serde_json::Error),

    #[error("{0}")]
    Prompt(#[from] inquire::InquireError),
}

/// Where `fnug init` creates a config, and how.
#[derive(Debug, Clone, Default)]
pub struct InitOptions {
    /// The project directory the config goes in.
    pub dir: PathBuf,
    /// Replace the config fnug would load from `dir`, if it has one.
    pub force: bool,
    /// Include every proposal without asking.
    pub yes: bool,
}

/// A config ready to be written.
#[derive(Debug)]
pub struct NewConfig {
    pub path: PathBuf,
    pub content: String,
}

impl NewConfig {
    /// Write the config in one step, so a crash never leaves a partial file.
    ///
    /// # Errors
    ///
    /// Returns `InitError::Write` if the file can't be written.
    pub fn write(&self) -> Result<(), InitError> {
        fsutil::write_atomic(&self.path, &self.content, None).map_err(|source| InitError::Write {
            path: self.path.clone(),
            source,
        })
    }
}

/// The config for the project in `dir`, named after the directory, with `groups` and `ws` (see
/// [`render`]). It is `dir/.fnug.yaml`, or with `force`, whichever config fnug would load from
/// `dir`, in that file's format.
///
/// # Errors
///
/// Returns `InitError::Exists` if `dir` has a config and `force` is false, and
/// `InitError::Yaml` or `InitError::Json` if the config can't be serialized.
pub fn prepare(
    dir: &Path,
    force: bool,
    groups: Vec<ConfigCommandGroup>,
    ws: Option<WorkspaceConfig>,
) -> Result<NewConfig, InitError> {
    let path = target(dir, force)?;
    let name = project_name(dir);
    let content = if path.extension().is_some_and(|ext| ext == "json") {
        render::render_json(&name, groups, ws)?
    } else {
        render(&name, groups, ws)?
    };
    Ok(NewConfig { path, content })
}

fn target(dir: &Path, force: bool) -> Result<PathBuf, InitError> {
    match find_config_in_dir(dir) {
        Some(existing) if force => Ok(existing),
        Some(existing) => Err(InitError::Exists(existing)),
        None => Ok(dir.join(".fnug.yaml")),
    }
}

fn project_name(dir: &Path) -> String {
    let dir = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    dir.file_name().map_or_else(
        || "project".to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// Ask which proposals to include, with all of them preselected, and return their indices.
///
/// # Errors
///
/// Returns `InitError::Prompt` if the prompt fails or the user cancels it.
pub fn prompt(proposals: &[Proposal]) -> Result<Vec<usize>, InitError> {
    let answer = |chosen: &[ListOption<&&Proposal>]| {
        let labels: Vec<_> = chosen.iter().map(|o| o.value.label.as_str()).collect();
        labels.join(", ")
    };
    let chosen = MultiSelect::new(
        "Which commands should the config include?",
        proposals.iter().collect(),
    )
    .with_all_selected_by_default()
    .with_formatter(&answer)
    .raw_prompt()?;
    Ok(chosen.into_iter().map(|option| option.index).collect())
}

/// Create a config for the project in `opts.dir` from what [`detect`] finds there, and return
/// its path. `choose` picks the proposals to include, as indices, unless `opts.yes` includes
/// them all. With none picked or found, the config has an example command instead.
///
/// # Errors
///
/// Returns `InitError::Dir` if `opts.dir` isn't a directory, `InitError::Exists` if it has a
/// config and `opts.force` is false (before calling `choose`), whatever `choose` returns, and
/// `InitError::Write` if the config can't be written.
pub fn run(
    opts: &InitOptions,
    choose: impl FnOnce(&[Proposal]) -> Result<Vec<usize>, InitError>,
) -> Result<PathBuf, InitError> {
    let dir_error = |source| InitError::Dir {
        path: opts.dir.clone(),
        source,
    };
    let dir = opts.dir.canonicalize().map_err(dir_error)?;
    if !dir.is_dir() {
        return Err(dir_error(io::ErrorKind::NotADirectory.into()));
    }
    target(&dir, opts.force)?;

    let proposals = detect(&dir);
    if proposals.is_empty() {
        warn_nothing_detected(&dir);
    }
    let chosen = if opts.yes || proposals.is_empty() {
        (0..proposals.len()).collect()
    } else {
        choose(&proposals)?
    };
    let groups = proposals
        .into_iter()
        .enumerate()
        .filter(|(i, _)| chosen.contains(i))
        .map(|(_, proposal)| proposal.group)
        .collect();
    let config = prepare(&dir, opts.force, groups, None)?;
    config.write()?;
    Ok(config.path)
}

fn warn_nothing_detected(dir: &Path) {
    let repo_root = git2::Repository::discover(dir)
        .ok()
        .and_then(|repo| repo.workdir()?.canonicalize().ok());
    match repo_root {
        Some(root) if root != dir => warn!(
            "{} isn't the top of its git repository and has no tooling fnug knows, so its config has an example command; did you mean `fnug init {}`?",
            dir.display(),
            root.display()
        ),
        _ => warn!(
            "found no tooling in {}, so the config has an example command; `fnug init --help` lists what fnug looks for",
            dir.display()
        ),
    }
}
