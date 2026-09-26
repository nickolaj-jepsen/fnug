//! Tests for `fnug mcp` as a process: stopping commands on cancellation and shutdown.

mod common;

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const TIMEOUT: Duration = Duration::from_secs(15);

const HANG_CONFIG: &str = r"
name: root
commands:
  - name: hang
    cmd: 'sleep 30 & echo $! > pid; wait'
";

/// A running `fnug mcp`, killed on drop.
struct Server {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
}

impl Server {
    /// Start the server in `dir` and complete the MCP handshake.
    fn start(dir: &Path) -> Self {
        Self::start_with_stderr(dir, Stdio::null())
    }

    /// [`Server::start`], with the server's stderr going to `stderr`.
    fn start_with_stderr(dir: &Path, stderr: impl Into<Stdio>) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_fnug"))
            .current_dir(dir)
            .args(["--no-workspace", "mcp"])
            .env_remove("FNUG_LOG")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(stderr)
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, lines) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let mut server = Self {
            stdin: child.stdin.take(),
            child,
            lines,
        };
        server.send(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "test", "version": "0"}
            }
        }));
        server.response(1);
        server.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        server
    }

    fn send(&mut self, message: &Value) {
        let stdin = self.stdin.as_mut().unwrap();
        writeln!(stdin, "{message}").unwrap();
        stdin.flush().unwrap();
    }

    fn call(&mut self, id: u64, tool: &str, arguments: &Value) {
        self.send(&json!({
            "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": {"name": tool, "arguments": arguments}
        }));
    }

    /// The response to request `id`, skipping other messages.
    fn response(&self, id: u64) -> Value {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.lines.recv_timeout(left) {
                Ok(line) => {
                    let message: Value = serde_json::from_str(&line).unwrap();
                    if message["id"] == id {
                        return message;
                    }
                }
                Err(RecvTimeoutError::Timeout) => panic!("no response to request {id}"),
                Err(RecvTimeoutError::Disconnected) => panic!("server closed stdout"),
            }
        }
    }

    /// Wait for the server to exit, killing it after [`TIMEOUT`].
    fn wait(&mut self) -> ExitStatus {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "fnug mcp did not exit within {TIMEOUT:?}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Kills a process when dropped, so a failed assertion doesn't leave it running.
struct KillOnDrop(i32);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        // SAFETY: plain syscall; the pid is one of the test's own descendants.
        unsafe { libc::kill(self.0, libc::SIGKILL) };
    }
}

/// Start `run_lint hang` and return its background `sleep`.
fn start_hang(server: &mut Server, dir: &Path) -> KillOnDrop {
    server.call(2, "run_lint", &json!({"command": "hang"}));
    KillOnDrop(common::read_pid(&dir.join("pid")))
}

fn assert_dies(pid: i32) {
    assert!(
        common::wait_until(TIMEOUT, || !common::process_alive(pid)),
        "pid {pid} is still running"
    );
}

#[test]
fn cancelled_call_kills_command() {
    let dir = tempfile::tempdir().unwrap();
    common::write_config(dir.path(), HANG_CONFIG);
    let mut server = Server::start(dir.path());
    let sleep = start_hang(&mut server, dir.path());

    server.send(&json!({
        "jsonrpc": "2.0", "method": "notifications/cancelled",
        "params": {"requestId": 2, "reason": "test"}
    }));
    assert_dies(sleep.0);

    // The server keeps serving
    server.call(3, "list_lints", &json!({}));
    assert!(server.response(3)["result"].is_object());
}

#[test]
fn starts_with_broken_config_and_reports_it_per_call() {
    let dir = tempfile::tempdir().unwrap();
    common::write_config(
        dir.path(),
        "name: root\ncommands:\n  - name: a\n    cmd: 'true'\n    colour: red\n",
    );
    let mut server = Server::start(dir.path());
    server.call(2, "list_lints", &json!({}));
    let result = &server.response(2)["result"];
    assert_eq!(result["isError"], true, "{result}");
    assert!(
        result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("colour"),
        "{result}"
    );

    // Fixing the config takes effect without a restart
    common::write_config(dir.path(), HANG_CONFIG);
    server.call(3, "list_lints", &json!({}));
    let result = &server.response(3)["result"];
    assert_eq!(result["isError"], false, "{result}");
}

#[test]
fn base_outside_repo_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    if git2::Repository::discover(dir.path()).is_ok() {
        eprintln!("skipping: the temp dir is inside a git repo");
        return;
    }
    // As `fnug check --base` does: passing with only `always` commands run would be false
    common::write_config(
        dir.path(),
        "name: root\ncommands:\n  - name: build\n    cmd: 'true'\n    auto:\n      always: true\n",
    );
    let mut server = Server::start(dir.path());
    for (id, tool) in [(2, "run_lints"), (3, "list_lints")] {
        server.call(id, tool, &json!({"base": "main"}));
        let result = &server.response(id)["result"];
        assert_eq!(result["isError"], true, "{tool}: {result}");
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(
            text.contains("base needs a git repository"),
            "{tool}: {text}"
        );
    }
    server.call(4, "run_lints", &json!({}));
    assert_eq!(server.response(4)["result"]["isError"], false);
}

