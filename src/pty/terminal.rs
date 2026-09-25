use std::io::{Read, Write};
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crossbeam_channel::RecvTimeoutError;
use log::{debug, error, warn};
use parking_lot::Mutex;
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use tokio::sync::{Notify, watch};

use crate::commands::command::Command;
use crate::process::{ExitInfo, ProcessHandle, StopSignal};

use super::messages::format_emulator_reset_message;

const DEFAULT_SCROLLBACK_SIZE: usize = 3500;
/// Updates applied per parser lock acquisition before yielding it to the renderer
const MAX_UPDATES_PER_LOCK: usize = 64;
/// How long output may keep draining after the command exits before its exit is published
const DRAIN_DEADLINE: Duration = Duration::from_millis(200);
/// How much longer the PTY may stay open after `DRAIN_DEADLINE` before whatever still holds it
/// is warned about; under load, a plain command can miss the deadline too
const HOLDER_WARN_AFTER: Duration = Duration::from_secs(2);

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
    #[error("Working directory {} does not exist", .0.display())]
    MissingCwd(PathBuf),
    #[error("Input buffer full: the command is not reading its input")]
    InputBufferFull,
    #[error("Failed to signal process: {0}")]
    Signal(std::io::Error),
}

/// PTY dimensions in columns and rows
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

/// Options for [`Terminal::new`]
#[derive(Debug, Clone)]
pub struct TerminalOptions {
    /// Lines of scrollback kept by the parser
    pub scrollback: usize,
    /// Notified when output arrives while the terminal is not dirty
    pub output_notify: Option<Arc<Notify>>,
}

impl Default for TerminalOptions {
    fn default() -> Self {
        Self {
            scrollback: DEFAULT_SCROLLBACK_SIZE,
            output_notify: None,
        }
    }
}

struct SpawnedPty {
    child: Box<dyn Child + Send + Sync>,
    pid: u32,
    master: Box<dyn MasterPty + Send>,
    reader: Box<dyn Read + Send>,
    writer: Box<dyn Write + Send>,
}

fn spawn_pty(command: &Command, size: TerminalSize) -> Result<SpawnedPty, ProcessError> {
    debug!("Running PTY for command: {command:?}");
    if !command.cwd.is_dir() {
        return Err(ProcessError::MissingCwd(command.cwd.clone()));
    }

    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(size.into())
        .map_err(|e| ProcessError::PtyError(e.to_string()))?;
    // Fallible setup happens before the spawn, so a started child always gets a waiter
    let reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| ProcessError::PtyError(format!("Failed to clone PTY reader: {e}")))?;
    let writer = pair
        .master
        .take_writer()
        .map_err(|e| ProcessError::PtyError(format!("Failed to take PTY writer: {e}")))?;

    let child = pair
        .slave
        .spawn_command(CommandBuilder::from(command))
        .map_err(|e| ProcessError::Process(e.to_string()))?;

    drop(pair.slave); // This will make the reader close when the child process exits

    // Never None on unix, so this cannot strand a started child
    let pid = child
        .process_id()
        .ok_or_else(|| ProcessError::Process("Spawned process has no pid".into()))?;
    Ok(SpawnedPty {
        child,
        pid,
        master: pair.master,
        reader,
        writer,
    })
}

fn spawn_thread(name: &str, f: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .name(name.into())
        .spawn(f)
        .expect("failed to spawn PTY thread");
}

#[derive(Debug)]
enum TerminalUpdate {
    Process(Vec<u8>),
    Resize(TerminalSize),
    Echo(Vec<u8>),
    Scroll(isize),
    SetScroll(usize),
    Clear,
    #[cfg(test)]
    Panic,
}

/// Spawn a thread to process terminal output and set a dirty flag
fn spawn_output_writer(
    parser: Arc<Mutex<vt100::Parser>>,
    dirty: Arc<AtomicBool>,
    notify: Option<Arc<Notify>>,
    scrollback: usize,
) -> crossbeam_channel::Sender<TerminalUpdate> {
    let (update_tx, terminal_rx) = crossbeam_channel::bounded(1000);

    spawn_thread("fnug-pty-parse", move || {
        while let Ok(update) = terminal_rx.recv() {
            let mut parser = parser.lock();
            apply_update_guarded(&mut parser, update, scrollback);
            // Bounded batch: an unbounded drain holds the lock for as long as output floods in
            for update in terminal_rx.try_iter().take(MAX_UPDATES_PER_LOCK - 1) {
                apply_update_guarded(&mut parser, update, scrollback);
            }
            drop(parser);
            if !dirty.swap(true, Ordering::AcqRel)
                && let Some(notify) = &notify
            {
                notify.notify_one();
            }
        }
        debug!("Terminal update channel closed");
    });

    update_tx
}

