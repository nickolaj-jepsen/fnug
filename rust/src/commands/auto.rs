use crate::config_file::parse_regexes;
use pyo3::{pyclass, pymethods, PyResult};
use regex_cache::LazyRegex;
use std::path::PathBuf;

/// Automation rules that determine when commands should execute
///
/// # Examples
///
/// ```python
/// # Watch for git changes in specific paths matching regex patterns
/// auto = Auto(
///     watch=True,
///     git=True,
///     path=["src/", "tests/"],
///     regex=[".*\\.rs$", ".*\\.toml$"]
/// )
/// ```
#[derive(Default, Debug, Clone)]
#[cfg_attr(feature = "stub_gen", pyo3_stub_gen::derive::gen_stub_pyclass)]
#[pyclass]
pub struct Auto {
    pub watch: Option<bool>,
    pub git: Option<bool>,
    pub path: Vec<PathBuf>,
    pub regex: Vec<LazyRegex>,
    pub always: Option<bool>,
}

#[cfg_attr(feature = "stub_gen", pyo3_stub_gen::derive::gen_stub_pymethods)]
#[pymethods]
impl Auto {
    #[new]
    #[pyo3(signature = (watch = None, git = None, path = Vec::new(), regex = Vec::new(), always = None))]
    pub fn new(
        watch: Option<bool>,
        git: Option<bool>,
        path: Vec<PathBuf>,
        regex: Vec<String>,
        always: Option<bool>,
    ) -> PyResult<Self> {
        Ok(Auto {
            watch,
            git,
            path,
            always,
            regex: parse_regexes(regex)?,
        })
    }

    #[getter]
    fn watch(&self) -> bool {
        self.watch.unwrap_or(false)
    }

    #[getter]
    fn git(&self) -> bool {
        self.git.unwrap_or(false)
    }

    #[getter]
    fn path(&self) -> Vec<PathBuf> {
        self.path.clone()
    }

    #[getter]
    fn regex(&self) -> Vec<String> {
        self.regex.iter().map(|r| r.to_string()).collect()
    }

    #[getter]
    fn always(&self) -> bool {
        self.always.unwrap_or(false)
    }
}
