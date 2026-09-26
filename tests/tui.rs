//! The fnug binary's TUI in a pseudo-terminal.

mod common;

use std::path::Path;
use std::time::Duration;

use portable_pty::{CommandBuilder, PtySize};

use common::pty::Pty;
use common::{TIMEOUT, process_alive, read_pid, wait_until, write_config};

/// Leaves the alternate screen, which restoring the terminal writes
const LEAVE_ALTERNATE_SCREEN: &str = "\x1b[?1049l";
const ENTER_ALTERNATE_SCREEN: &str = "\x1b[?1049h";
/// Written with every frame the TUI draws
const HIDE_CURSOR: &str = "\x1b[?25l";

const SIZE: PtySize = PtySize {
    rows: 24,
    cols: 100,
    pixel_width: 0,
    pixel_height: 0,
};

/// Run fnug with `args` in `dir`, or `None` where no PTY can be opened.
fn spawn(dir: &Path, args: &[&str]) -> Option<Pty> {
    let mut command = common::pty::fnug(dir, args);
    command.env("TERM", "xterm-256color");
    Pty::spawn(command, SIZE)
}

/// Wait until the TUI is on the alternate screen and shows `marker`.
fn wait_started(tui: &Pty, from: usize, marker: &str) {
    tui.wait_for(from, ENTER_ALTERNATE_SCREEN);
    let started = from + tui.output()[from..].find(ENTER_ALTERNATE_SCREEN).unwrap();
    tui.wait_for(started, marker);
}

/// Send `signal` to the TUI once it shows `marker`, then check that it restores the terminal
/// and exits by itself. Returns its exit status.
fn signal_after_start(tui: &mut Pty, from: usize, marker: &str, signal: libc::c_int) -> u32 {
    wait_started(tui, from, marker);
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
        let Some(mut tui) = spawn(dir.path(), &[]) else {
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
        let Some(mut tui) = spawn(dir.path(), &["check"]) else {
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
    let mut command = CommandBuilder::new("sh");
    command.args(["-c", script, env!("CARGO_BIN_EXE_fnug")]);
    command.cwd(dir.path());
    command.env("TERM", "xterm-256color");
    let Some(mut tui) = Pty::spawn(command, SIZE) else {
        return;
    };
    wait_started(&tui, 0, "lintcmd");
    let cat = read_pid(&dir.path().join("catpid"));
    // SAFETY: sends a signal to a process this test started.
    unsafe { libc::kill(cat, libc::SIGKILL) };
    assert!(wait_until(Duration::from_secs(5), || !process_alive(cat)));

    // The help overlay changes the screen, so it is drawn. fnug may already have exited after
    // a draw of its own, so the key can fail to arrive
    let _ = tui.try_send(b"?");
    let status = dir.path().join("status");
    let mut code = String::new();
    let exited = wait_until(TIMEOUT, || {
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
    let Some(mut tui) = spawn(dir.path(), &[]) else {
        return;
    };
    wait_started(&tui, 0, "sleeper");
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
    let Some(mut tui) = spawn(dir.path(), &[]) else {
        return;
    };
    wait_started(&tui, 0, "two");

    // `l` expands the collapsed `grp`, so `one` takes the row `two` was drawn on, and a
    // double-click there runs it
    let click = [false, true, false, true].map(|release| mouse(0, 10, 2, release));
    tui.send(format!("jl{}", click.concat()).as_bytes());

    let ran = wait_until(TIMEOUT, || dir.path().join("ran-one").exists());
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
    let Some(mut tui) = spawn(dir.path(), &[]) else {
        return;
    };
    wait_started(&tui, 0, "stream");
    let running = tui.output().len();
    tui.send(b"\r");
    let streaming = wait_until(TIMEOUT, || frames(&tui.output()[running..]) > 10);
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
