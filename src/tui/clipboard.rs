//! Copying a command's output to the system clipboard.

use std::io::Write;
use std::process::{Command, Stdio};

use crate::pty::is_banner_line;

/// Most bytes one copy puts on the clipboard; longer output keeps its end, where failures are
pub const MAX_COPY_BYTES: usize = 1024 * 1024;

/// A way to reach the clipboard
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// A program that reads the text on stdin
    Program {
        name: &'static str,
        args: &'static [&'static str],
    },
    /// The OSC 52 escape sequence, which the terminal fnug runs in puts on its clipboard. It
    /// works over SSH, but fnug can't tell whether the terminal supports it.
    Osc52,
}

const PBCOPY: Backend = Backend::Program {
    name: "pbcopy",
    args: &[],
};
const LINUX_PROGRAMS: [Backend; 3] = [
    Backend::Program {
        name: "wl-copy",
        args: &[],
    },
    Backend::Program {
        name: "xclip",
        args: &["-selection", "clipboard"],
    },
    Backend::Program {
        name: "xsel",
        args: &["--clipboard", "--input"],
    },
];

/// The clipboard backends to try, in order. `is_set` tells whether an environment variable
/// is set.
///
/// Over SSH a clipboard program would copy on the remote machine, so OSC 52, which reaches
/// the user's terminal, comes first; elsewhere it is the last resort.
#[must_use]
pub fn choose_clipboard(is_set: impl Fn(&str) -> bool, macos: bool) -> Vec<Backend> {
    let programs: &[Backend] = if macos { &[PBCOPY] } else { &LINUX_PROGRAMS };
    if is_set("SSH_TTY") || is_set("SSH_CONNECTION") {
        std::iter::once(Backend::Osc52)
            .chain(programs.iter().copied())
            .collect()
    } else {
        programs
            .iter()
            .copied()
            .chain(std::iter::once(Backend::Osc52))
            .collect()
    }
}

/// The backends for this process's environment and platform.
#[must_use]
pub fn backends_for_env() -> Vec<Backend> {
    choose_clipboard(
        |var| std::env::var_os(var).is_some(),
        cfg!(target_os = "macos"),
    )
}

/// Copy `text` with the first program in `backends` that works, trying them in order and
/// skipping [`Backend::Osc52`], which the caller writes itself.
///
/// # Errors
///
/// Returns why each program failed, or that none was found.
pub fn copy_with_programs(text: &str, backends: &[Backend]) -> Result<&'static str, String> {
    let mut errors = Vec::new();
    let mut missing = Vec::new();
    for backend in backends {
        let Backend::Program { name, args } = *backend else {
            continue;
        };
        match run_program(name, args, text) {
            Ok(()) => return Ok(name),
            Err(ProgramError::NotFound) => missing.push(name),
            Err(ProgramError::Failed(e)) => errors.push(format!("{name}: {e}")),
        }
    }
    if errors.is_empty() {
        Err(format!(
            "no clipboard program found (tried {})",
            missing.join(", ")
        ))
    } else {
        Err(errors.join("; "))
    }
}

enum ProgramError {
    NotFound,
    Failed(String),
}

fn run_program(name: &str, args: &[&str], text: &str) -> Result<(), ProgramError> {
    let mut child = Command::new(name)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => ProgramError::NotFound,
            _ => ProgramError::Failed(e.to_string()),
        })?;
    let written = child
        .stdin
        .take()
        .map_or(Ok(()), |mut stdin| stdin.write_all(text.as_bytes()));
    // Wait even after a failed write, so the child is reaped
    let status = child
        .wait()
        .map_err(|e| ProgramError::Failed(e.to_string()))?;
    written.map_err(|e| ProgramError::Failed(format!("failed to write: {e}")))?;
    if status.success() {
        Ok(())
    } else {
        Err(ProgramError::Failed(format!("exited with {status}")))
    }
}