/// Apply `update`; if the emulator panics, replace it with a blank one and carry on
fn apply_update_guarded(parser: &mut vt100::Parser, update: TerminalUpdate, scrollback: usize) {
    if std::panic::catch_unwind(AssertUnwindSafe(|| apply_update(parser, update))).is_ok() {
        return;
    }
    error!("Terminal emulator crashed; its screen was reset");
    let (rows, cols) = parser.screen().size();
    *parser = vt100::Parser::new(rows, cols, scrollback);
    parser.process(&format_emulator_reset_message());
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
        #[cfg(test)]
        TerminalUpdate::Panic => panic!("injected parser panic"),
    }
}

/// Spawn a thread that forwards PTY output to the parser; `done` is dropped at EOF
fn spawn_pty_reader(
    mut reader: Box<dyn Read + Send>,
    update_tx: crossbeam_channel::Sender<TerminalUpdate>,
    done: crossbeam_channel::Sender<()>,
) {
    spawn_thread("fnug-pty-read", move || {
        let _done = done;
        let mut buf = [0u8; 1024];
        loop {
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
    });
}

/// Spawn a thread that detects the exit, publishes it and reaps the child.
///
/// Exit is published once output has drained, or after `DRAIN_DEADLINE` if background processes
/// still hold the PTY. The child is reaped only after EOF: until then its zombie keeps the process
/// group id reserved, so stopping those background processes cannot signal a reused pid.
/// `reaped_tx` is set last, after both.
fn spawn_waiter(
    name: String,
    handle: ProcessHandle,
    mut child: Box<dyn Child + Send + Sync>,
    reader_done: crossbeam_channel::Receiver<()>,
    status_tx: watch::Sender<Option<ExitInfo>>,
    reaped_tx: watch::Sender<bool>,
) {
    spawn_thread("fnug-pty-wait", move || {
        let mut reap = || {
            if let Err(e) = handle.reap_with(|| child.wait()) {
                error!("Failed to reap '{name}': {e}");
            }
        };
        match handle.wait_exit() {
            Err(e) => {
                error!("Failed to wait for '{name}': {e}");
                reap();
            }
            Ok(exit) => {
                if matches!(
                    reader_done.recv_timeout(DRAIN_DEADLINE),
                    Err(RecvTimeoutError::Timeout)
                ) {
                    status_tx.send_replace(Some(exit));
                    if matches!(
                        reader_done.recv_timeout(HOLDER_WARN_AFTER),
                        Err(RecvTimeoutError::Timeout)
                    ) {
                        warn!("'{name}' exited but background processes still hold its terminal");
                        let _ = reader_done.recv();
                    }
                    reap();
                } else {
                    reap();
                    status_tx.send_replace(Some(exit));
                }
            }
        }
        reaped_tx.send_replace(true);
    });
}

/// Spawn a thread that writes input to the PTY; a child that stops reading only blocks this thread
fn spawn_pty_writer(mut writer: Box<dyn Write + Send>) -> crossbeam_channel::Sender<PtyInput> {
    let (pty_tx, pty_rx) = crossbeam_channel::bounded(1000);

    spawn_thread("fnug-pty-write", move || {
        loop {
            match pty_rx.recv() {
                Ok(PtyInput::MouseClick(x, y)) => {
                    if write!(writer, "\x1b[<0;{};{}M", x + 1, y + 1).is_err() {
                        break;
                    }
                    if write!(writer, "\x1b[<0;{};{}m", x + 1, y + 1).is_err() {
                        break;
                    }
                }
                Ok(PtyInput::MouseScroll { up, x, y }) => {
                    // SGR mouse encoding: button 64 = scroll up, 65 = scroll down
                    let button = if up { 64 } else { 65 };
                    if write!(writer, "\x1b[<{button};{};{}M", x + 1, y + 1).is_err() {
                        break;
                    }
                }
                Ok(PtyInput::Write(input)) => {
                    if let Err(e) = writer.write_all(&input) {
                        error!("Failed to write to PTY: {e:?}");
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
    pty_tx: crossbeam_channel::Sender<PtyInput>,
    status_rx: watch::Receiver<Option<ExitInfo>>,
    reaped_rx: watch::Receiver<bool>,
    handle: ProcessHandle,
    master: Mutex<Box<dyn MasterPty + Send>>,
    size: Mutex<TerminalSize>,
    parser: Arc<Mutex<vt100::Parser>>,
    dirty: Arc<AtomicBool>,
}

#[derive(Debug)]
enum PtyInput {
    MouseClick(u16, u16),
    MouseScroll { up: bool, x: u16, y: u16 },
    Write(Vec<u8>),
}

impl Terminal {
    /// Spawn a new command in a PTY of the given size.
    ///
    /// # Errors
    ///
    /// Returns `ProcessError::MissingCwd` if the command's working directory does not exist,
    /// `ProcessError::PtyError` if the PTY cannot be opened, or `ProcessError::Process` if the
    /// command fails to spawn.
    ///
    /// # Panics
    ///
    /// Panics if a worker thread cannot be spawned.
    pub fn new(
        command: &Command,
        size: TerminalSize,
        opts: TerminalOptions,
    ) -> Result<Self, ProcessError> {
        let SpawnedPty {
            child,
            pid,
            master,
            reader,
            writer,
        } = spawn_pty(command, size)?;
        // portable-pty calls setsid, so the child leads its own process group
        let handle = ProcessHandle::new(pid);

        let parser = Arc::new(Mutex::new(vt100::Parser::new(
            size.rows,
            size.cols,
            opts.scrollback,
        )));
        let dirty = Arc::new(AtomicBool::new(false));
        let (status_tx, status_rx) = watch::channel(None);
        let (reaped_tx, reaped_rx) = watch::channel(false);
        let (reader_done_tx, reader_done_rx) = crossbeam_channel::bounded(0);

        let update_tx = spawn_output_writer(
            Arc::clone(&parser),
            Arc::clone(&dirty),
            opts.output_notify,
            opts.scrollback,
        );
        spawn_pty_reader(reader, update_tx.clone(), reader_done_tx);
        spawn_waiter(
            command.name.clone(),
            handle.clone(),
            child,
            reader_done_rx,
            status_tx,
            reaped_tx,
        );
        let pty_tx = spawn_pty_writer(writer);

        Ok(Self {
            update_tx,
            pty_tx,
            status_rx,
            reaped_rx,
            handle,
            master: Mutex::new(master),
            size: Mutex::new(size),
            parser,
            dirty,
        })
    }

    /// Returns the default scrollback size
    #[must_use]
    pub fn default_scrollback_size() -> usize {
        DEFAULT_SCROLLBACK_SIZE
    }

    /// Process id of the command, which also leads its process group
    #[must_use]
    pub fn pid(&self) -> u32 {
        self.handle.pid()
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

    // Never blocks: a child that stops reading its input must not stall the caller's event loop
    fn send_pty(&self, input: PtyInput) -> Result<(), ProcessError> {
        self.pty_tx.try_send(input).map_err(|e| match e {
            crossbeam_channel::TrySendError::Full(_) => ProcessError::InputBufferFull,
            crossbeam_channel::TrySendError::Disconnected(_) => ProcessError::WriterDisconnected,
        })
    }

    /// Resize the parser and the PTY.
    ///
    /// # Errors
    ///
    /// Returns `ProcessError::UpdateChannelDisconnected` if the parser thread is gone, or
    /// `ProcessError::PtyError` if the PTY cannot be resized.
    pub fn resize(&self, size: TerminalSize) -> Result<(), ProcessError> {
        self.send_terminal(TerminalUpdate::Resize(size))?;
        self.master
            .lock()
            .resize(size.into())
            .map_err(|e| ProcessError::PtyError(format!("Failed to resize PTY: {e}")))?;
        *self.size.lock() = size;
        Ok(())
    }

    /// Last size applied to the PTY
    #[must_use]
    pub fn size(&self) -> TerminalSize {
        *self.size.lock()
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
    /// Returns `ProcessError::WriterDisconnected` if the PTY channel is closed, or
    /// `ProcessError::InputBufferFull` if the command is not reading its input.
    pub fn click(&self, x: u16, y: u16) -> Result<(), ProcessError> {
        // If the terminal is not in mouse protocol mode, ignore the click
        if self.parser.lock().screen().mouse_protocol_mode() == vt100::MouseProtocolMode::None {
            return Ok(());
        }

        self.send_pty(PtyInput::MouseClick(x, y))
    }

    /// Send a mouse scroll event to the terminal.
    /// Returns `true` if the event was forwarded (mouse protocol active), `false` otherwise.
    ///
    /// # Errors
    ///
    /// Returns `ProcessError::WriterDisconnected` if the PTY channel is closed, or
    /// `ProcessError::InputBufferFull` if the command is not reading its input.
    pub fn mouse_scroll(&self, up: bool, x: u16, y: u16) -> Result<bool, ProcessError> {
        if self.parser.lock().screen().mouse_protocol_mode() == vt100::MouseProtocolMode::None {
            return Ok(false);
        }

        self.send_pty(PtyInput::MouseScroll { up, x, y })?;
        Ok(true)
    }

    /// Wait until the exit is published: once output has drained, or shortly after the command
    /// exits if background processes keep the PTY open. Cancel-safe.
    ///
    /// # Errors
    ///
    /// Returns `ProcessError::Process` if the exit status could not be determined.
    pub async fn wait(&self) -> Result<ExitInfo, ProcessError> {
        let mut status_rx = self.status_rx.clone();
        let exit = status_rx
            .wait_for(Option::is_some)
            .await
            .ok()
            .and_then(|exit| exit.clone());
        exit.ok_or_else(|| ProcessError::Process("Process exit status unavailable".into()))
    }

    /// How the command ended, once [`wait`](Self::wait) would return.
    #[must_use]
    pub fn exit_info(&self) -> Option<ExitInfo> {
        self.status_rx.borrow().clone()
    }

    #[must_use]
    pub fn is_running(&self) -> bool {
        self.exit_info().is_none()
    }

    /// Whether the command has been reaped, which happens once it has exited and nothing holds
    /// its PTY any more. Signals are no-ops from then on.
    #[must_use]
    pub fn is_reaped(&self) -> bool {
        self.handle.is_reaped()
    }

    /// Wait until the command is reaped and its exit published (unless the exit could not be
    /// determined). Unlike [`wait`](Self::wait), this also waits for background processes that
    /// still hold the PTY. Cancel-safe.
    pub async fn wait_reaped(&self) {
        let mut reaped_rx = self.reaped_rx.clone();
        // An error means the waiter thread is gone, so there is nothing left to wait for
        let _ = reaped_rx.wait_for(|reaped| *reaped).await;
    }

    /// Send `signal` to the command's process group, then `SIGKILL` if it is still unreaped
    /// after `grace`.
    ///
    /// Also reaches background processes left behind by a command that already exited. Returns
    /// whether `signal` was sent: `false` once the command and everything holding its PTY are gone.
    ///
    /// # Errors
    ///
    /// Returns `ProcessError::Signal` if the signal cannot be sent.
    pub fn stop(&self, signal: StopSignal, grace: Duration) -> Result<bool, ProcessError> {
        self.handle
            .stop(signal, grace)
            .map_err(ProcessError::Signal)
    }

    /// Send `SIGKILL` to the command's process group right away.
    ///
    /// # Errors
    ///
    /// Returns `ProcessError::Signal` if the signal cannot be sent.
    pub fn force_kill(&self) -> Result<bool, ProcessError> {
        self.handle.force_kill().map_err(ProcessError::Signal)
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

    /// Queue bytes for the process's stdin without blocking.
    ///
    /// # Errors
    ///
    /// Returns `ProcessError::WriterDisconnected` if the PTY channel is closed, or
    /// `ProcessError::InputBufferFull` if the command is not reading its input.
    pub fn write(&self, input: Vec<u8>) -> Result<(), ProcessError> {
        self.send_pty(PtyInput::Write(input))
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::{Duration, Instant};

    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use parking_lot::Mutex;

    use super::{
        ProcessError, Terminal, TerminalOptions, TerminalSize, TerminalUpdate, spawn_output_writer,
    };
    use crate::commands::command::Command;
    use crate::process::StopSignal;
    use crate::pty::test_util::{pty_available, wait_until};

    /// Short grace so escalation tests finish well inside their deadlines
    const GRACE: Duration = Duration::from_millis(300);

    /// Kills the command's process group when dropped, so a failed assertion doesn't leak it.
    struct Spawned(Terminal);

    impl Drop for Spawned {
        fn drop(&mut self) {
            let _ = self.0.force_kill();
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
        Spawned(Terminal::new(&command, size, TerminalOptions::default()).unwrap())
    }

    fn output_writer(parser: &Arc<Mutex<vt100::Parser>>, dirty: &Arc<AtomicBool>) -> Sender {
        spawn_output_writer(Arc::clone(parser), Arc::clone(dirty), None, 0)
    }

    type Sender = crossbeam_channel::Sender<TerminalUpdate>;

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
        let tx = output_writer(&parser, &dirty);

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

    #[test]
    fn output_notify_fires_once_per_dirty_cycle() {
        let parser = Arc::new(Mutex::new(vt100::Parser::new(24, 80, 0)));
        let dirty = Arc::new(AtomicBool::new(false));
        let notify = Arc::new(tokio::sync::Notify::new());
        let tx = spawn_output_writer(
            Arc::clone(&parser),
            Arc::clone(&dirty),
            Some(Arc::clone(&notify)),
            0,
        );

        tx.send(TerminalUpdate::Process(b"a".to_vec())).unwrap();
        assert!(wait_until(Duration::from_secs(5), || dirty.load(Ordering::Acquire)));
        // notify_one stores a permit, so the notification is observable after the fact
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        let notified = |n: &tokio::sync::Notify| {
            rt.block_on(async {
                tokio::time::timeout(Duration::from_millis(100), n.notified())
                    .await
                    .is_ok()
            })
        };
        assert!(notified(&notify));

        // Still dirty: more output must not notify again
        tx.send(TerminalUpdate::Process(b"b".to_vec())).unwrap();
        assert!(wait_until(Duration::from_secs(5), || {
            parser.lock().screen().contents().contains("ab")
        }));
        assert!(!notified(&notify));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stop_after_exit_sends_nothing() {
        if !pty_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let term = spawn("true", dir.path());

        assert!(term.0.wait().await.unwrap().success());
        // Once reaped, the pid may already belong to someone else
        let reaped = wait_until(Duration::from_secs(5), || term.0.handle.is_reaped());
        assert!(reaped, "command was never reaped");
        assert!(!term.0.stop(StopSignal::Interrupt, GRACE).unwrap());
        assert!(!term.0.force_kill().unwrap());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stop_interrupts_running_command() {
        if !pty_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        // A builtin, not `touch`: bash drops a SIGINT that arrives while it waits on a child
        let term = spawn(": > ready && exec sleep 30", dir.path());
        let started = wait_until(Duration::from_secs(5), || dir.path().join("ready").exists());
        assert!(started, "command did not start");

        assert!(
            term.0
                .stop(StopSignal::Interrupt, Duration::from_secs(30))
                .unwrap()
        );

        let waited = tokio::time::timeout(Duration::from_secs(3), term.0.wait()).await;
        let exit = waited.expect("command ignored SIGINT").unwrap();
        assert_eq!(exit.signal, Some(libc::SIGINT));
        assert!(exit.stop_requested);
    }

    #[test]
    fn resize_applies_to_pty() {
        if !pty_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let term = spawn("exec sleep 30", dir.path());
        let size = TerminalSize::new(100, 30);

        term.0.resize(size).unwrap();

        assert_eq!(term.0.size(), size);
        let pty = term.0.master.lock().get_size().unwrap();
        assert_eq!((pty.cols, pty.rows), (100, 30));
        assert!(wait_until(Duration::from_secs(5), || {
            term.0.parser().lock().screen().size() == (30, 100)
        }));
    }

    #[test]
    fn missing_cwd_is_error() {
        if !pty_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let gone = dir.path().canonicalize().unwrap().join("gone");
        std::fs::create_dir(&gone).unwrap();
        std::fs::remove_dir(&gone).unwrap();
        let command = Command {
            id: "t".into(),
            name: "t".into(),
            cmd: "true".into(),
            cwd: gone.clone(),
            ..Default::default()
        };

        let result = Terminal::new(
            &command,
            TerminalSize::new(80, 24),
            TerminalOptions::default(),
        );
        match result {
            Err(ProcessError::MissingCwd(path)) => assert_eq!(path, gone),
            Err(e) => panic!("unexpected error: {e}"),
            Ok(_) => panic!("spawned with a missing working directory"),
        }
    }

    /// Leaves a HUP-ignoring `sleep 30` holding the PTY, whose pid is in `bg.pid`, and exits.
    const BACKGROUND_HOLDER: &str = "sh -c 'trap \"\" HUP; echo $$ > bg.pid; exec sleep 30' & \
        while [ ! -s bg.pid ]; do sleep 0.01; done; echo done";

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn wait_reports_signal() {
        if !pty_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let term = spawn("ulimit -c 0; kill -SEGV $$", dir.path());
        let exit = term.0.wait().await.unwrap();
        assert_eq!(exit.signal_name(), Some("SIGSEGV"));
        assert_eq!(exit.shell_code(), 128 + libc::SIGSEGV.unsigned_abs());
        assert!(!exit.stop_requested);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn wait_not_gated_by_background_holder() {
        if !pty_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let term = spawn(BACKGROUND_HOLDER, dir.path());
        let waited = tokio::time::timeout(Duration::from_secs(3), term.0.wait()).await;
        let exit = waited.expect("wait() blocked on a background PTY holder");
        assert!(exit.unwrap().success());
        assert!(!term.0.is_running());
        assert!(
            !term.0.handle.is_reaped(),
            "reaped while the PTY was still held"
        );
    }

    #[test]
    fn aborted_wait_does_not_block_runtime_drop() {
        if !pty_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let term = Arc::new(spawn("exec sleep 30", dir.path()));
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let polled = Arc::new(AtomicBool::new(false));
        let task = rt.spawn({
            let term = Arc::clone(&term);
            let polled = Arc::clone(&polled);
            async move {
                let mut wait = std::pin::pin!(term.0.wait());
                assert!(futures::poll!(&mut wait).is_pending());
                polled.store(true, Ordering::Release);
                let _ = wait.await;
            }
        });
        assert!(wait_until(Duration::from_secs(5), || polled.load(Ordering::Acquire)));
        task.abort();

        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            drop(rt);
            let _ = done_tx.send(());
        });
        assert!(
            done_rx.recv_timeout(Duration::from_secs(2)).is_ok(),
            "runtime drop blocked on an aborted wait()"
        );
    }

    #[test]
    fn parser_panic_keeps_output_thread_alive() {
        let parser = Arc::new(Mutex::new(vt100::Parser::new(24, 80, 0)));
        let dirty = Arc::new(AtomicBool::new(false));
        let tx = output_writer(&parser, &dirty);

        tx.send(TerminalUpdate::Panic).unwrap();
        let _ = tx.send(TerminalUpdate::Process(b"after".to_vec()));
        let alive = wait_until(Duration::from_secs(5), || {
            parser.lock().screen().contents().contains("after")
        });
        assert!(alive, "output stopped after a parser panic");
        assert_eq!(parser.lock().screen().size(), (24, 80));
    }

    fn pid_alive(pid: libc::pid_t) -> bool {
        // SAFETY: signal 0 only checks that the pid exists.
        unsafe { libc::kill(pid, 0) == 0 }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stop_kills_hup_ignoring_group() {
        if !pty_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let term = spawn("trap '' HUP INT; touch ready; sleep 30", dir.path());
        assert!(wait_until(Duration::from_secs(5), || dir
            .path()
            .join("ready")
            .exists()));

        assert!(term.0.stop(StopSignal::Interrupt, GRACE).unwrap());

        let waited = tokio::time::timeout(Duration::from_secs(3), term.0.wait()).await;
        let exit = waited.expect("stopped command kept running").unwrap();
        assert!(exit.stop_requested);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stop_kills_lingering_member_after_leader_exit() {
        if !pty_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let term = spawn(BACKGROUND_HOLDER, dir.path());
        let waited = tokio::time::timeout(Duration::from_secs(3), term.0.wait()).await;
        assert!(waited.expect("leader did not exit").unwrap().success());
        let holder = std::fs::read_to_string(dir.path().join("bg.pid")).unwrap();
        let holder: libc::pid_t = holder.trim().parse().unwrap();
        assert!(pid_alive(holder));

        assert!(term.0.stop(StopSignal::Interrupt, GRACE).unwrap());

        // Reaping waits for EOF, which needs the holder dead; its pid may linger as an orphan
        // zombie wherever nothing reaps orphans, so kill(pid, 0) would be a flaky check
        let killed = wait_until(Duration::from_secs(3), || term.0.handle.is_reaped());
        assert!(killed, "background process survived stop");
    }

    #[test]
    fn stop_not_blocked_by_pending_input() {
        if !pty_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let term = spawn("touch ready; exec sleep 30", dir.path());
        assert!(wait_until(Duration::from_secs(5), || dir
            .path()
            .join("ready")
            .exists()));

        // Far more than the tty input queue holds, so the writer thread blocks
        let mut line = vec![b'x'; 1023];
        line.push(b'\n');
        for _ in 0..200 {
            let _ = term.0.write(line.clone());
        }
        assert!(term.0.stop(StopSignal::Interrupt, GRACE).unwrap());

        let exit = term.0.handle.wait_timeout(Duration::from_secs(3));
        assert!(exit.is_some(), "stop was queued behind pending input");
    }
}