#[test]
fn bad_base_is_an_error_without_git_commands() {
    let dir = tempfile::tempdir().unwrap();
    common::write_config(
        dir.path(),
        "name: root\ncommands:\n  - name: build\n    cmd: 'true'\n    auto:\n      always: true\n",
    );
    let repo = git2::Repository::init(dir.path()).unwrap();
    common::commit_all(&repo);
    let mut server = Server::start(dir.path());

    // No command has auto.git, so no repo is scanned, yet the base must still resolve
    for (id, tool) in [(2, "run_lints"), (3, "list_lints")] {
        server.call(id, tool, &json!({"base": "no-such-ref"}));
        let result = &server.response(id)["result"];
        assert_eq!(result["isError"], true, "{tool}: {result}");
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("base 'no-such-ref'"), "{tool}: {text}");
    }
    server.call(4, "run_lints", &json!({"base": "HEAD"}));
    let result = &server.response(4)["result"];
    assert_eq!(result["isError"], false, "{result}");
    let summary: Value =
        serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(summary["passed"], 1, "{summary}");
}

#[test]
fn config_warnings_logged_only_when_they_change() {
    let dir = tempfile::tempdir().unwrap();
    let logs = tempfile::tempdir().unwrap();
    let stderr_path = logs.path().join("stderr");
    let config = |group: &str| {
        format!(
            "name: root\ncommands:\n  - name: a\n    cmd: 'true'\nchildren:\n  - name: {group}\n"
        )
    };
    common::write_config(dir.path(), &config("empty"));
    let stderr = std::fs::File::create(&stderr_path).unwrap();
    let mut server = Server::start_with_stderr(dir.path(), stderr);
    for id in 2..5 {
        server.call(id, "list_lints", &json!({}));
        assert_eq!(server.response(id)["result"]["isError"], false);
    }
    common::write_config(dir.path(), &config("vacant"));
    for id in 5..7 {
        server.call(id, "list_lints", &json!({}));
        assert_eq!(server.response(id)["result"]["isError"], false);
    }
    drop(server.stdin.take());
    assert!(server.wait().success());

    let stderr = std::fs::read_to_string(&stderr_path).unwrap();
    assert_eq!(
        stderr.matches("Group 'empty' has no commands").count(),
        1,
        "{stderr}"
    );
    assert_eq!(
        stderr.matches("Group 'vacant' has no commands").count(),
        1,
        "{stderr}"
    );
}

#[test]
fn closing_stdin_stops_commands_and_server() {
    let dir = tempfile::tempdir().unwrap();
    common::write_config(dir.path(), HANG_CONFIG);
    let mut server = Server::start(dir.path());
    let sleep = start_hang(&mut server, dir.path());

    drop(server.stdin.take());
    assert!(server.wait().success());
    assert_dies(sleep.0);
}

#[test]
fn sigterm_stops_commands_and_exits_143() {
    let dir = tempfile::tempdir().unwrap();
    common::write_config(dir.path(), HANG_CONFIG);
    let mut server = Server::start(dir.path());
    let sleep = start_hang(&mut server, dir.path());

    // SAFETY: plain syscall on our own child.
    unsafe { libc::kill(server.child.id().cast_signed(), libc::SIGTERM) };
    assert_eq!(server.wait().code(), Some(143));
    assert_dies(sleep.0);
}

#[test]
fn sighup_and_sigint_reach_commands_as_themselves() {
    for (signal, name, code) in [(libc::SIGHUP, "HUP", 129), (libc::SIGINT, "INT", 130)] {
        let dir = tempfile::tempdir().unwrap();
        common::write_config(
            dir.path(),
            r#"
name: root
commands:
  - name: traps
    cmd: 'for s in HUP INT TERM; do trap "echo $s > got; exit 1" $s; done; touch started; while :; do sleep 0.05; done'
"#,
        );
        let mut server = Server::start(dir.path());
        server.call(2, "run_lint", &json!({"command": "traps"}));
        assert!(common::wait_until(TIMEOUT, || dir
            .path()
            .join("started")
            .exists()));

        // SAFETY: plain syscall on our own child.
        unsafe { libc::kill(server.child.id().cast_signed(), signal) };
        assert_eq!(server.wait().code(), Some(code), "{name}");
        let got = std::fs::read_to_string(dir.path().join("got")).unwrap();
        assert_eq!(got.trim(), name);
    }
}

#[test]
fn shutdown_waits_for_commands_to_clean_up() {
    let dir = tempfile::tempdir().unwrap();
    common::write_config(
        dir.path(),
        r"
name: root
commands:
  - name: graceful
    cmd: 'trap ''sleep 0.5; touch cleaned-up; exit 1'' TERM; touch started; i=0; while [ $i -lt 600 ]; do i=$((i+1)); sleep 0.05; done'
",
    );
    let mut server = Server::start(dir.path());
    server.call(2, "run_lint", &json!({"command": "graceful"}));
    let started = dir.path().join("started");
    assert!(common::wait_until(TIMEOUT, || started.exists()));

    drop(server.stdin.take());
    assert!(server.wait().success());
    assert!(
        dir.path().join("cleaned-up").exists(),
        "the server exited before the command's SIGTERM handler finished"
    );
}
