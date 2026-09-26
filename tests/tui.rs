//! The fnug binary's TUI in a pseudo-terminal.

mod common;

use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, BorrowedFd};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use portable_pty::{Child, CommandBuilder, ExitStatus, MasterPty, PtySize, native_pty_system};

use common::{process_alive, read_pid, wait_until, write_config};

/// Leaves the alternate screen, which restoring the terminal writes
const LEAVE_ALTERNATE_SCREEN: &str = "\x1b[?1049l";
const ENTER_ALTERNATE_SCREEN: &str = "\x1b[?1049h";
/// Written with every frame the TUI draws
const HIDE_CURSOR: &str = "\x1b[?25l";

/// fnug running in a PTY, and everything it wrote so far
struct Tui {
    child: Box<dyn Child + Send + Sync>,
    output: Arc<Mutex<Vec<u8>>>,
    /// `None` once the terminal is hung up
    master: Option<Master>,
}

/// Every open handle on the PTY's master side; closing them all hangs up fnug's terminal
struct Master {
    input: File,
    stop_reading: Arc<AtomicBool>,
    reader: JoinHandle<()>,
    _pty: Box<dyn MasterPty + Send>,
}

impl Tui {
    /// Run fnug with `args` in `dir`, or `None` where no PTY can be opened.
    fn spawn(dir: &Path, args: &[&str]) -> Option<Self> {
        Self::spawn_program(dir, env!("CARGO_BIN_EXE_fnug"), args)
    }

