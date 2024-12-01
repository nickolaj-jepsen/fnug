use crate::commands::command::Command;
use crate::pty::python::Output;
use log::{debug, error};
use parking_lot::Mutex;
use portable_pty::{
    native_pty_system, Child, ChildKiller, CommandBuilder, ExitStatus, MasterPty, PtySize,
};
use std::fmt::Debug;
use std::io::{Read, Write};
use std::sync::Arc;
use std::thread::spawn;
use tokio::sync::watch;

const SCROLLBACK_SIZE: usize = 3500;

#[derive(Debug, thiserror::Error)]
pub enum ProcessError {
    #[error("Writer disconnected")]
    WriterDisconnected,
    #[error("Update channel disconnected")]
    UpdateChannelDisconnected,
    #[error("Unable to open PTY: {0}")]
    PtyError(String),
    #[error("Process error: {0}")]
    Process(String),
}

#[derive(Debug, Clone)]
pub struct TerminalSize {
    cols: u16,
    rows: u16,
}

impl TerminalSize {
    pub fn new(cols: u16, rows: u16) -> Self {
        Self { cols, rows }
    }
}

impl From<TerminalSize> for PtySize {
    fn from(size: TerminalSize) -> Self {
        Self {
            cols: size.cols,
            rows: size.rows,
            pixel_width: 0,
            pixel_height: 0,
        }
    }
}

fn spawn_pty(
    command: &Command,
    size: TerminalSize,
) -> Result<(Box<dyn Child + Send + Sync>, Box<dyn MasterPty + Send>), ProcessError> {
    debug!("Running PTY for command: {:?}", command);

    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(size.into())
        .map_err(|e| ProcessError::PtyError(e.to_string()))?;

    let child = pair
        .slave
        .spawn_command(CommandBuilder::from(command))
        .map_err(|e| ProcessError::Process(e.to_string()))?;

    drop(pair.slave); // This will make the reader close when the child process exits

    Ok((child, pair.master))
}

#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
enum TerminalUpdate {
    Process([u8; 1024]),
    Resize(TerminalSize),
    Echo(Vec<u8>),
    Scroll(isize),
    SetScroll(usize),
    Clear,
}

impl From<&vt100::Screen> for Output {
    fn from(screen: &vt100::Screen) -> Self {
        let width = screen.size().1;
        let contents = screen
            .rows_formatted(0, width)
            .map(|r| String::from_utf8_lossy(&r).to_string())
            .collect::<Vec<String>>();

        Self {
            screen: contents,
            scrollback_position: screen.scrollback(),
            scrollback_size: screen.scrollback_len(),
        }
    }
}

/// Spawn a thread to process terminal output and send it to the output channel
fn spawn_output_writer(
    parser: Arc<Mutex<vt100::Parser>>,
    out_chan: watch::Sender<Output>,
) -> Result<crossbeam_channel::Sender<TerminalUpdate>, ProcessError> {
    let (terminal_tx, terminal_rx) = crossbeam_channel::bounded(1000);

    spawn(move || loop {
        let res = terminal_rx.recv();
        let mut parser = parser.lock();
        match res {
            Ok(TerminalUpdate::Process(bytes)) => {
                parser.process(&bytes);
            }
            Ok(TerminalUpdate::Resize(size)) => {
                parser.set_size(size.rows, size.cols);
            }
            Ok(TerminalUpdate::Scroll(delta)) => {
                let pos = parser.screen().scrollback();
                let new_pos = pos.saturating_add_signed(-delta);

                if pos != new_pos {
                    parser.set_scrollback(new_pos);
                }
            }
            Ok(TerminalUpdate::SetScroll(rows)) => {
                parser.set_scrollback(rows);
            }
            Ok(TerminalUpdate::Echo(text)) => {
                parser.process(text.as_slice());
            }
            Ok(TerminalUpdate::Clear) => {
                parser.clear();
            }
            Err(e) => {
                error!("Terminal update error: {:?}", e);
                break;
            }
        }

        // Send to output channel
        out_chan.send_replace(parser.screen().into());
    });

    Ok(terminal_tx)
}

/// Spawn a thread to read from the PTY and send output to the update channel
///
/// Returns a channel to receive the process exit status
fn spawn_pty_reader(
    mut reader: Box<dyn Read + Send>,
    mut process: Box<dyn Child + Send + Sync>,
    terminal_tx: crossbeam_channel::Sender<TerminalUpdate>,
) -> Result<crossbeam_channel::Receiver<ExitStatus>, ProcessError> {
    let (status_tx, status_rx) = crossbeam_channel::bounded(1);

    spawn(move || {
        loop {
            let mut buf = [0u8; 1024];
            match reader.read(&mut buf) {
                Ok(0) => {
                    debug!("PTY reader EOF");
                    break;
                }
                Ok(_) => {
                    terminal_tx.send(TerminalUpdate::Process(buf)).unwrap();
                }
                Err(e) => {
                    error!("PTY reader thread error: {:?}", e);
                    break;
                }
            };
        }

        // Wait for the process to exit
        let status = process.wait().unwrap();
        status_tx.send(status).unwrap();
    });

    Ok(status_rx)
}