/// The text to copy from a command's terminal: its whole output, scrollback included,
/// without the banner fnug printed before it and the one after it.
#[must_use]
pub fn extract_copy_text(screen: &vt100::Screen) -> String {
    let contents = screen.all_contents();
    let mut lines: Vec<&str> = contents.lines().collect();
    if lines.first().is_some_and(|line| is_banner_line(line)) {
        lines.remove(0);
    }
    if lines.last().is_some_and(|line| is_banner_line(line)) {
        lines.pop();
    }
    let start = lines
        .iter()
        .position(|line| !line.trim().is_empty())
        .unwrap_or(lines.len());
    lines[start..].join("\n").trim_end().to_string()
}

/// `text` cut to its last [`MAX_COPY_BYTES`] bytes, starting at a line if one starts there.
#[must_use]
pub fn cap_copy_text(text: String) -> String {
    if text.len() <= MAX_COPY_BYTES {
        return text;
    }
    let mut start = text.len() - MAX_COPY_BYTES;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    let tail = &text[start..];
    let tail = tail.find('\n').map_or(tail, |i| &tail[i + 1..]);
    tail.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::ExitInfo;
    use crate::pty::{format_exit_message, format_start_message};

    #[test]
    fn extract_copy_text_whole_buffer() {
        let mut parser = vt100::Parser::new(24, 80, 1000);
        parser.process(&format_start_message("./many"));
        for i in 1..=200 {
            parser.process(format!("line-{i}\r\n").as_bytes());
        }
        parser.process("error: expected ❱ got foo\r\nFINAL\r\n".as_bytes());
        let exit = ExitInfo {
            code: Some(1),
            signal: None,
            stop_requested: false,
        };
        parser.process(&format_exit_message(&exit));

        let text = extract_copy_text(parser.screen());

        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.first(), Some(&"line-1"), "{text}");
        assert!(lines.contains(&"line-200"), "{text}");
        assert!(lines.contains(&"error: expected ❱ got foo"), "{text}");
        assert_eq!(lines.last(), Some(&"FINAL"), "{text}");
        assert_eq!(lines.len(), 202);
    }

    #[test]
    fn extract_copy_text_keeps_indentation_and_running_output() {
        let mut parser = vt100::Parser::new(5, 40, 100);
        parser.process(&format_start_message("cargo test"));
        parser.process(b"  indented\r\nlast");

        assert_eq!(extract_copy_text(parser.screen()), "  indented\nlast");
    }

    #[test]
    fn choose_clipboard_prefers_osc52_over_ssh() {
        let ssh = |var: &str| var == "SSH_CONNECTION";
        let local = |_: &str| false;

        let over_ssh = choose_clipboard(ssh, false);
        assert_eq!(over_ssh.first(), Some(&Backend::Osc52));
        assert_eq!(over_ssh.len(), 4);

        let linux = choose_clipboard(local, false);
        assert!(matches!(
            linux.first(),
            Some(Backend::Program {
                name: "wl-copy",
                ..
            })
        ));
        assert_eq!(linux.last(), Some(&Backend::Osc52));

        assert_eq!(choose_clipboard(local, true), [PBCOPY, Backend::Osc52]);
        assert_eq!(
            choose_clipboard(|var| var == "SSH_TTY", true),
            [Backend::Osc52, PBCOPY]
        );
    }

    #[test]
    fn missing_programs_are_named() {
        let backends = [
            Backend::Program {
                name: "fnug-no-such-clipboard",
                args: &[],
            },
            Backend::Osc52,
        ];
        assert_eq!(
            copy_with_programs("x", &backends),
            Err("no clipboard program found (tried fnug-no-such-clipboard)".into())
        );
    }

    #[test]
    fn cap_keeps_the_end_from_a_line_start() {
        let text = format!("{}\nlast line", "x".repeat(MAX_COPY_BYTES));
        assert_eq!(cap_copy_text(text), "last line");
        assert_eq!(cap_copy_text("short".into()), "short");
    }
}
