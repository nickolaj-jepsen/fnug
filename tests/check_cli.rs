//! Tests for `fnug check` as a process: its output, exit codes and signal handling.

mod common;

use std::io::{Read, Write};
use std::num::NonZeroUsize;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};

const TIMEOUT: Duration = Duration::from_secs(10);

fn fnug_command(dir: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_fnug"));
    common::git::isolate(&mut command)
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
    if std::thread::available_parallelism().map_or(1, NonZeroUsize::get) < 2 {
        eprintln!("skipping: with one CPU, `--jobs 0` runs one command at a time");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    // Each command marks itself started, then waits for the other; run in turn, `a` fails
    common::write_config(
        dir.path(),
        r"
name: root
auto:
  always: true
commands:
  - name: a
    cmd: &wait 'touch $MARK; i=0; until [ -e a ] && [ -e b ]; do i=$((i+1)); [ $i -gt 250 ] && exit 1; sleep 0.02; done'
    env: {MARK: a}
  - name: b
    cmd: *wait
    env: {MARK: b}
",
    );
    let output = check(dir.path(), &["--jobs", "0"]);
    let stderr = stderr(&output);
    assert!(output.status.success(), "{stderr}");
    assert!(stderr.contains("2 commands: 2 passed ("), "{stderr}");
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

#[test]
fn config_error_exits_2() {
    let dir = tempfile::tempdir().unwrap();
    common::write_config(dir.path(), "name: root\ncomands: []\n");
    let output = check(dir.path(), &[]);
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));

    let output = check(dir.path(), &["-c", "missing.yaml"]);
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(stderr(&output).contains("Config file not found"));

    // The TUI fails the same way, before it starts
    let output = Command::new(env!("CARGO_BIN_EXE_fnug"))
        .current_dir(dir.path())
        .args(["-c", "missing.yaml"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
}

/// Names of the commands a check ran, from their `PASS` and `FAIL` lines, in order.
fn ran(output: &Output) -> Vec<String> {
    stderr(output)
        .lines()
        .filter_map(|line| {
            let rest = line.split_once("] ")?.1;
            let end = rest.find(" PASS").or_else(|| rest.find(" FAIL"))?;
            Some(rest[..end].to_string())
        })
        .collect()
}

/// A git repo in `dir` with `config` and `src/a.rs` committed, so the work tree is clean.
fn clean_repo(dir: &Path, config: &str) -> git2::Repository {
    let repo = git2::Repository::init(dir).unwrap();
    common::write_config(dir, config);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/a.rs"), "fn a() {}\n").unwrap();
    common::commit_all(&repo);
    repo
}

const SELECTION: &str = r"
name: root
commands:
  - name: lint
    cmd: 'true'
    auto:
      git: true
      path: [src]
  - name: slow
    cmd: 'true'
    auto:
      git: true
      path: [src]
      check: false
  - name: build
    cmd: 'true'
  - name: test
    cmd: 'true'
    depends_on: [build]
";

#[test]
fn all_runs_every_command_on_clean_tree() {
    let dir = tempfile::tempdir().unwrap();
    clean_repo(dir.path(), SELECTION);

    let output = check(dir.path(), &["--all"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        ran(&output),
        ["lint", "build", "test"],
        "{}",
        stderr(&output)
    );

    let output = check(dir.path(), &["--all", "--include-manual"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        ran(&output),
        ["lint", "slow", "build", "test"],
        "{}",
        stderr(&output)
    );
}

#[test]
fn include_manual_keeps_old_all() {
    let dir = tempfile::tempdir().unwrap();
    clean_repo(dir.path(), SELECTION);
    std::fs::write(dir.path().join("src/a.rs"), "fn a() { }\n").unwrap();

    let output = check(dir.path(), &[]);
    assert_eq!(ran(&output), ["lint"], "{}", stderr(&output));
    let output = check(dir.path(), &["--include-manual"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(ran(&output), ["lint", "slow"], "{}", stderr(&output));
}

#[test]
fn targets_run_with_deps() {
    let dir = tempfile::tempdir().unwrap();
    clean_repo(dir.path(), SELECTION);

    let output = check(dir.path(), &["test"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(ran(&output), ["build", "test"], "{}", stderr(&output));

    // By name, case-insensitively, and even with `check: false`
    let output = check(dir.path(), &["SLOW", "lint"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(ran(&output), ["lint", "slow"], "{}", stderr(&output));

    for conflicting in [&["test", "--all"][..], &["test", "--include-manual"]] {
        let output = check(dir.path(), conflicting);
        assert_eq!(output.status.code(), Some(2), "{conflicting:?}");
        assert!(
            stderr(&output).contains("cannot be used with"),
            "{output:?}"
        );
    }
}

#[test]
fn ambiguous_target_exits_2() {
    let dir = tempfile::tempdir().unwrap();
    common::write_config(
        dir.path(),
        r"
name: root
children:
  - name: backend
    commands:
      - name: test
        cmd: 'echo RAN'
  - name: frontend
    commands:
      - name: test
        cmd: 'echo RAN'
",
    );
    let output = check(dir.path(), &["test"]);
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    let err = stderr(&output);
    assert!(err.contains("backend/test"), "{err}");
    assert!(err.contains("frontend/test"), "{err}");
    assert!(!String::from_utf8_lossy(&output.stdout).contains("RAN"));

    let output = check(dir.path(), &["tset"]);
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(stderr(&output).contains("backend/test"), "{output:?}");

    let output = check(dir.path(), &["backend/test"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(ran(&output), ["test"], "{}", stderr(&output));
}

/// A clean repo from [`clean_repo`] with a branch `base` at its first commit.
fn repo_with_base_branch(dir: &Path) -> git2::Repository {
    let repo = clean_repo(dir, SELECTION);
    {
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        repo.branch("base", &head, false).unwrap();
    }
    repo
}

#[test]
fn base_selects_committed_change() {
    let dir = tempfile::tempdir().unwrap();
    let repo = repo_with_base_branch(dir.path());
    std::fs::write(dir.path().join("src/a.rs"), "fn a() { }\n").unwrap();
    common::commit_all(&repo);

    // A clean tree selects nothing by itself, and says how to select more
    let output = check(dir.path(), &[]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(ran(&output).is_empty(), "{}", stderr(&output));
    assert!(
        stderr(&output).contains(
            "No commands selected (4 configured; use --all, --base <ref>, or name commands)"
        ),
        "{}",
        stderr(&output)
    );

    let output = check(dir.path(), &["--base", "base"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(ran(&output), ["lint"], "{}", stderr(&output));

    let output = check(dir.path(), &["--base", "base", "--include-manual"]);
    assert_eq!(ran(&output), ["lint", "slow"], "{}", stderr(&output));

    // Nothing changed since HEAD itself
    let output = check(dir.path(), &["--base", "HEAD"]);
    assert!(ran(&output).is_empty(), "{}", stderr(&output));
}

#[test]
fn base_includes_untracked() {
    let dir = tempfile::tempdir().unwrap();
    repo_with_base_branch(dir.path());
    std::fs::write(dir.path().join("src/new.rs"), "fn new() {}\n").unwrap();

    let output = check(dir.path(), &["--base", "base"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(ran(&output), ["lint"], "{}", stderr(&output));
}

#[test]
fn base_unresolvable_exits_2() {
    let dir = tempfile::tempdir().unwrap();
    repo_with_base_branch(dir.path());
    std::fs::write(dir.path().join("src/a.rs"), "fn a() { }\n").unwrap();

    let output = check(dir.path(), &["--base", "origin/nope"]);
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    let err = stderr(&output);
    assert!(err.contains("base 'origin/nope'"), "{err}");
    assert!(err.contains("fetch-depth: 0"), "{err}");
    assert!(ran(&output).is_empty(), "{err}");

    let output = check(dir.path(), &["--base", "base", "--all"]);
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("cannot be used with"),
        "{output:?}"
    );
}

const STAGED: &str = r"
name: root
commands:
  - name: rust-lint
    cmd: 'touch .git/rust-lint-ran; ! grep -rn BAD src/'
    auto:
      git: true
      path: [src]
      regex: ['\.rs$']
  - name: docs
    cmd: 'true'
    auto:
      git: true
      path: [docs]
";

/// Write [`STAGED`], `src/a.rs` and `docs/a.md` in `dir`.
fn write_staged_project(dir: &Path) {
    common::write_config(dir, STAGED);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::create_dir_all(dir.join("docs")).unwrap();
    std::fs::write(dir.join("src/a.rs"), "ok\n").unwrap();
    std::fs::write(dir.join("docs/a.md"), "docs\n").unwrap();
}

#[test]
fn staged_ignores_unstaged_and_untracked() {
    let dir = tempfile::tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    write_staged_project(dir.path());
    common::commit_all(&repo);

    std::fs::write(dir.path().join("docs/a.md"), "more docs\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("docs/a.md")).unwrap();
    index.write().unwrap();
    std::fs::write(dir.path().join("src/a.rs"), "BAD\n").unwrap();
    std::fs::write(dir.path().join("src/scratch.rs"), "BAD\n").unwrap();

    let output = check(dir.path(), &["--staged"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(ran(&output), ["docs"], "{}", stderr(&output));

    // The working tree scope sees the unstaged work
    let output = check(dir.path(), &[]);
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert_eq!(ran(&output), ["rust-lint", "docs"], "{}", stderr(&output));

    let output = check(dir.path(), &["--staged", "--base", "HEAD"]);
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
}

#[test]
fn staged_outside_repo_exits_2() {
    let dir = tempfile::tempdir().unwrap();
    if git2::Repository::discover(dir.path()).is_ok() {
        eprintln!("skipping: the temp dir is inside a git repo");
        return;
    }
    common::write_config(dir.path(), STAGED);
    let output = check(dir.path(), &["--staged"]);
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("staged changes need a git repository"),
        "{}",
        stderr(&output)
    );
}

/// A repo in `dir` made with the git CLI, with [`write_staged_project`] committed and a
/// pre-commit hook that runs `fnug check --staged`.
fn repo_with_staged_hook(dir: &Path) {
    common::git::init(dir);
    write_staged_project(dir);
    common::git::git(dir, &["add", "-A"]);
    common::git::git(dir, &["commit", "-qm", "init"]);
    let hook = dir.join(".git/hooks/pre-commit");
    std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
    std::fs::write(
        &hook,
        format!(
            "#!/bin/sh\nexec '{}' --no-workspace check --no-tui --staged --mute-success\n",
            env!("CARGO_BIN_EXE_fnug")
        ),
    )
    .unwrap();
    std::fs::set_permissions(&hook, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
}

/// Run `git commit` with `args` in `dir`; returns whether it succeeded, and its stderr.
fn git_commit(dir: &Path, args: &[&str]) -> (bool, String) {
    let output = common::git::command(dir)
        .arg("commit")
        .args(args)
        .env_remove("FNUG_LOG")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn staged_honours_git_commit_a() {
    if !common::git::available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    repo_with_staged_hook(dir.path());
    let marker = dir.path().join(".git/rust-lint-ran");

    // A docs-only commit isn't blocked by unstaged work
    std::fs::write(dir.path().join("src/a.rs"), "BAD\n").unwrap();
    std::fs::write(dir.path().join("docs/a.md"), "more docs\n").unwrap();
    common::git::git(dir.path(), &["add", "docs/a.md"]);
    let (committed, err) = git_commit(dir.path(), &["-qm", "docs"]);
    assert!(committed, "{err}");
    assert!(!marker.exists(), "{err}");

    // `commit -a` stages the work in a temporary index, which selects the lint
    let (committed, err) = git_commit(dir.path(), &["-qam", "all"]);
    assert!(!committed, "the lint should block the commit: {err}");
    assert!(marker.exists(), "{err}");
}

#[test]
fn staged_honours_commit_path() {
    if !common::git::available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    repo_with_staged_hook(dir.path());
    std::fs::write(dir.path().join("src/a.rs"), "BAD\n").unwrap();

    let (committed, err) = git_commit(dir.path(), &["-qm", "path", "src/a.rs"]);
    assert!(!committed, "the lint should block the commit: {err}");
    assert!(dir.path().join(".git/rust-lint-ran").exists(), "{err}");

    std::fs::write(dir.path().join("src/a.rs"), "fine\n").unwrap();
    let (committed, err) = git_commit(dir.path(), &["-qm", "path", "src/a.rs"]);
    assert!(committed, "{err}");
}

const FIXER: &str = r"
name: root
auto:
  always: true
commands:
  - name: fmt
    cmd: sed -i.bak 's/x=1/x = 1/' src/a.py && rm src/a.py.bak
  - name: report
    cmd: 'echo report > report.txt'
  - name: revert
    cmd: printf 'y=2\n' > src/b.py
  - name: broken
    cmd: 'exit 3'
";

/// A clean repo in `dir` with [`FIXER`], an unformatted `src/a.py` and `src/b.py`.
fn fixer_repo(dir: &Path) -> git2::Repository {
    let repo = git2::Repository::init(dir).unwrap();
    common::write_config(dir, FIXER);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/a.py"), "x=1\n").unwrap();
    std::fs::write(dir.join("src/b.py"), "y=2\n").unwrap();
    common::commit_all(&repo);
    repo
}

#[test]
fn fixer_fails_in_check() {
    let dir = tempfile::tempdir().unwrap();
    fixer_repo(dir.path());

    let output = check(dir.path(), &[]);
    let err = stderr(&output);
    assert_eq!(output.status.code(), Some(1), "{err}");
    assert!(
        err.contains("fmt FAIL (modified: src/a.py — review and re-stage)"),
        "{err}"
    );
    // Neither a new untracked file nor rewriting a clean file's content is a modification,
    // and a failure keeps its own reason
    assert!(err.contains("report PASS"), "{err}");
    assert!(err.contains("revert PASS"), "{err}");
    assert!(err.contains("broken FAIL (exit 3)"), "{err}");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("src/a.py")).unwrap(),
        "x = 1\n"
    );

    // Formatted now, so it passes
    let output = check(dir.path(), &["fmt"]);
    assert!(output.status.success(), "{}", stderr(&output));
}

#[test]
fn fixer_on_dirty_file_detected_by_hash() {
    let dir = tempfile::tempdir().unwrap();
    fixer_repo(dir.path());
    // Both already modified: one the fixer changes again, one it leaves alone
    std::fs::write(dir.path().join("src/a.py"), "x=1\nz=3\n").unwrap();
    std::fs::write(dir.path().join("src/b.py"), "y=3\n").unwrap();

    let output = check(dir.path(), &["fmt", "--mute-success"]);
    let err = stderr(&output);
    assert_eq!(output.status.code(), Some(1), "{err}");
    assert!(err.contains("fmt FAIL (modified: src/a.py — "), "{err}");

    // Reverting a modification changes the file too
    let output = check(dir.path(), &["revert"]);
    let err = stderr(&output);
    assert_eq!(output.status.code(), Some(1), "{err}");
    assert!(err.contains("revert FAIL (modified: src/b.py — "), "{err}");
}

#[test]
fn allow_modifications_passes() {
    let dir = tempfile::tempdir().unwrap();
    fixer_repo(dir.path());

    let output = check(dir.path(), &["fmt", "--allow-modifications"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stderr(&output).contains("fmt PASS"), "{}", stderr(&output));
    assert_eq!(
        std::fs::read_to_string(dir.path().join("src/a.py")).unwrap(),
        "x = 1\n"
    );
}

/// Each command writes the files it was given to `.git/<name>.out`.
const FILES: &str = r#"
name: root
commands:
  - name: here
    cwd: src
    cmd: 'printf "%s" "${FNUG_FILES-unset}" > ../.git/here.out'
    auto:
      git: true
      path: [.]
  - name: elsewhere
    cwd: docs
    cmd: 'printf "%s" "${FNUG_FILES-unset}" > ../.git/elsewhere.out'
    auto:
      git: true
      path: [../src]
  - name: quoted
    cmd: 'for f in {files}; do echo "[$f]"; done > .git/quoted.out'
    auto:
      git: true
      path: [src]
"#;

/// A repo with [`FILES`], where two files in `src` changed, one was deleted and one is new.
/// Returns its canonical path.
fn files_repo(dir: &Path) -> std::path::PathBuf {
    let root = dir.canonicalize().unwrap();
    let repo = git2::Repository::init(&root).unwrap();
    common::write_config(&root, FILES);
    for file in ["src/a.py", "src/lib/b.py", "src/gone.py", "docs/x.md"] {
        let path = root.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "old\n").unwrap();
    }
    common::commit_all(&repo);
    std::fs::write(root.join("src/a.py"), "new\n").unwrap();
    std::fs::write(root.join("src/lib/b.py"), "new\n").unwrap();
    std::fs::remove_file(root.join("src/gone.py")).unwrap();
    std::fs::write(root.join("src/new file.py"), "new\n").unwrap();
    root
}

fn out(root: &Path, name: &str) -> String {
    std::fs::read_to_string(root.join(format!(".git/{name}.out"))).unwrap()
}

#[test]
fn fnug_files_env_relative_paths() {
    let dir = tempfile::tempdir().unwrap();
    let root = files_repo(dir.path());

    let output = check(&root, &[]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(out(&root, "here"), "a.py\nlib/b.py\nnew file.py");
    // Absolute outside the command's cwd
    let src = root.join("src");
    assert_eq!(
        out(&root, "elsewhere"),
        format!(
            "{}\n{}\n{}",
            src.join("a.py").display(),
            src.join("lib/b.py").display(),
            src.join("new file.py").display()
        )
    );
}

#[test]
fn files_placeholder_quotes_spaces() {
    let dir = tempfile::tempdir().unwrap();
    let root = files_repo(dir.path());
    std::fs::write(root.join("src/it's.py"), "new\n").unwrap();

    let output = check(&root, &["quoted"]);
    assert!(output.status.success(), "{}", stderr(&output));
    // Named as a target, so it has no matched files
    assert_eq!(out(&root, "quoted"), "[src]\n");

    let output = check(&root, &[]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        out(&root, "quoted"),
        "[src/a.py]\n[src/it's.py]\n[src/lib/b.py]\n[src/new file.py]\n"
    );
}

#[test]
fn deleted_files_excluded() {
    let dir = tempfile::tempdir().unwrap();
    let root = files_repo(dir.path());

    let output = check(&root, &[]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(!out(&root, "here").contains("gone"));
    assert!(!out(&root, "quoted").contains("gone"));

    // Only deletions: the command is selected, with nothing to pass
    std::fs::write(root.join("src/a.py"), "old\n").unwrap();
    std::fs::write(root.join("src/lib/b.py"), "old\n").unwrap();
    std::fs::remove_file(root.join("src/new file.py")).unwrap();
    let output = check(&root, &[]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(out(&root, "here"), "unset");
    assert_eq!(out(&root, "quoted"), "[src]\n");
}

#[test]
fn files_fallback_to_auto_path() {
    let dir = tempfile::tempdir().unwrap();
    let root = files_repo(dir.path());

    // Every command, none selected by its files; a FNUG_FILES fnug got isn't passed on
    let output = fnug_command(&root, &["--all"])
        .env("FNUG_FILES", "stale")
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(out(&root, "here"), "unset");
    assert_eq!(out(&root, "elsewhere"), "unset");
    assert_eq!(out(&root, "quoted"), "[src]\n");
}

#[test]
fn oversize_list_falls_back() {
    let dir = tempfile::tempdir().unwrap();
    let root = files_repo(dir.path());
    let name = "f".repeat(200);
    for i in 0..600 {
        std::fs::write(root.join(format!("src/{name}{i}")), "").unwrap();
    }

    let output = check(&root, &["--mute-success"]);
    let err = stderr(&output);
    assert!(output.status.success(), "{err}");
    assert!(err.contains("too many to pass"), "{err}");
    assert_eq!(out(&root, "here"), "unset");
    assert_eq!(out(&root, "quoted"), "[src]\n");
}

#[test]
fn streamed_cleanup_output_comes_before_not_run_lines() {
    let dir = tempfile::tempdir().unwrap();
    common::write_config(
        dir.path(),
        r"
name: root
auto:
  always: true
commands:
  - name: first
    cmd: 'trap ''sleep 0.3; echo CLEANUP >&2; exit 1'' TERM; echo $$ > pid; while :; do sleep 0.05; done'
  - name: second
    cmd: 'true'
",
    );
    let mut child = fnug_command(dir.path(), &[])
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let shell = KillOnDrop(common::read_pid(&dir.path().join("pid")));

    // SAFETY: plain syscall on our own child.
    unsafe { libc::kill(child.id().cast_signed(), libc::SIGTERM) };
    let status = wait(&mut child);
    let stderr = stderr(&child.wait_with_output().unwrap());
    assert_eq!(status.code(), Some(143), "{stderr}");
    let at = |text: &str| {
        stderr
            .find(text)
            .unwrap_or_else(|| panic!("no {text:?} in:\n{stderr}"))
    };
    assert!(at("CLEANUP") < at("[1/2] first CANCELLED"), "{stderr}");
    assert!(
        at("[1/2] first CANCELLED") < at("[2/2] second NOT RUN"),
        "{stderr}"
    );
    drop(shell);
}