    /// Run `program` with `args` in `dir`, or `None` where no PTY can be opened.
    fn spawn_program(dir: &Path, program: &str, args: &[&str]) -> Option<Self> {
        let size = PtySize {
            rows: 24,
            cols: 100,
            ..PtySize::default()
        };
        let Ok(pty) = native_pty_system().openpty(size) else {
            eprintln!("skipping: no PTY available");
            return None;
        };
        let mut cmd = CommandBuilder::new(program);
        cmd.args(args);
        cmd.cwd(dir);
        cmd.env("TERM", "xterm-256color");
        let child = pty.slave.spawn_command(cmd).unwrap();
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

    fn output(&self) -> String {
        String::from_utf8_lossy(&self.output.lock().unwrap()).into_owned()
    }

    /// Type `keys` into the terminal.
    fn send(&mut self, keys: &[u8]) {
        let master = self.master.as_mut().expect("terminal hung up");
        master.input.write_all(keys).unwrap();
    }

    /// Close the master side, as closing a terminal window does. The kernel then sends fnug,
    /// the session leader, SIGHUP, and every write to its terminal fails.
    fn hang_up(&mut self) {
        let master = self.master.take().expect("terminal hung up");
        master.stop_reading.store(true, Ordering::Relaxed);
        master.reader.join().unwrap();
    }

    /// Wait until the output after byte `from` contains `text`.
    fn wait_for(&self, from: usize, text: &str) {
        let printed = wait_until(Duration::from_secs(10), || {
            self.output()
                .get(from..)
                .is_some_and(|out| out.contains(text))
        });
        assert!(printed, "never printed {text:?}:\n{:?}", self.output());
    }

    /// Wait until the TUI is on the alternate screen and shows `marker`.
    fn wait_started(&self, from: usize, marker: &str) {
        self.wait_for(from, ENTER_ALTERNATE_SCREEN);
        let started = from + self.output()[from..].find(ENTER_ALTERNATE_SCREEN).unwrap();
        self.wait_for(started, marker);
    }

    fn signal(&self, signal: libc::c_int) {
        let pid = libc::pid_t::try_from(self.child.process_id().unwrap()).unwrap();
        // SAFETY: sends a signal to the fnug process this test started.
        assert_eq!(unsafe { libc::kill(pid, signal) }, 0);
    }

    fn wait_exit(&mut self) -> ExitStatus {
        let mut status = None;
        let exited = wait_until(Duration::from_secs(10), || {
            status = self.child.try_wait().unwrap();
            status.is_some()
        });
        assert!(exited, "fnug did not exit");
        status.unwrap()
    }
}

impl Drop for Tui {
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

/// Send `signal` to the TUI once it shows `marker`, then check that it restores the terminal
/// and exits by itself. Returns its exit status.
fn signal_after_start(tui: &mut Tui, from: usize, marker: &str, signal: libc::c_int) -> u32 {
    tui.wait_started(from, marker);
    let before = tui.output().len();

    tui.signal(signal);
    let status = tui.wait_exit();

    assert_eq!(status.signal(), None, "killed by the signal");
    let after = tui.output();
    assert!(
        after[before..].contains(LEAVE_ALTERNATE_SCREEN),
        "terminal not restored:\n{after:?}"
    );
    status.exit_code()
}

#[test]
fn tui_quits_cleanly_on_sigterm_and_sighup() {
    for (signal, code) in [(libc::SIGTERM, 143), (libc::SIGHUP, 129)] {
        let dir = tempfile::tempdir().unwrap();
        write_config(
            dir.path(),
            "name: root\ncommands:\n  - name: lintcmd\n    cmd: \"true\"\n",
        );
        let Some(mut tui) = Tui::spawn(dir.path(), &[]) else {
            return;
        };

        assert_eq!(signal_after_start(&mut tui, 0, "lintcmd", signal), code);
    }
}

#[test]
fn check_handoff_quits_cleanly_on_sigterm_and_sighup() {
    for (signal, code) in [(libc::SIGTERM, 143), (libc::SIGHUP, 129)] {
        let dir = tempfile::tempdir().unwrap();
        write_config(
            dir.path(),
            "name: root\ncommands:\n  - name: failcmd\n    cmd: \"exit 1\"\n    auto:\n      always: true\n",
        );
        let Some(mut tui) = Tui::spawn(dir.path(), &["check"]) else {
            return;
        };
        tui.wait_for(0, "Open TUI");
        let prompt = tui.output().len();
        tui.send(b"y\r");

        // As in headless check, the signal wins over the check's failure
        assert_eq!(
            signal_after_start(&mut tui, prompt, "failcmd", signal),
            code
        );
    }
}

#[test]
fn tui_error_exits_2() {
    let dir = tempfile::tempdir().unwrap();
    write_config(
        dir.path(),
        "name: root\ncommands:\n  - name: lintcmd\n    cmd: \"true\"\n",
    );
    // fnug's stdout is a pipe to `cat`, the terminal's only writer, so killing `cat` makes
    // fnug's next draw fail
    let script =
        r#"{ "$0" --no-workspace; echo $? > status; } | sh -c 'echo $$ > catpid; exec cat'"#;
    let fnug = env!("CARGO_BIN_EXE_fnug");
    let Some(mut tui) = Tui::spawn_program(dir.path(), "sh", &["-c", script, fnug]) else {
        return;
    };
    tui.wait_started(0, "lintcmd");
    let cat = read_pid(&dir.path().join("catpid"));
    // SAFETY: sends a signal to a process this test started.
    unsafe { libc::kill(cat, libc::SIGKILL) };
    assert!(wait_until(Duration::from_secs(5), || !process_alive(cat)));

    // The help overlay changes the screen, so it is drawn
    tui.send(b"?");
    let status = dir.path().join("status");
    let mut code = String::new();
    let exited = wait_until(Duration::from_secs(10), || {
        code = std::fs::read_to_string(&status).unwrap_or_default();
        code.ends_with('\n')
    });
    assert!(exited, "fnug did not exit:\n{:?}", tui.output());
    assert_eq!(code.trim(), "2", "{:?}", tui.output());
}

#[test]
fn tui_stops_commands_when_its_terminal_hangs_up() {
    let dir = tempfile::tempdir().unwrap();
    write_config(
        dir.path(),
        "name: root\ncommands:\n  - name: sleeper\n    cmd: \"echo $$ > pid; exec sleep 1000\"\n    auto:\n      always: true\n",
    );
    let Some(mut tui) = Tui::spawn(dir.path(), &[]) else {
        return;
    };
    tui.wait_started(0, "sleeper");
    tui.send(b"\r");
    let pid = read_pid(&dir.path().join("pid"));

    tui.hang_up();
    let status = tui.wait_exit();

    // Not aborted by a panic on the dead terminal
    assert_eq!(status.signal(), None, "killed by a signal");
    assert_eq!(status.exit_code(), 129);
    assert!(
        wait_until(Duration::from_secs(5), || !process_alive(pid)),
        "command still running"
    );
}

/// An SGR mouse report for the 0-based cell `(x, y)`: `button` 0 is the left button, 35 a
/// move with no button down
fn mouse(button: u8, x: u16, y: u16, release: bool) -> String {
    let end = if release { 'm' } else { 'M' };
    format!("\x1b[<{button};{};{}{end}", x + 1, y + 1)
}

/// The frames drawn in `output`
fn frames(output: &str) -> usize {
    output.matches(HIDE_CURSOR).count()
}

#[test]
fn click_after_key_hits_the_new_layout() {
    let dir = tempfile::tempdir().unwrap();
    write_config(
        dir.path(),
        "name: root\nchildren:\n  - name: grp\n    commands:\n      - name: one\n        cmd: touch ran-one\ncommands:\n  - name: two\n    cmd: touch ran-two\n",
    );
    let Some(mut tui) = Tui::spawn(dir.path(), &[]) else {
        return;
    };
    tui.wait_started(0, "two");

    // `l` expands the collapsed `grp`, so `one` takes the row `two` was drawn on, and a
    // double-click there runs it
    let click = [false, true, false, true].map(|release| mouse(0, 10, 2, release));
    tui.send(format!("jl{}", click.concat()).as_bytes());

    let ran = wait_until(Duration::from_secs(10), || {
        dir.path().join("ran-one").exists()
    });
    assert!(ran, "one never ran:\n{:?}", tui.output());
    assert!(
        !dir.path().join("ran-two").exists(),
        "clicked the old layout"
    );
}

#[test]
fn mouse_moves_keep_redraws_within_the_frame_cap() {
    /// Time between two moves, faster than frames
    const MOVE_EVERY: Duration = Duration::from_millis(3);
    const MOVES: u32 = 300;

    let dir = tempfile::tempdir().unwrap();
    // New output about every millisecond, so each move finds the pane changed
    write_config(
        dir.path(),
        "name: root\ncommands:\n  - name: stream\n    cmd: \"while :; do echo x; sleep 0.001; done\"\n    auto:\n      always: true\n",
    );
    let Some(mut tui) = Tui::spawn(dir.path(), &[]) else {
        return;
    };
    tui.wait_started(0, "stream");
    let running = tui.output().len();
    tui.send(b"\r");
    let streaming = wait_until(Duration::from_secs(10), || {
        frames(&tui.output()[running..]) > 10
    });
    assert!(streaming, "output never redrawn:\n{:?}", tui.output());

    let from = tui.output().len();
    let start = std::time::Instant::now();
    for i in 0..MOVES {
        let x = 60 + u16::try_from(i % 10).unwrap();
        tui.send(mouse(35, x, 5, false).as_bytes());
        std::thread::sleep(MOVE_EVERY);
    }
    let drawn = frames(&tui.output()[from..]);
    let elapsed = start.elapsed();

    // One frame per 16 ms, plus the frames under way when counting started and stopped
    let cap = usize::try_from(elapsed.as_millis() / 16).unwrap() + 10;
    assert!(
        drawn <= cap,
        "{drawn} frames for {MOVES} moves in {elapsed:?}, more than {cap}"
    );
}
