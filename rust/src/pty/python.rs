use crate::commands::command::Command;
use crate::pty::messages::{format_failure_message, format_start_message, format_success_message};
use crate::pty::terminal::{ProcessError, Terminal, TerminalSize};
use log::{debug, error};
use pyo3::exceptions::{PyStopAsyncIteration, PyValueError};
use pyo3::{pyclass, pymethods, Bound, Py, PyAny, PyRef, PyResult, Python};
use std::sync::Arc;
use tokio::sync::{watch, Mutex};

#[derive(Default, Clone)]
#[pyclass]
pub struct Output {
    #[pyo3(get)]
    pub screen: Vec<String>,
    #[pyo3(get)]
    pub scrollback_position: usize,
    #[pyo3(get)]
    pub scrollback_size: usize,
}

#[cfg_attr(feature = "stub_gen", pyo3_stub_gen::derive::gen_stub_pyclass)]
#[pyclass]
pub struct OutputIterator {
    rx: Arc<Mutex<watch::Receiver<Output>>>,
}

#[cfg_attr(feature = "stub_gen", pyo3_stub_gen::derive::gen_stub_pymethods)]
#[pymethods]
impl OutputIterator {
    fn __aiter__(slf: PyRef<Self>) -> PyRef<Self> {
        if let Ok(mut rx) = slf.rx.try_lock() {
            rx.mark_changed();
        }
        slf
    }

    fn __anext__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let rx = self.rx.clone();
        let promise = py.allow_threads(|| async move {
            let mut rx = rx.lock().await;
            rx.changed().await.map_err(|e| {
                error!("Error receiving output: {:?}", e);
                PyStopAsyncIteration::new_err("End of output")
            })?;
            let data = rx.borrow_and_update().clone();
            Ok(data)
        });

        pyo3_async_runtimes::tokio::future_into_py(py, promise)
    }
}

#[cfg_attr(feature = "stub_gen", pyo3_stub_gen::derive::gen_stub_pyclass)]
#[pyclass]
#[pyo3(name = "Process")]
pub struct Process {
    #[pyo3(get)]
    output: Py<OutputIterator>,
    terminal: Arc<Terminal>,
    command: Command,
}

#[cfg_attr(feature = "stub_gen", pyo3_stub_gen::derive::gen_stub_pymethods)]
#[pymethods]
impl Process {
    #[new]
    fn new(command: Command, width: u16, height: u16, py: Python<'_>) -> PyResult<Self> {
        debug!("Creating new process: {:?}", command);
        let (output_tx, output_rx) = watch::channel(Output::default());

        let start_message = format_start_message(&command.cmd);
        let term_cmd = &command;
        let terminal = py.allow_threads(move || {
            Terminal::new(term_cmd, TerminalSize::new(width, height), output_tx)
                .map_err(|e| PyValueError::new_err(format!("Terminal setup failed: {:?}", e)))
        })?;
        terminal.echo(start_message).unwrap();

        Ok(Self {
            terminal: Arc::new(terminal),
            output: Py::new(
                py,
                OutputIterator {
                    rx: Arc::new(Mutex::new(output_rx)),
                },
            )?,
            command,
        })
    }

    fn status<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let terminal = self.terminal.clone();
        let promise = py.allow_threads(|| async move {
            match terminal.wait().await {
                Ok(0) => {
                    terminal.echo(format_success_message()).unwrap();
                    Ok(0)
                }
                Ok(status) => {
                    terminal.echo(format_failure_message(status)).unwrap();
                    Ok(status)
                }
                Err(e) => {
                    error!("Error waiting for process: {:?}", e);
                    Err(PyValueError::new_err(format!(
                        "Error waiting for process: {:?}",
                        e
                    )))
                }
            }
        });

        pyo3_async_runtimes::tokio::future_into_py(py, promise)
    }

    pub fn kill(&self) -> PyResult<()> {
        // Ignore errors here, as the process may have already exited
        self.terminal.kill().unwrap_or(());
        Ok(())
    }

    pub fn scroll(&self, lines: isize) -> PyResult<()> {
        self.terminal
            .scroll(lines)
            .map_err(|e| PyValueError::new_err(format!("Error scrolling terminal: {:?}", e)))?;
        Ok(())
    }

    pub fn set_scroll(&self, rows: usize) -> PyResult<()> {
        self.terminal
            .set_scroll(rows)
            .map_err(|e| PyValueError::new_err(format!("Error setting scrollback: {:?}", e)))?;
        Ok(())
    }

    pub fn resize(&self, width: u16, height: u16) -> PyResult<()> {
        match self.terminal.resize(TerminalSize::new(width, height)) {
            Ok(_) => Ok(()),
            // Ignore write errors, as they usually mean the process is dead
            Err(ProcessError::WriterDisconnected) => Ok(()),
            Err(e) => Err(PyValueError::new_err(format!(
                "Error resizing terminal: {:?}",
                e
            ))),
        }
    }

    pub fn click(&self, x: u16, y: u16) -> PyResult<()> {
        // Ignore errors here, as the process may have already exited
        self.terminal.click(x, y).unwrap_or(());
        Ok(())
    }

    pub fn clear(&self) -> PyResult<()> {
        self.terminal.clear().unwrap();
        Ok(())
    }

    pub fn write(&self, data: Vec<u8>) -> PyResult<()> {
        self.terminal
            .write(data)
            .map_err(|e| PyValueError::new_err(format!("Error writing to terminal: {:?}", e)))?;
        Ok(())
    }

    #[getter]
    pub fn can_focus(&self) -> bool {
        self.command.interactive
    }
}
