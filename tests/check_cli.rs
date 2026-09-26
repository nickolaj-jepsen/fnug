//! Tests for `fnug check` as a process: its output, exit codes and signal handling.

mod common;

use std::path::Path;
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::time::{Duration, Instant};

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
    assert!(
        stderr.contains("3 commands: 1 failed, 2 not run ("),
        "{stderr}"
    );
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
        assert!(stderr.contains("hang TIMEOUT after 0.3s"), "{stderr}");
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
