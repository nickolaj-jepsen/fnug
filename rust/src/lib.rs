use crate::command_group::{build_command_group, Auto, Command, CommandGroup};
use crate::config_file::ConfigError;
use crate::git::{commands_with_changes, GitError};
use config_file::Config;
use pyo3::exceptions::{PyFileNotFoundError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::PyType;
use pyo3_stub_gen::derive::{gen_stub_pyclass, gen_stub_pymethods};
use pyo3_stub_gen::StubInfo;
use std::path::PathBuf;

mod command_group;
mod config_file;
mod git;

#[gen_stub_pyclass]
#[pyclass]
struct FnugCore {
    #[pyo3(get)]
    config: CommandGroup,
    cwd: PathBuf,
}

#[gen_stub_pymethods]
#[pymethods]
impl FnugCore {
    #[new]
    #[pyo3(signature = (command_group, cwd))]
    fn new(command_group: CommandGroup, cwd: PathBuf) -> Self {
        FnugCore {
            config: command_group,
            cwd,
        }
    }

    #[classmethod]
    #[pyo3(signature = (config_file=None))]
    fn from_config_file(_cls: &Bound<'_, PyType>, config_file: Option<&str>) -> PyResult<Self> {
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
        let config = match Config::from_file(&config_path) {
            Ok(config) => config,
            Err(ConfigError::Io(err)) => {
                return Err(PyFileNotFoundError::new_err(format!(
                    "Error reading config file: {:?}",
                    err
                )))
            }
            Err(ConfigError::Serde(err)) => {
                return Err(PyValueError::new_err(format!(
                    "Error parsing config file: {:?}",
                    err
                )))
            }
        };

        let cwd = config_path.parent().unwrap().to_path_buf();
        let config = build_command_group(config.root, &cwd, None);

        Ok(FnugCore::new(config, cwd))
    }

    #[getter]
    fn get_cwd(&self, py: Python<'_>) -> PyResult<PyObject> {
        let pathlib = py.import_bound("pathlib")?;
        let path = pathlib.getattr("Path")?;
        let obj = path.call1((self.cwd.to_string_lossy(),))?;
        let resolved = obj.call_method0("resolve")?;
        Ok(resolved.into())
    }

    fn all_commands(&self) -> Vec<Command> {
        self.config.all_commands().into_iter().cloned().collect()
    }

    fn commands_with_git_changes(&self) -> PyResult<Vec<Command>> {
        let commands = self.config.all_commands();
        match commands_with_changes(commands) {
            Ok(commands) => Ok(commands.into_iter().cloned().collect()),
            Err(GitError::NoGitRepo(path)) => Err(PyValueError::new_err(format!(
                "No git repository found for path: {:?}",
                path
            ))),
            Err(GitError::Regex(err)) => Err(PyValueError::new_err(format!(
                "Error parsing regex: {:?}",
                err
            ))),
        }
    }
}

#[pymodule]
#[pyo3(name = "core")]
fn main(m: &Bound<PyModule>) -> PyResult<()> {
    m.add_class::<FnugCore>()?;
    m.add_class::<Auto>()?;
    m.add_class::<Command>()?;
    m.add_class::<CommandGroup>()?;

    Ok(())
}

pub fn stub_info() -> StubInfo {
    let manifest_dir: &::std::path::Path = env!("CARGO_MANIFEST_DIR").as_ref();
    StubInfo::from_pyproject_toml(manifest_dir.parent().unwrap().join("pyproject.toml")).unwrap()
}