fn spawn_pty_writer(
    mut writer: Box<dyn Write + Send>,
    master: Box<dyn MasterPty + Send>,
    mut killer: Box<dyn ChildKiller + Send + Sync>,
) -> crossbeam_channel::Sender<PtyUpdate> {
    let (pty_tx, pty_rx) = crossbeam_channel::bounded(1000);

    spawn(move || loop {
        match pty_rx.recv() {
            Ok(PtyUpdate::MouseClick(x, y)) => {
                write!(writer, "\x1b[<0;{};{}M", x + 1, y + 1).unwrap();
                write!(writer, "\x1b[<0;{};{}m", x + 1, y + 1).unwrap();
            }
            Ok(PtyUpdate::Resize(size)) => {
                master.resize(size.into()).unwrap();
            }
            Ok(PtyUpdate::Write(input)) => {
                writer.write_all(&input).unwrap();
            }
            Ok(PtyUpdate::KillProcess) => {
                debug!("Killing process");
                killer
                    .kill()
                    .unwrap_or_else(|e| debug!("Failed to kill process: {:?}", e));
            }
            Err(_) => {
                debug!("PTY writer thread EOF");
                break;
            }
        }
    });

    pty_tx
}

pub struct Terminal {
    terminal_tx: crossbeam_channel::Sender<TerminalUpdate>,
    pty_tx: crossbeam_channel::Sender<PtyUpdate>,
    status_rx: crossbeam_channel::Receiver<ExitStatus>,
    parser: Arc<Mutex<vt100::Parser>>,
}

#[derive(Debug)]
enum PtyUpdate {
    MouseClick(u16, u16),
    Resize(TerminalSize),
    Write(Vec<u8>),
    KillProcess,
}

impl Terminal {
    pub fn new(
        command: &Command,
        size: TerminalSize,
        out_chan: watch::Sender<Output>,
    ) -> Result<Self, ProcessError> {
        let (process, master) = spawn_pty(command, size.clone())?;
        let reader = master.try_clone_reader().unwrap();
        let killer = process.clone_killer();
        let writer = master.take_writer().unwrap();

        let parser = Arc::new(Mutex::new(vt100::Parser::new(
            size.rows,
            size.cols,
            SCROLLBACK_SIZE,
        )));

        let terminal_tx = spawn_output_writer(Arc::clone(&parser), out_chan)?;
        let status_rx = spawn_pty_reader(reader, process, terminal_tx.clone())?;
        let pty_tx = spawn_pty_writer(writer, master, killer);

        Ok(Self {
            terminal_tx,
            status_rx,
            pty_tx,
            parser,
        })
    }

    /// Resize the terminal
    ///
    /// `size` is the new size of the terminal
    ///
    /// Returns an error if the update channel is disconnected, which usually means the process has
    /// exited
    pub fn resize(&self, size: TerminalSize) -> Result<(), ProcessError> {
        self.terminal_tx
            .send(TerminalUpdate::Resize(size.clone()))
            .map_err(|_| ProcessError::UpdateChannelDisconnected)?;
        // Run writer update last, as it may fail if the process has already exited
        self.pty_tx
            .send(PtyUpdate::Resize(size))
            .map_err(|_| ProcessError::WriterDisconnected)
    }

    /// Scroll the terminal output by a number of lines
    ///
    /// `delta` is the number of lines to scroll. Positive values scroll up, negative values scroll
    ///
    /// Returns an error if the update channel is disconnected, which usually means the process has
    /// exited
    pub fn scroll(&self, delta: isize) -> Result<(), ProcessError> {
        self.terminal_tx
            .send(TerminalUpdate::Scroll(delta))
            .map_err(|_| ProcessError::UpdateChannelDisconnected)
    }

    /// Set the scrollback position
    pub fn set_scroll(&self, rows: usize) -> Result<(), ProcessError> {
        self.terminal_tx
            .send(TerminalUpdate::SetScroll(rows))
            .map_err(|_| ProcessError::UpdateChannelDisconnected)
    }

    /// Send a mouse click event to the terminal
    ///
    /// Returns an error if the writer channel is disconnected, which usually means the process has
    /// exited
    pub fn click(&self, x: u16, y: u16) -> Result<(), ProcessError> {
        // If the terminal is not in mouse protocol mode, ignore the click
        if self.parser.lock().screen().mouse_protocol_mode() == vt100::MouseProtocolMode::None {
            return Ok(());
        }

        self.pty_tx
            .send(PtyUpdate::MouseClick(x, y))
            .map_err(|_| ProcessError::WriterDisconnected)
    }

    /// Wait for the process to exit, returning the exit code
    pub async fn wait(&self) -> Result<u32, ProcessError> {
        let status = self.status_rx.recv().unwrap();
        Ok(status.exit_code())
    }

    /// Kill the process running in the terminal
    ///
    /// Returns an error if the writer channel is disconnected, which usually means the process has
    /// already exited
    pub fn kill(&self) -> Result<(), ProcessError> {
        self.pty_tx
            .send(PtyUpdate::KillProcess)
            .map_err(|_| ProcessError::WriterDisconnected)
    }

    /// Write text to the terminal
    pub fn echo(&self, text: Vec<u8>) -> Result<(), ProcessError> {
        self.terminal_tx
            .send(TerminalUpdate::Echo(text))
            .map_err(|_| ProcessError::UpdateChannelDisconnected)
    }

    /// Clear the terminal
    pub fn clear(&self) -> Result<(), ProcessError> {
        self.terminal_tx
            .send(TerminalUpdate::Clear)
            .map_err(|_| ProcessError::UpdateChannelDisconnected)
    }

    /// Write bytes to stdin of the process
    pub fn write(&self, input: Vec<u8>) -> Result<(), ProcessError> {
        self.pty_tx
            .send(PtyUpdate::Write(input))
            .map_err(|_| ProcessError::WriterDisconnected)
    }
}
