use crate::command_group::{build_command_group, CommandGroup};
use crate::config_file::ConfigError;
use config_file::Config;
use pyo3::exceptions::{PyFileNotFoundError, PyValueError};
use pyo3::prelude::*;
use pyo3_stub_gen::derive::{gen_stub_pyclass, gen_stub_pymethods};
use pyo3_stub_gen::StubInfo;
use std::path::PathBuf;

mod command_group;
mod config_file;

#[gen_stub_pyclass]
#[pyclass]
struct FnugCore {
    #[pyo3(get)]
    config: CommandGroup,
    #[pyo3(get)]
    cwd: PathBuf,
}

#[gen_stub_pymethods]
#[pymethods]
impl FnugCore {
    #[new]
    fn new(config_file: Option<&str>) -> PyResult<Self> {
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
        let command_group = build_command_group(config.root, &cwd, None);

        Ok(FnugCore {
            config: command_group,
            cwd,
        })
    }
}

#[pymodule]
#[pyo3(name = "core")]
fn main(m: &Bound<PyModule>) -> PyResult<()> {
    m.add_class::<FnugCore>()?;
    Ok(())
}

pub fn stub_info() -> StubInfo {
    let manifest_dir: &::std::path::Path = env!("CARGO_MANIFEST_DIR").as_ref();
    StubInfo::from_pyproject_toml(manifest_dir.parent().unwrap().join("pyproject.toml")).unwrap()
}
