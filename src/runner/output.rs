//! Bounded capture of a command's output.

use std::collections::VecDeque;
use std::fmt::Write as _;

/// How much captured output to keep: the first `head` bytes and the last `tail` bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureLimits {
    pub head: usize,
    pub tail: usize,
}

impl CaptureLimits {
    /// 256 KiB from the start and 1 MiB from the end.
    pub const DEFAULT: Self = Self {
        head: 256 * 1024,
        tail: 1024 * 1024,
    };
}

impl Default for CaptureLimits {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// A command's output with the middle dropped once it outgrows its [`CaptureLimits`].
#[derive(Debug, Clone)]
pub struct CapturedOutput {
    limits: CaptureLimits,
    head: Vec<u8>,
    tail: VecDeque<u8>,
    total: u64,
}

impl CapturedOutput {
    #[must_use]
    pub fn new(limits: CaptureLimits) -> Self {
        Self {
            limits,
            head: Vec::new(),
            tail: VecDeque::new(),
            total: 0,
        }
    }

    /// Append output, dropping the oldest bytes past the head once the tail is full.
    pub fn push(&mut self, mut bytes: &[u8]) {
        self.total += bytes.len() as u64;
        let room = self.limits.head.saturating_sub(self.head.len());
        let (head, rest) = bytes.split_at(room.min(bytes.len()));
        self.head.extend_from_slice(head);
        bytes = rest;

        if bytes.len() >= self.limits.tail {
            self.tail.clear();
            bytes = &bytes[bytes.len() - self.limits.tail..];
        } else {
            let excess = (self.tail.len() + bytes.len()).saturating_sub(self.limits.tail);
            self.tail.drain(..excess);
        }
        self.tail.extend(bytes);
    }

    /// Bytes the command wrote, including dropped ones.
    #[must_use]
    pub fn total_bytes(&self) -> u64 {
        self.total
    }

    /// Bytes dropped from the middle.
    #[must_use]
    pub fn omitted_bytes(&self) -> u64 {
        self.total - (self.head.len() + self.tail.len()) as u64
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.total == 0
    }

    /// The kept bytes as text, with invalid UTF-8 replaced and a marker where bytes were
    /// dropped.
    #[must_use]
    pub fn text(&self) -> String {
        let (a, b) = self.tail.as_slices();
        let omitted = self.omitted_bytes();
        if omitted == 0 {
            // Contiguous, so a character split between head and tail decodes whole
            return String::from_utf8_lossy(&[&self.head, a, b].concat()).into_owned();
        }
        let mut text = String::from_utf8_lossy(&self.head).into_owned();
        if !text.ends_with('\n') {
            text.push('\n');
        }
        let _ = writeln!(text, "… {omitted} bytes omitted …");
        text.push_str(&String::from_utf8_lossy(&[a, b].concat()));
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn captured(limits: CaptureLimits, chunks: &[&[u8]]) -> CapturedOutput {
        let mut output = CapturedOutput::new(limits);
        for chunk in chunks {
            output.push(chunk);
        }
        output
    }

    const SMALL: CaptureLimits = CaptureLimits { head: 4, tail: 4 };

    #[test]
    fn keeps_everything_within_limits() {
        let output = captured(SMALL, &[b"abc", b"defgh"]);
        assert_eq!(output.omitted_bytes(), 0);
        assert_eq!(output.text(), "abcdefgh");
    }

    #[test]
    fn drops_the_middle() {
        let output = captured(SMALL, &[b"abcdef", b"ghij", b"klmn"]);
        assert_eq!(output.total_bytes(), 14);
        assert_eq!(output.omitted_bytes(), 6);
        assert_eq!(output.text(), "abcd\n… 6 bytes omitted …\nklmn");
    }

    #[test]
    fn character_across_head_and_tail_stays_whole() {
        // "é" is two bytes, split between the head and the tail
        let output = captured(SMALL, &["abcé".as_bytes(), b"fg"]);
        assert_eq!(output.omitted_bytes(), 0);
        assert_eq!(output.text(), "abcéfg");
    }

    #[test]
    fn oversized_chunk_keeps_its_end() {
        let output = captured(SMALL, &[b"0123456789"]);
        assert_eq!(output.text(), "0123\n… 2 bytes omitted …\n6789");
    }
}
