use crate::commands::auto::Auto;
use pyo3::{pyclass, pymethods};
use std::path::PathBuf;
use uuid::Uuid;

/// A single executable task with its configuration and automation rules
///
/// Commands are the leaf nodes in the command tree. Each command has:
/// - A unique identifier
/// - A working directory (inherited from parent group if not specified)
/// - Automation rules (merged with parent group rules)
/// - An executable shell command
///
/// # Examples
///
/// ```python
/// cmd = Command(
///     name="build",
///     cmd="cargo build",
/// )
/// ```
#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "stub_gen", pyo3_stub_gen::derive::gen_stub_pyclass)]
#[pyclass]
#[pyo3(get_all)]
pub struct Command {
    pub id: String,
    pub name: String,
    pub cmd: String,
    pub cwd: PathBuf,
    pub interactive: bool,
    pub auto: Auto,
}

#[cfg_attr(feature = "stub_gen", pyo3_stub_gen::derive::gen_stub_pymethods)]
#[pymethods]
impl Command {
    #[new]
    #[pyo3(signature = (name, cmd, id = Uuid::new_v4().to_string(), cwd = PathBuf::new(), interactive = false, auto = Auto::default()))]
    pub fn new(
        name: String,
        cmd: String,
        id: String,
        cwd: PathBuf,
        interactive: bool,
        auto: Auto,
    ) -> Self {
        Command {
            id,
            name,
            cmd,
            cwd,
            interactive,
            auto,
        }
    }
    fn __eq__(&self, other: &Self) -> bool {
        self.id == other.id
    }
}
