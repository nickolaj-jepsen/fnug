//! Tests for `fnug check` as a process: its output, exit codes and signal handling.

mod common;

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};

const TIMEOUT: Duration = Duration::from_secs(10);

fn fnug_command(dir: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_fnug"));
    command
        .current_dir(dir)
        .args(["--no-workspace", "check", "--no-tui"])
        .args(args)
        .env_remove("FNUG_LOG")
        .stdin(Stdio::null());
    command
}

fn check(dir: &Path, args: &[&str]) -> Output {
    fnug_command(dir, args).output().unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Wait for `child` to exit, killing it after [`TIMEOUT`].
fn wait(child: &mut Child) -> ExitStatus {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("fnug did not exit within {TIMEOUT:?}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Kills a process when dropped, so a failed assertion doesn't leave it running.
struct KillOnDrop(i32);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        // SAFETY: plain syscall; the pid is one of the test's own grandchildren.
        unsafe { libc::kill(self.0, libc::SIGKILL) };
    }
}

/// `fnug check` with a pty as its controlling terminal, as when started from a shell. Killed
/// on drop.
struct PtyCheck {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    writer: Box<dyn Write + Send>,
    output: Arc<Mutex<Vec<u8>>>,
    _master: Box<dyn MasterPty + Send>,
}

impl PtyCheck {
    /// Start `fnug check` in `dir`, or return `None` when no pty can be opened here.
    fn start(dir: &Path, args: &[&str]) -> Option<Self> {
        let Ok(pair) = native_pty_system().openpty(PtySize::default()) else {
            eprintln!("skipping: no PTY available");
            return None;
        };
        let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_fnug"));
        command.cwd(dir);
        command.args(["--no-workspace", "check", "--no-tui"]);
        command.args(args);
        command.env_remove("FNUG_LOG");
        let child = pair.slave.spawn_command(command).unwrap();

        let mut reader = pair.master.try_clone_reader().unwrap();
        let output = Arc::new(Mutex::new(Vec::new()));
        let sink = output.clone();
        std::thread::spawn(move || {
            let mut buf = [0; 4096];
            while let Ok(n @ 1..) = reader.read(&mut buf) {
                sink.lock().unwrap().extend_from_slice(&buf[..n]);
            }
        });
        Some(Self {
            child,
            writer: pair.master.take_writer().unwrap(),
            output,
            _master: pair.master,
        })
    }

    /// What fnug and its commands wrote to the terminal so far.
    fn output(&self) -> String {
        String::from_utf8_lossy(&self.output.lock().unwrap()).into_owned()
    }

    /// Type `keys` at the terminal.
    fn type_keys(&mut self, keys: &[u8]) {
        self.writer.write_all(keys).unwrap();
        self.writer.flush().unwrap();
    }

    /// Wait for fnug to exit and return its exit code, killing it after [`TIMEOUT`].
    fn wait(&mut self) -> u32 {
        let mut status = None;
        let exited = common::wait_until(TIMEOUT, || {
            status = self.child.try_wait().unwrap();
            status.is_some()
        });
        assert!(
            exited,
            "fnug did not exit within {TIMEOUT:?}:\n{}",
            self.output()
        );
        status.unwrap().exit_code()
    }
}

impl Drop for PtyCheck {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

#[test]
fn summary_line_counts() {
    let dir = tempfile::tempdir().unwrap();
    common::write_config(
        dir.path(),
        r"
name: root
auto:
  always: true
commands:
  - name: build
    cmd: 'exit 1'
  - name: test
    cmd: 'true'
    depends_on: [build]
  - name: lint
    cmd: 'true'
    depends_on: [test]
  - name: other
    cmd: 'true'
",
    );
    let output = check(dir.path(), &[]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = stderr(&output);
    assert!(
        stderr.contains("4 commands: 1 passed, 1 failed, 2 skipped ("),
        "{stderr}"
    );
    assert!(stderr.contains("lint SKIP (build failed)"), "{stderr}");
}

#[test]
fn fail_fast_summary_reports_not_run() {
    let dir = tempfile::tempdir().unwrap();
    common::write_config(
        dir.path(),
        r"
name: root
auto:
  always: true
commands:
  - name: first
    cmd: 'exit 1'
  - name: second
    cmd: 'true'
  - name: third
    cmd: 'true'
",
    );
    let output = check(dir.path(), &["--fail-fast"]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = stderr(&output);
    assert!(stderr.contains("[2/3] second NOT RUN\n"), "{stderr}");
    assert!(stderr.contains("[3/3] third NOT RUN\n"), "{stderr}");
    assert!(
        stderr.contains("3 commands: 1 failed, 2 not run ("),
        "{stderr}"
    );
}

#[test]
fn parallel_fail_fast_numbers_every_command() {
    let dir = tempfile::tempdir().unwrap();
    common::write_config(
        dir.path(),
        r"
name: root
auto:
  always: true
commands:
  - name: slow
    cmd: 'echo $$ > pid; exec sleep 30'
  - name: fail
    cmd: 'i=0; until [ -e pid ]; do i=$((i+1)); [ $i -gt 250 ] && exit 9; sleep 0.02; done; exit 4'
  - name: third
    cmd: 'true'
",
    );
    let mut child = fnug_command(dir.path(), &["-j", "2", "--fail-fast", "--mute-success"])
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let slow = KillOnDrop(common::read_pid(&dir.path().join("pid")));
    let status = wait(&mut child);
    let stderr = stderr(&child.wait_with_output().unwrap());
    assert_eq!(status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("[1/3] fail FAIL (exit 4) "), "{stderr}");
    assert!(stderr.contains("[2/3] third NOT RUN\n"), "{stderr}");
    assert!(stderr.contains("[3/3] slow CANCELLED "), "{stderr}");
    assert!(
        stderr.contains("3 commands: 1 failed, 1 cancelled, 1 not run ("),
        "{stderr}"
    );
    assert!(!common::process_alive(slow.0), "{stderr}");
}

#[test]
fn streaming_header_on_own_line() {
    let dir = tempfile::tempdir().unwrap();
    common::write_config(
        dir.path(),
        r"
name: root
commands:
  - name: greet
    cmd: 'echo hello >&2'
    auto:
      always: true
",
    );
    let output = check(dir.path(), &[]);
    assert!(output.status.success());
    let stderr = stderr(&output);
    assert!(
        stderr.contains("[1/1] greet\nhello\n[1/1] greet PASS "),
        "{stderr}"
    );
}

#[test]
fn captured_serial_run_shows_the_running_command() {
    let dir = tempfile::tempdir().unwrap();
    common::write_config(
        dir.path(),
        r"
name: root
auto:
  always: true
commands:
  - name: slow
    cmd: 'i=0; until [ -e go ]; do i=$((i+1)); [ $i -gt 500 ] && exit 1; sleep 0.02; done'
  - name: quick
    cmd: 'true'
",
    );
    let log = dir.path().join("stderr");
    let mut child = fnug_command(dir.path(), &["--mute-success"])
        .stderr(std::fs::File::create(&log).unwrap())
        .spawn()
        .unwrap();
    let shown = common::wait_until(TIMEOUT, || {
        std::fs::read_to_string(&log).is_ok_and(|s| s.contains("[1/2] slow "))
    });
    std::fs::write(dir.path().join("go"), "").unwrap();
    let status = wait(&mut child);
    let stderr = std::fs::read_to_string(&log).unwrap();
    assert!(shown, "nothing printed while `slow` ran:\n{stderr}");
    assert!(status.success(), "{stderr}");
    assert!(stderr.contains("[1/2] slow PASS "), "{stderr}");
    assert!(stderr.contains("[2/2] quick PASS "), "{stderr}");
}

#[test]
fn failure_shows_exit_code_and_merged_output() {
    let dir = tempfile::tempdir().unwrap();
    common::write_config(
        dir.path(),
        r"
name: root
commands:
  - name: mixed
    cmd: 'echo out1; echo err1 >&2; echo out2; printf no-newline; exit 3'
    auto:
      always: true
  - name: quiet
    cmd: 'echo QUIET-OUTPUT'
    auto:
      always: true
",
    );
    let output = check(dir.path(), &["--mute-success"]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = stderr(&output);
    assert!(stderr.contains("mixed FAIL (exit 3)"), "{stderr}");
    assert!(
        stderr.contains("out1\nerr1\nout2\nno-newline\n"),
        "{stderr}"
    );
    assert!(!stderr.contains("QUIET-OUTPUT"), "{stderr}");
}

#[test]
fn spawn_error_is_printed() {
    let dir = tempfile::tempdir().unwrap();
    common::write_config(
        dir.path(),
        r"
name: root
commands:
  - name: build
    cmd: 'true'
    auto:
      always: true
",
    );
    for args in [&[][..], &["--mute-success"]] {
        let output = fnug_command(dir.path(), args)
            .env("PATH", "/nonexistent")
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        let stderr = stderr(&output);
        assert!(stderr.contains("failed to spawn sh"), "{stderr}");
    }
}

#[test]
fn timeout_flag_stops_hung_command() {
    let dir = tempfile::tempdir().unwrap();
    common::write_config(
        dir.path(),
        r"
name: root
auto:
  always: true
commands:
  - name: hang
    cmd: 'exec sleep 30'
  - name: slow
    cmd: 'sleep 0.5'
    timeout: 0
",
    );
    for args in [
        &["--timeout", "300ms"][..],
        &["--timeout=300ms", "--mute-success"],
    ] {
        let mut child = fnug_command(dir.path(), args)
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let status = wait(&mut child);
        let stderr = stderr(&child.wait_with_output().unwrap());
        assert_eq!(status.code(), Some(1), "{stderr}");
        assert!(stderr.contains("hang TIMEOUT after 0.3s\n"), "{stderr}");
        assert!(stderr.contains("slow PASS"), "{stderr}");
    }

    let output = check(dir.path(), &["--timeout", "soon"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("invalid duration"), "{output:?}");
}

#[test]
fn jobs_run_independent_commands_together() {
    let dir = tempfile::tempdir().unwrap();
    // Each command marks itself started, then waits until all three have
    common::write_config(
        dir.path(),
        r"
name: root
auto:
  always: true
commands:
  - name: a
    cmd: &wait 'touch $MARK; i=0; until [ -e a ] && [ -e b ] && [ -e c ]; do i=$((i+1)); [ $i -gt 250 ] && exit 1; sleep 0.02; done; echo $MARK done'
    env: {MARK: a}
  - name: b
    cmd: *wait
    env: {MARK: b}
  - name: c
    cmd: *wait
    env: {MARK: c}
",
    );
    let mut child = fnug_command(dir.path(), &["-j", "3"])
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let status = wait(&mut child);
    let stderr = stderr(&child.wait_with_output().unwrap());
    assert!(status.success(), "{stderr}");
    assert!(stderr.contains("3 commands: 3 passed ("), "{stderr}");
    // Captured, so each command's output follows its own result line
    assert!(stderr.contains(" b PASS "), "{stderr}");
    assert!(stderr.contains("b done\n"), "{stderr}");
}

#[test]
fn jobs_zero_means_one_per_cpu() {
    let dir = tempfile::tempdir().unwrap();
    common::write_config(
        dir.path(),
        r"
name: root
commands:
  - name: greet
    cmd: 'echo hello'
    auto:
      always: true
",
    );
    let output = check(dir.path(), &["--jobs", "0"]);
    assert!(output.status.success(), "{output:?}");
    assert!(stderr(&output).contains("[1/1] greet PASS "), "{output:?}");
}

#[test]
fn sigterm_kills_children_exits_143() {
    let dir = tempfile::tempdir().unwrap();
    common::write_config(
        dir.path(),
        r"
name: root
commands:
  - name: hang
    cmd: 'sleep 30 & echo $! > pid; wait'
    auto:
      always: true
",
    );
    let mut child = fnug_command(dir.path(), &["--mute-success"])
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let sleep = KillOnDrop(common::read_pid(&dir.path().join("pid")));

    // SAFETY: plain syscall on our own child.
    unsafe { libc::kill(child.id().cast_signed(), libc::SIGTERM) };
    let status = wait(&mut child);
    let output = child.wait_with_output().unwrap();
    assert_eq!(status.code(), Some(143), "{}", stderr(&output));
    assert!(common::wait_until(TIMEOUT, || !common::process_alive(
        sleep.0
    )));
    let stderr = stderr(&output);
    assert!(stderr.contains("hang CANCELLED"), "{stderr}");
    assert!(stderr.contains("Interrupted"), "{stderr}");
}

#[test]
fn sigterm_stops_streaming_command() {
    let dir = tempfile::tempdir().unwrap();
    common::write_config(
        dir.path(),
        r"
name: root
commands:
  - name: hang
    cmd: 'echo $$ > pid; exec sleep 30'
    auto:
      always: true
",
    );
    let mut child = fnug_command(dir.path(), &[]).spawn().unwrap();
    let sleep = KillOnDrop(common::read_pid(&dir.path().join("pid")));

    // SAFETY: plain syscall on our own child.
    unsafe { libc::kill(child.id().cast_signed(), libc::SIGTERM) };
    assert_eq!(wait(&mut child).code(), Some(143));
    assert!(common::wait_until(TIMEOUT, || !common::process_alive(
        sleep.0
    )));
}

#[test]
fn streamed_timeout_stops_children_without_a_terminal() {
    let dir = tempfile::tempdir().unwrap();
    common::write_config(
        dir.path(),
        r"
name: root
commands:
  - name: hang
    cmd: 'sleep 30 >/dev/null 2>&1 & echo $! > pid; wait'
    auto:
      always: true
",
    );
    let log = dir.path().join("stderr");
    let mut child = fnug_command(dir.path(), &["--timeout", "300ms"])
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(&log).unwrap())
        .spawn()
        .unwrap();
    let sleep = KillOnDrop(common::read_pid(&dir.path().join("pid")));
    let status = wait(&mut child);
    let stderr = std::fs::read_to_string(&log).unwrap();
    assert_eq!(status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("[1/1] hang\n"), "{stderr}");
    assert!(stderr.contains("hang TIMEOUT"), "{stderr}");
    assert!(
        common::wait_until(TIMEOUT, || !common::process_alive(sleep.0)),
        "the command's child outlived its timeout"
    );
}

#[test]
fn captured_command_cannot_block_on_the_terminal() {
    let dir = tempfile::tempdir().unwrap();
    common::write_config(
        dir.path(),
        r"
name: root
commands:
  - name: prompt
    cmd: 'read answer < /dev/tty && echo got $answer'
    auto:
      always: true
",
    );
    for args in [&["--mute-success"][..], &["-j", "2"]] {
        let Some(mut fnug) = PtyCheck::start(dir.path(), args) else {
            return;
        };
        let code = fnug.wait();
        let output = fnug.output();
        assert_eq!(code, 1, "{args:?}:\n{output}");
        assert!(output.contains("prompt"), "{args:?}:\n{output}");
        assert!(output.contains("FAIL"), "{args:?}:\n{output}");
    }
}

#[test]
fn ctrl_c_lets_commands_clean_up() {
    let dir = tempfile::tempdir().unwrap();
    common::write_config(
        dir.path(),
        r"
name: root
commands:
  - name: graceful
    cmd: 'trap ''sleep 0.3; touch cleaned-up; exit 130'' INT; touch started; i=0; while [ $i -lt 600 ]; do i=$((i+1)); sleep 0.05; done'
    auto:
      always: true
",
    );
    for args in [&[][..], &["--mute-success"]] {
        for marker in ["started", "cleaned-up"] {
            let _ = std::fs::remove_file(dir.path().join(marker));
        }
        let Some(mut fnug) = PtyCheck::start(dir.path(), args) else {
            return;
        };
        let started = dir.path().join("started");
        assert!(common::wait_until(TIMEOUT, || started.exists()), "{args:?}");
        fnug.type_keys(b"\x03");
        let code = fnug.wait();
        let output = fnug.output();
        assert_eq!(code, 130, "{args:?}:\n{output}");
        assert!(
            dir.path().join("cleaned-up").exists(),
            "{args:?}:\n{output}"
        );
        assert!(output.contains("CANCELLED"), "{args:?}:\n{output}");
    }
}

#[test]
fn ctrl_c_sends_sigterm_later_to_streamed_command_that_ignores_it() {
    let dir = tempfile::tempdir().unwrap();
    common::write_config(
        dir.path(),
        r"
name: root
commands:
  - name: stubborn
    cmd: 'trap '''' INT; trap ''echo TERM > caught; exit 1'' TERM; sleep 30 & echo $! > pid; wait'
    auto:
      always: true
",
    );
    let Some(mut fnug) = PtyCheck::start(dir.path(), &[]) else {
        return;
    };
    // Stays behind, since only the shell gets signals
    let _sleep = KillOnDrop(common::read_pid(&dir.path().join("pid")));
    fnug.type_keys(b"\x03");
    let interrupted = Instant::now();
    let code = fnug.wait();
    let output = fnug.output();
    assert_eq!(code, 130, "{output}");
    let caught = std::fs::read_to_string(dir.path().join("caught")).unwrap_or_default();
    assert_eq!(caught.trim(), "TERM", "{output}");
    // SIGTERM waits for the kill grace
    assert!(
        interrupted.elapsed() > Duration::from_secs(2),
        "{:?}",
        interrupted.elapsed()
    );
}
