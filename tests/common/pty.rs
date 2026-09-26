//! A process with a pseudo-terminal as its controlling terminal, as when started from a shell.

use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, BorrowedFd};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use portable_pty::{Child, CommandBuilder, ExitStatus, MasterPty, PtySize, native_pty_system};

use super::{TIMEOUT, wait_until};

/// A process running on a PTY, and everything it wrote so far. Killed on drop.
pub struct Pty {
    child: Box<dyn Child + Send + Sync>,
    output: Arc<Mutex<Vec<u8>>>,
    /// `None` once the terminal is hung up
    master: Option<Master>,
}

/// Every open handle on the PTY's master side; closing them all hangs up the terminal
struct Master {
    input: File,
    stop_reading: Arc<AtomicBool>,
    reader: JoinHandle<()>,
    _pty: Box<dyn MasterPty + Send>,
}

/// The fnug binary with `args` in `dir`, for [`Pty::spawn`], set up as
/// [`super::fnug_command`] does.
pub fn fnug(dir: &Path, args: &[&str]) -> CommandBuilder {
    let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_fnug"));
    command.cwd(dir);
    command.args(args);
    command.env_remove("FNUG_LOG");
    super::git::isolate_pty(&mut command);
    command
}

impl Pty {
    /// Start `command` on a new PTY of `size`, or return `None` where no PTY can be opened.
    pub fn spawn(command: CommandBuilder, size: PtySize) -> Option<Self> {
        let Ok(pty) = native_pty_system().openpty(size) else {
            eprintln!("skipping: no PTY available");
            return None;
        };
        let child = pty.slave.spawn_command(command).unwrap();
        drop(pty.slave);
        // Own handles, since portable-pty's reader can't be interrupted and its writer sends
        // EOF when dropped
        // SAFETY: the fd stays open while `pty.master` lives, which outlives the borrow.
        let fd = unsafe { BorrowedFd::borrow_raw(pty.master.as_raw_fd().unwrap()) };
        let reader = File::from(fd.try_clone_to_owned().unwrap());
        let input = File::from(fd.try_clone_to_owned().unwrap());
        let output = Arc::new(Mutex::new(Vec::new()));
        let stop_reading = Arc::new(AtomicBool::new(false));
        let reader = {
            let (sink, stop) = (Arc::clone(&output), Arc::clone(&stop_reading));
            std::thread::spawn(move || read_until_stopped(reader, &sink, &stop))
        };
        Some(Self {
            child,
            output,
            master: Some(Master {
                input,
                stop_reading,
                reader,
                _pty: pty.master,
            }),
        })
    }

    /// What the process wrote to the terminal so far.
    pub fn output(&self) -> String {
        String::from_utf8_lossy(&self.output.lock().unwrap()).into_owned()
    }

    /// Type `keys` into the terminal.
    pub fn send(&mut self, keys: &[u8]) {
        let master = self.master.as_mut().expect("terminal hung up");
        master.input.write_all(keys).unwrap();
    }

    /// Type `keys` into the terminal, which fails on macOS once every process has closed it.
    pub fn try_send(&mut self, keys: &[u8]) -> std::io::Result<()> {
        let master = self.master.as_mut().expect("terminal hung up");
        master.input.write_all(keys)
    }

    /// Close the master side, as closing a terminal window does. The kernel then sends the
    /// session leader SIGHUP, and every write to the terminal fails.
    pub fn hang_up(&mut self) {
        let master = self.master.take().expect("terminal hung up");
        master.stop_reading.store(true, Ordering::Relaxed);
        master.reader.join().unwrap();
    }

    /// Wait until the output after byte `from` contains `text`.
    pub fn wait_for(&self, from: usize, text: &str) {
        let printed = wait_until(TIMEOUT, || {
            self.output()
                .get(from..)
                .is_some_and(|out| out.contains(text))
        });
        assert!(printed, "never printed {text:?}:\n{:?}", self.output());
    }

    pub fn signal(&self, signal: libc::c_int) {
        let pid = libc::pid_t::try_from(self.child.process_id().unwrap()).unwrap();
        // SAFETY: sends a signal to the process this test started.
        assert_eq!(unsafe { libc::kill(pid, signal) }, 0);
    }

    /// Wait for the process to exit, failing with its output after [`TIMEOUT`].
    pub fn wait_exit(&mut self) -> ExitStatus {
        let mut status = None;
        let exited = wait_until(TIMEOUT, || {
            status = self.child.try_wait().unwrap();
            status.is_some()
        });
        assert!(
            exited,
            "did not exit within {TIMEOUT:?}:\n{:?}",
            self.output()
        );
        status.unwrap()
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        let _ = self.child.kill();
        if let Some(master) = &self.master {
            master.stop_reading.store(true, Ordering::Relaxed);
        }
    }
}

/// Copy what `reader` reads into `sink` until it fails or `stop` is set.
fn read_until_stopped(mut reader: File, sink: &Mutex<Vec<u8>>, stop: &AtomicBool) {
    let mut buf = [0; 4096];
    while !stop.load(Ordering::Relaxed) {
        let mut poll = libc::pollfd {
            fd: reader.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: polls one open fd, for at most 20 ms so `stop` is seen.
        if unsafe { libc::poll(&raw mut poll, 1, 20) } <= 0 {
            continue;
        }
        match reader.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => sink.lock().unwrap().extend_from_slice(&buf[..n]),
        }
    }
}
