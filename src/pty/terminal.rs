use std::io::{Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::spawn;

use log::{debug, error};
use parking_lot::Mutex;
use portable_pty::{
    Child, ChildKiller, CommandBuilder, ExitStatus, MasterPty, PtySize, native_pty_system,
};

use crate::commands::command::Command;

const DEFAULT_SCROLLBACK_SIZE: usize = 3500;
/// Updates applied per parser lock acquisition before yielding it to the renderer
const MAX_UPDATES_PER_LOCK: usize = 64;

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

/// PTY dimensions in columns and rows
#[derive(Debug, Clone, Copy)]
pub struct TerminalSize {
    cols: u16,
    rows: u16,
}

impl TerminalSize {
    #[must_use]
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

type SpawnedPty = (Box<dyn Child + Send + Sync>, Box<dyn MasterPty + Send>);

fn spawn_pty(command: &Command, size: TerminalSize) -> Result<SpawnedPty, ProcessError> {
    debug!("Running PTY for command: {command:?}");

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

#[derive(Debug)]
enum TerminalUpdate {
    Process(Vec<u8>),
    Resize(TerminalSize),
    Echo(Vec<u8>),
    Scroll(isize),
    SetScroll(usize),
    Clear,
}

/// Spawn a thread to process terminal output and set a dirty flag
fn spawn_output_writer(
    parser: Arc<Mutex<vt100::Parser>>,
    dirty: Arc<AtomicBool>,
) -> crossbeam_channel::Sender<TerminalUpdate> {
    let (update_tx, terminal_rx) = crossbeam_channel::bounded(1000);

    spawn(move || {
        while let Ok(update) = terminal_rx.recv() {
            let mut parser = parser.lock();
            apply_update(&mut parser, update);
            // Bounded batch: an unbounded drain holds the lock for as long as output floods in
            for update in terminal_rx.try_iter().take(MAX_UPDATES_PER_LOCK - 1) {
                apply_update(&mut parser, update);
            }
            drop(parser);
            dirty.store(true, Ordering::Release);
        }
        debug!("Terminal update channel closed (process exited)");
    });

    update_tx
}

fn apply_update(parser: &mut vt100::Parser, update: TerminalUpdate) {
    match update {
        TerminalUpdate::Process(bytes) => {
            parser.process(&bytes);
        }
        TerminalUpdate::Resize(size) => {
            parser.set_size(size.rows, size.cols);
        }
        TerminalUpdate::Scroll(delta) => {
            let pos = parser.screen().scrollback();
            let new_pos = pos.saturating_add_signed(-delta);
            if pos != new_pos {
                parser.set_scrollback(new_pos);
            }
        }
        TerminalUpdate::SetScroll(rows) => {
            parser.set_scrollback(rows);
        }
        TerminalUpdate::Echo(text) => {
            parser.process(text.as_slice());
        }
        TerminalUpdate::Clear => {
            parser.clear();
        }
    }
}

/// Spawn a thread to read from the PTY and send output to the update channel
///
/// Returns a channel to receive the process exit status. `reaped` is set once the
/// process has been waited on, before its status is published.
fn spawn_pty_reader(
    mut reader: Box<dyn Read + Send>,
    mut process: Box<dyn Child + Send + Sync>,
    update_tx: crossbeam_channel::Sender<TerminalUpdate>,
    reaped: Arc<AtomicBool>,
) -> crossbeam_channel::Receiver<ExitStatus> {
    let (status_tx, status_rx) = crossbeam_channel::bounded(1);

    spawn(move || {
        loop {
            let mut buf = [0u8; 1024];
            match reader.read(&mut buf) {
                Ok(0) => {
                    debug!("PTY reader EOF");
                    break;
                }
                Ok(n) => {
                    if update_tx
                        .send(TerminalUpdate::Process(buf[..n].to_vec()))
                        .is_err()
                    {
                        debug!("PTY reader: terminal update channel closed");
                        break;
                    }
                }
                Err(e) => {
                    error!("PTY reader thread error: {e:?}");
                    break;
                }
            }
        }

        // Wait for the process to exit
        let result = process.wait();
        // The pid may be reused from here on, so it must never be signalled again
        reaped.store(true, Ordering::Release);
        match result {
            Ok(status) => {
                let _ = status_tx.send(status);
            }
            Err(e) => {
                error!("Failed to wait for process: {e:?}");
            }
        }
    });

    status_rx
}

fn spawn_pty_writer(
    mut writer: Box<dyn Write + Send>,
    master: Box<dyn MasterPty + Send>,
    mut killer: Box<dyn ChildKiller + Send + Sync>,
    reaped: Arc<AtomicBool>,
) -> crossbeam_channel::Sender<PtyUpdate> {
    let (pty_tx, pty_rx) = crossbeam_channel::bounded(1000);

    spawn(move || {
        loop {
            match pty_rx.recv() {
                Ok(PtyUpdate::MouseClick(x, y)) => {
                    if write!(writer, "\x1b[<0;{};{}M", x + 1, y + 1).is_err() {
                        break;
                    }
                    if write!(writer, "\x1b[<0;{};{}m", x + 1, y + 1).is_err() {
                        break;
                    }
                }
                Ok(PtyUpdate::MouseScroll { up, x, y }) => {
                    // SGR mouse encoding: button 64 = scroll up, 65 = scroll down
                    let button = if up { 64 } else { 65 };
                    if write!(writer, "\x1b[<{button};{};{}M", x + 1, y + 1).is_err() {
                        break;
                    }
                }
                Ok(PtyUpdate::Resize(size)) => {
                    if let Err(e) = master.resize(size.into()) {
                        error!("Failed to resize PTY: {e:?}");
                    }
                }
                Ok(PtyUpdate::Write(input)) => {
                    if let Err(e) = writer.write_all(&input) {
                        error!("Failed to write to PTY: {e:?}");
                    }
                }
                Ok(PtyUpdate::KillProcess) => {
                    if reaped.load(Ordering::Acquire) {
                        debug!("Process already exited, not killing");
                    } else {
                        debug!("Killing process");
                        killer
                            .kill()
                            .unwrap_or_else(|e| debug!("Failed to kill process: {e:?}"));
                    }
                }
                Err(_) => {
                    debug!("PTY writer thread EOF");
                    break;
                }
            }
        }
    });

    pty_tx
}

/// Manages a command running in a pseudo-terminal
pub struct Terminal {
    update_tx: crossbeam_channel::Sender<TerminalUpdate>,
    pty_tx: crossbeam_channel::Sender<PtyUpdate>,
    status_rx: crossbeam_channel::Receiver<ExitStatus>,
    parser: Arc<Mutex<vt100::Parser>>,
    dirty: Arc<AtomicBool>,
    reaped: Arc<AtomicBool>,
}

#[derive(Debug)]
enum PtyUpdate {
    MouseClick(u16, u16),
    MouseScroll { up: bool, x: u16, y: u16 },
    Resize(TerminalSize),
    Write(Vec<u8>),
    KillProcess,
}

impl Terminal {
    /// Spawn a new command in a PTY of the given size.
    ///
    /// # Errors
    ///
    /// Returns `ProcessError::PtyError` if the PTY cannot be opened, or
    /// `ProcessError::Process` if the command fails to spawn.
    pub fn new(
        command: &Command,
        size: TerminalSize,
        scrollback_size: usize,
    ) -> Result<Self, ProcessError> {
        let (process, master) = spawn_pty(command, size)?;
        let reader = master
            .try_clone_reader()
            .map_err(|e| ProcessError::PtyError(format!("Failed to clone PTY reader: {e}")))?;
        let killer = process.clone_killer();
        let writer = master
            .take_writer()
            .map_err(|e| ProcessError::PtyError(format!("Failed to take PTY writer: {e}")))?;

        let parser = Arc::new(Mutex::new(vt100::Parser::new(
            size.rows,
            size.cols,
            scrollback_size,
        )));

        let dirty = Arc::new(AtomicBool::new(false));
        let reaped = Arc::new(AtomicBool::new(false));
        let update_tx = spawn_output_writer(Arc::clone(&parser), Arc::clone(&dirty));
        let status_rx = spawn_pty_reader(reader, process, update_tx.clone(), Arc::clone(&reaped));
        let pty_tx = spawn_pty_writer(writer, master, killer, Arc::clone(&reaped));

        Ok(Self {
            update_tx,
            pty_tx,
            status_rx,
            parser,
            dirty,
            reaped,
        })
    }

    /// Returns the default scrollback size
    #[must_use]
    pub fn default_scrollback_size() -> usize {
        DEFAULT_SCROLLBACK_SIZE
    }

    /// Access the vt100 parser (for rendering with tui-term)
    #[must_use]
    pub fn parser(&self) -> &Arc<Mutex<vt100::Parser>> {
        &self.parser
    }

    /// Check if the terminal has new output since last clear
    #[must_use]
    pub fn is_dirty(&self) -> bool {
        self.dirty.load(Ordering::Acquire)
    }

    /// Clear the dirty flag (call after rendering)
    pub fn clear_dirty(&self) {
        self.dirty.store(false, Ordering::Release);
    }

    fn send_terminal(&self, update: TerminalUpdate) -> Result<(), ProcessError> {
        self.update_tx
            .send(update)
            .map_err(|_| ProcessError::UpdateChannelDisconnected)
    }

    fn send_pty(&self, update: PtyUpdate) -> Result<(), ProcessError> {
        self.pty_tx
            .send(update)
            .map_err(|_| ProcessError::WriterDisconnected)
    }

    /// Resize the terminal.
    ///
    /// # Errors
    ///
    /// Returns `ProcessError` if the update or PTY channel is disconnected.
    pub fn resize(&self, size: TerminalSize) -> Result<(), ProcessError> {
        self.send_terminal(TerminalUpdate::Resize(size))?;
        // Run writer update last, as it may fail if the process has already exited
        self.send_pty(PtyUpdate::Resize(size))
    }

    /// Scroll the terminal output by a number of lines.
    ///
    /// # Errors
    ///
    /// Returns `ProcessError::UpdateChannelDisconnected` if the channel is closed.
    pub fn scroll(&self, delta: isize) -> Result<(), ProcessError> {
        self.send_terminal(TerminalUpdate::Scroll(delta))
    }

    /// Set the scrollback position.
    ///
    /// # Errors
    ///
    /// Returns `ProcessError::UpdateChannelDisconnected` if the channel is closed.
    pub fn set_scroll(&self, rows: usize) -> Result<(), ProcessError> {
        self.send_terminal(TerminalUpdate::SetScroll(rows))
    }

    /// Send a mouse click event to the terminal.
    ///
    /// # Errors
    ///
    /// Returns `ProcessError::WriterDisconnected` if the PTY channel is closed.
    pub fn click(&self, x: u16, y: u16) -> Result<(), ProcessError> {
        // If the terminal is not in mouse protocol mode, ignore the click
        if self.parser.lock().screen().mouse_protocol_mode() == vt100::MouseProtocolMode::None {
            return Ok(());
        }

        self.send_pty(PtyUpdate::MouseClick(x, y))
    }

    /// Send a mouse scroll event to the terminal.
    /// Returns `true` if the event was forwarded (mouse protocol active), `false` otherwise.
    ///
    /// # Errors
    ///
    /// Returns `ProcessError::WriterDisconnected` if the PTY channel is closed.
    pub fn mouse_scroll(&self, up: bool, x: u16, y: u16) -> Result<bool, ProcessError> {
        if self.parser.lock().screen().mouse_protocol_mode() == vt100::MouseProtocolMode::None {
            return Ok(false);
        }

        self.send_pty(PtyUpdate::MouseScroll { up, x, y })?;
        Ok(true)
    }

    /// Wait for the process to exit, returning the exit code.
    ///
    /// # Errors
    ///
    /// Returns `ProcessError::Process` if the status channel closes or the join fails.
    pub async fn wait(&self) -> Result<u32, ProcessError> {
        let rx = self.status_rx.clone();
        let status = tokio::task::spawn_blocking(move || rx.recv())
            .await
            .map_err(|e| ProcessError::Process(format!("Task join error: {e}")))?
            .map_err(|_| ProcessError::Process("Process status channel closed".into()))?;
        Ok(status.exit_code())
    }

    /// Whether the process has exited and been reaped.
    pub(crate) fn has_exited(&self) -> bool {
        self.reaped.load(Ordering::Acquire)
    }

    /// Kill the process running in the terminal. Does nothing once it has exited.
    ///
    /// # Errors
    ///
    /// Returns `ProcessError::WriterDisconnected` if the PTY channel is closed.
    pub fn kill(&self) -> Result<(), ProcessError> {
        if self.has_exited() {
            return Ok(());
        }
        self.send_pty(PtyUpdate::KillProcess)
    }

    /// Write text to the terminal.
    ///
    /// # Errors
    ///
    /// Returns `ProcessError::UpdateChannelDisconnected` if the channel is closed.
    pub fn echo(&self, text: Vec<u8>) -> Result<(), ProcessError> {
        self.send_terminal(TerminalUpdate::Echo(text))
    }

    /// Clear the terminal.
    ///
    /// # Errors
    ///
    /// Returns `ProcessError::UpdateChannelDisconnected` if the channel is closed.
    pub fn clear(&self) -> Result<(), ProcessError> {
        self.send_terminal(TerminalUpdate::Clear)
    }

    /// Write bytes to stdin of the process.
    ///
    /// # Errors
    ///
    /// Returns `ProcessError::WriterDisconnected` if the PTY channel is closed.
    pub fn write(&self, input: Vec<u8>) -> Result<(), ProcessError> {
        self.send_pty(PtyUpdate::Write(input))
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::{Duration, Instant};

    use portable_pty::{PtySize, native_pty_system};

    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use parking_lot::Mutex;

    use super::{Terminal, TerminalSize, TerminalUpdate, spawn_output_writer};
    use crate::commands::command::Command;

    fn pty_available() -> bool {
        let ok = native_pty_system().openpty(PtySize::default()).is_ok();
        if !ok {
            eprintln!("skipping: no PTY available");
        }
        ok
    }

    /// Kills the command when dropped, so a failed assertion doesn't leak it.
    struct Spawned(Terminal);

    impl Drop for Spawned {
        fn drop(&mut self) {
            let _ = self.0.kill();
        }
    }

    fn spawn(cmd: &str, cwd: &Path) -> Spawned {
        let command = Command {
            id: "t".into(),
            name: "t".into(),
            cmd: cmd.into(),
            cwd: cwd.to_path_buf(),
            ..Default::default()
        };
        let size = TerminalSize::new(80, 24);
        Spawned(Terminal::new(&command, size, Terminal::default_scrollback_size()).unwrap())
    }

    fn wait_until(timeout: Duration, mut cond: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if cond() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        cond()
    }

    #[test]
    fn parser_lock_available_during_flood() {
        if !pty_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let term = spawn("touch ready && exec yes", dir.path());
        let started = wait_until(Duration::from_secs(5), || dir.path().join("ready").exists());
        assert!(started, "command did not start");

        let parser = term.0.parser();
        let start = Instant::now();
        let mut acquired = 0;
        let mut saw_output = false;
        // Keep contending long enough for the output queue to fill up behind the parser
        while acquired < 5 || !saw_output || start.elapsed() < Duration::from_millis(500) {
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "no output from `yes`"
            );
            let Some(guard) = parser.try_lock_for(Duration::from_millis(500)) else {
                panic!("parser lock starved after {acquired} acquisitions");
            };
            saw_output |= guard.screen().contents().contains('y');
            acquired += 1;
        }
    }

    #[test]
    fn output_writer_releases_lock_between_batches() {
        let parser = Arc::new(Mutex::new(vt100::Parser::new(24, 80, 0)));
        let dirty = Arc::new(AtomicBool::new(false));
        let tx = spawn_output_writer(Arc::clone(&parser), Arc::clone(&dirty));

        // Queue a full channel of output while the writer is blocked on the lock
        let guard = parser.lock();
        for _ in 0..1000 {
            tx.send(TerminalUpdate::Process(b"y\r\n".repeat(2730)))
                .unwrap();
        }
        drop(guard);

        while !dirty.load(Ordering::Acquire) {
            std::hint::spin_loop();
        }
        // An unbounded drain only publishes once the queue is empty
        assert!(!tx.is_empty(), "writer drained everything before yielding");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn has_exited_once_wait_returns() {
        if !pty_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let term = spawn("true", dir.path());

        assert_eq!(term.0.wait().await.unwrap(), 0);
        // Set before the exit status is published, so later kills are always skipped
        assert!(term.0.has_exited());
        term.0.kill().unwrap();
    }

    #[test]
    fn kill_stops_running_command() {
        if !pty_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let term = spawn("touch ready && exec sleep 30", dir.path());
        let started = wait_until(Duration::from_secs(5), || dir.path().join("ready").exists());
        assert!(started, "command did not start");
        assert!(!term.0.has_exited());

        term.0.kill().unwrap();

        let exited = wait_until(Duration::from_secs(5), || term.0.has_exited());
        assert!(exited, "command still running after kill");
    }
}
