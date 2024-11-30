//! Core implementation of the Fnug command scheduler
//!
//! Fnug is a command scheduler that detects and executes commands based on file system
//! and git changes. It allows users to define commands and command groups in a configuration
//! file, with flexible automation rules for when commands should be executed.

use crate::commands::inherit::Inheritance;
use crate::pty::python::{Output, OutputIterator, Process};
use crate::selectors::watch::{watch, WatcherIterator};
use commands::auto::Auto;
use commands::command::Command;
use commands::group::CommandGroup;
use commands::inherit::Inheritable;
use config_file::Config;
use log::{debug, LevelFilter};
use pyo3::{exceptions::PyFileNotFoundError, prelude::*};
use pyo3_log::{Caching, Logger};
use selectors::get_selected_commands;
use std::path::PathBuf;

pub mod commands;
mod config_file;
pub mod pty;
mod selectors;
mod ui;

#[cfg_attr(feature = "stub_gen", pyo3_stub_gen::derive::gen_stub_pyclass)]
#[pyclass]
struct FnugCore {
    #[pyo3(get)]
    config: CommandGroup,
    cwd: PathBuf,
}

/// The main entry point for the Fnug command scheduler
#[cfg_attr(feature = "stub_gen", pyo3_stub_gen::derive::gen_stub_pymethods)]
#[pymethods]
impl FnugCore {
    /// Creates a new FnugCore instance from an existing CommandGroup
    ///
    /// This method is useful when you want to programmatically create a command structure
    /// rather than loading it from a configuration file.
    #[staticmethod]
    fn from_group(command_group: CommandGroup, cwd: PathBuf) -> PyResult<Self> {
        debug!(
            "Creating core from group: {:?} (cwd: {:?})",
            command_group.name, cwd
        );

        let mut command_group = command_group;
        command_group.inherit(&Inheritance::from(cwd.clone()))?;

        Ok(FnugCore {
            config: command_group,
            cwd,
        })
    }

    /// Creates a new FnugCore instance by loading a configuration file
    ///
    /// If no configuration file is specified, Fnug will search for a .fnug.yaml,
    /// .fnug.yml, or .fnug.json file in the current directory and its parents.
    ///
    /// # Errors
    ///
    /// - Raises `PyFileNotFoundError` if the config file doesn't exist or can't be read
    /// - Raises `PyValueError` if the config file contains invalid YAML/JSON
    ///
    /// # Examples
    ///
    /// ```python
    /// # Load from specific file
    /// core = FnugCore.from_config_file(".fnug.yaml")
    ///
    /// # Auto-detect config file
    /// core = FnugCore.from_config_file()
    /// ```
    #[staticmethod]
    #[pyo3(signature = (config_file=None))]
    fn from_config_file(config_file: Option<&str>) -> PyResult<Self> {
        let config_path = match config_file {
            Some(file) => {
                let config_path = PathBuf::from(file);
                if !config_path.exists() {
                    return Err(PyFileNotFoundError::new_err("Config file not found"));
                }
                config_path
            }
            None => Config::find_config().map_err(|err| {
                PyFileNotFoundError::new_err(format!("Error finding config file: {:?}", err))
            })?,
        };
        let cwd = config_path.parent().unwrap().to_path_buf();
        debug!(
            "Creating core from config file: {:?} (cwd: {:?})",
            config_path, cwd
        );
        let mut config: CommandGroup = Config::from_file(&config_path)?.root.try_into()?;

        config.inherit(&Inheritance::from(cwd.clone()))?;

        Ok(FnugCore { config, cwd })
    }

    /// Returns the working directory as a Python pathlib.Path object
    #[getter]
    fn get_cwd(&self, py: Python<'_>) -> PyResult<PyObject> {
        let pathlib = py.import_bound("pathlib")?;
        let path = pathlib.getattr("Path")?;
        let obj = path.call1((self.cwd.to_string_lossy(),))?;
        let resolved = obj.call_method0("resolve")?;
        Ok(resolved.into())
    }

    /// Returns a list of all commands in the configuration
    ///
    /// This includes commands from all nested command groups.
    fn all_commands(&self) -> Vec<Command> {
        self.config.all_commands().into_iter().cloned().collect()
    }

    /// Returns a async iterator that watches for file system changes, yielding commands to run
    fn watch(&self, py: Python<'_>) -> PyResult<WatcherIterator> {
        py.allow_threads(move || watch(self.all_commands()))
    }

    /// Returns commands that have detected git changes in their watched paths, or have `always=True`
    fn selected_commands(&self) -> PyResult<Vec<Command>> {
        let commands = self.config.all_commands().into_iter().cloned().collect();
        Ok(get_selected_commands(commands)?)
    }
}

#[pymodule]
fn core<'py>(py: Python<'py>, m: Bound<'py, PyModule>) -> PyResult<()> {
    Logger::new(py, Caching::LoggersAndLevels)?
        .filter_target("vt100".to_owned(), LevelFilter::Warn)
        .install()
        .unwrap();

    m.add_class::<FnugCore>()?;
    m.add_class::<Auto>()?;
    m.add_class::<Command>()?;
    m.add_class::<CommandGroup>()?;
    m.add_class::<WatcherIterator>()?;
    m.add_class::<Process>()?;
    m.add_class::<OutputIterator>()?;
    m.add_class::<Output>()?;

    Ok(())
}

#[cfg(feature = "stub_gen")]
pub mod stub_gen {
    use pyo3_stub_gen::StubInfo;

    pub fn stub_info() -> StubInfo {
        let manifest_dir: &::std::path::Path = env!("CARGO_MANIFEST_DIR").as_ref();
        StubInfo::from_pyproject_toml(manifest_dir.parent().unwrap().join("pyproject.toml"))
            .unwrap()
    }
}
