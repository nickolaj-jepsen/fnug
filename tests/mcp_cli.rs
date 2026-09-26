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
        let mut child = Command::new(env!("CARGO_BIN_EXE_fnug"))
            .current_dir(dir)
            .args(["--no-workspace", "mcp"])
            .env_remove("FNUG_LOG")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
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
