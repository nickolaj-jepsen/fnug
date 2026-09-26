//! Command output made fit for an LLM: terminal escapes removed and the size capped.

use std::sync::LazyLock;

use regex::Regex;

use crate::runner::CapturedOutput;

/// Escape sequences in their 7-bit and 8-bit forms: CSI; OSC, DCS, SOS, PM and APC with their
/// payload up to BEL or ST, or else to the end of the line; and two-byte ones. Then any other C0
/// or C1 control character but `\t`, `\n` and `\r`.
static NOISE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"\x1b(?:\[[0-?]*[ -/]*[@-~]",
        r"|[\]PX^_](?:[^\x07\x1b]*(?:\x07|\x1b\\)|[^\x07\x1b\n]*)",
        r"|[ -/]*[0-~])",
        r"|\x{9b}[0-?]*[ -/]*[@-~]",
        r"|[\x{90}\x{98}\x{9d}-\x{9f}](?:[^\x07\x1b\x{9c}]*(?:\x07|\x1b\\|\x{9c})|[^\x07\x1b\x{9c}\n]*)",
        r"|[\x00-\x08\x0b\x0c\x0e-\x1f\x7f-\x{9f}]",
    ))
    .expect("valid regex")
});

/// Remove terminal escape sequences and control characters, and keep only what a line shows
/// after carriage returns overwrite it: the text after its last `\r`.
pub(super) fn clean(text: &str) -> String {
    let stripped = NOISE.replace_all(text, "");
    let mut out = String::with_capacity(stripped.len());
    for (i, line) in stripped.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let line = line.trim_end_matches('\r');
        out.push_str(line.rsplit('\r').next().unwrap_or(line));
    }
    out
}

/// Where bytes were already left out of a text: `bytes` of them, at byte offset `at`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Gap {
    pub at: usize,
    pub bytes: u64,
}

/// A command's captured output, cleaned, and where capture dropped its middle.
pub(super) fn clean_captured(output: &CapturedOutput) -> (String, Option<Gap>) {
    let text = output.text();
    let omitted = output.omitted_bytes();
    // `CapturedOutput::text` marks the dropped middle this way
    let marker = format!("… {omitted} bytes omitted …\n");
    if omitted > 0
        && let Some(at) = text.find(&marker)
    {
        let mut cleaned = clean(&text[..at]);
        let gap = Gap {
            at: cleaned.len(),
            bytes: omitted,
        };
        cleaned.push_str(&clean(&text[at + marker.len()..]));
        return (cleaned, Some(gap));
    }
    (clean(&text), None)
}

/// A text cut down to a budget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Capped {
    pub text: String,
    /// Bytes left out, including those of the [`Gap`]; a marker line in `text` says how many.
    pub omitted: u64,
}

/// Keep at most `budget` bytes of `text`: a fifth from its start and the rest from its end,
/// where build errors and test summaries tend to be. Cuts fall on a line break when one is
/// near, and always on a character boundary. The left-out middle, which takes in `gap`, is
/// replaced by one marker line.
pub(super) fn cap(text: &str, gap: Option<Gap>, budget: usize) -> Capped {
    let len = text.len();
    let (head_end, tail_start) = if len <= budget {
        match gap {
            None => {
                return Capped {
                    text: text.to_owned(),
                    omitted: 0,
                };
            }
            Some(gap) => (gap.at, gap.at),
        }
    } else {
        let head = budget / 5;
        let tail = budget - head;
        // Put the cut where the gap already is, when it would fall in what is kept
        let (head, tail) = match gap {
            Some(gap) if gap.at < head => (gap.at, budget - gap.at),
            Some(gap) if len - gap.at < tail => (budget - (len - gap.at), len - gap.at),
            _ => (head, tail),
        };
        (cut_back(text, head), cut_forward(text, len - tail))
    };

    let omitted = (tail_start - head_end) as u64 + gap.map_or(0, |gap| gap.bytes);
    let mut capped = text[..head_end].to_owned();
    if !capped.is_empty() && !capped.ends_with('\n') {
        capped.push('\n');
    }
    capped.push_str(&marker(omitted));
    capped.push_str(&text[tail_start..]);
    Capped {
        text: capped,
        omitted,
    }
}

/// The line [`cap`] puts where it leaves bytes out.
fn marker(omitted: u64) -> String {
    format!("… {omitted} bytes omitted …\n")
}

/// Bytes [`cap`] may add to what it keeps of `text` with `gap`: a marker line, and the line
/// break before it.
pub(super) fn marker_room(text: &str, gap: Option<Gap>) -> usize {
    let most = text.len() as u64 + gap.map_or(0, |gap| gap.bytes);
    marker(most).len() + 1
}

/// An end for the kept start at or before `at`: after the last line break in the second half
/// of `text[..at]`, or else the nearest character boundary.
fn cut_back(text: &str, at: usize) -> usize {
    let mut at = at;
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    match text[..at].rfind('\n') {
        Some(newline) if newline + 1 >= at / 2 => newline + 1,
        _ => at,
    }
}

/// A start for the kept end at or after `at`: after the first line break in the first half of
/// `text[at..]`, or else the nearest character boundary.
fn cut_forward(text: &str, at: usize) -> usize {
    let mut at = at;
    while !text.is_char_boundary(at) {
        at += 1;
    }
    if at == 0 || text.as_bytes()[at - 1] == b'\n' {
        return at;
    }
    match text[at..].find('\n') {
        Some(newline) if newline < (text.len() - at) / 2 => at + newline + 1,
        _ => at,
    }
}

/// Share `total` bytes between texts of the given lengths, at most `each` per text: texts
/// shorter than their share leave the rest to longer ones.
pub(super) fn budgets(lengths: &[usize], each: usize, total: usize) -> Vec<usize> {
    let mut order: Vec<usize> = (0..lengths.len()).collect();
    order.sort_by_key(|&i| lengths[i]);
    let mut left = total;
    let mut budgets = vec![0; lengths.len()];
    for (done, &i) in order.iter().enumerate() {
        let share = (left / (lengths.len() - done)).min(each);
        budgets[i] = share;
        left -= lengths[i].min(share);
    }
    budgets
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::CaptureLimits;

    #[test]
    fn clean_strips_escapes_and_control_characters() {
        let text =
            "\x1b[1;31merror\x1b[0m: \x1b]8;;https://x\x07link\x1b]8;;\x1b\\ \x1b(Bok\x07\x1b\n";
        assert_eq!(clean(text), "error: link ok\n");
    }

    #[test]
    fn clean_strips_string_sequences_and_c1_controls() {
        // DCS (a sixel image), APC and PM with their payloads, then the 8-bit CSI, OSC and DCS
        let text = "a\x1bPq#0;2;0;0;0\n#0~~\x1b\\b\x1b_apc\x1b\\c\x1b^pm\x07d\u{9b}1;31me\u{9b}0mf\
                    \u{9d}0;title\u{9c}g\u{90}dcs\u{9c}h\u{85}i\n";
        assert_eq!(clean(text), "abcdefghi\n");
        // Unterminated, they stop at the end of the line
        assert_eq!(clean("x\x1b]0;title\nkept\n"), "x\nkept\n");
        assert_eq!(clean("x\x1bPpayload\nkept\x1b[0m\n"), "x\nkept\n");
    }

    #[test]
    fn clean_keeps_what_carriage_returns_leave() {
        let text = "10%\r50%\r\x1b[2K100%\r\ndone\r\n\ttab\n";
        assert_eq!(clean(text), "100%\ndone\n\ttab\n");
    }

    #[test]
    fn cap_keeps_short_text_whole() {
        let capped = cap("abc\n", None, 4);
        assert_eq!(capped.text, "abc\n");
        assert_eq!(capped.omitted, 0);
    }

    #[test]
    fn cap_prefers_line_breaks() {
        let text: String = (0..100)
            .map(|i| format!("line {i:03}\n"))
            .collect::<Vec<_>>()
            .concat();
        let capped = cap(&text, None, 100);
        // 20 bytes of head hold two whole lines, 80 of tail eight
        assert_eq!(
            capped.text,
            format!(
                "line 000\nline 001\n… 810 bytes omitted …\n{}",
                &text[text.len() - 72..]
            )
        );
        assert_eq!(capped.omitted, 810);
    }

    #[test]
    fn cap_never_splits_characters() {
        // One long line of three-byte characters, so cuts can't use a line break
        let text = "日本語".repeat(400);
        for budget in 1..40 {
            let capped = cap(&text, None, budget);
            let marker = format!("… {} bytes omitted …\n", capped.omitted);
            let (head, tail) = capped.text.split_once(&marker).unwrap();
            let head = head.strip_suffix('\n').unwrap_or(head);
            assert!(text.starts_with(head) && text.ends_with(tail), "{budget}");
            assert!(head.len() + tail.len() <= budget, "{budget}");
            assert_eq!(
                (head.len() + tail.len()) as u64 + capped.omitted,
                text.len() as u64
            );
        }
    }

    #[test]
    fn cap_adds_at_most_marker_room() {
        let text = "some output\n".repeat(50);
        for gap in [
            None,
            Some(Gap {
                at: 36,
                bytes: 99_999,
            }),
        ] {
            for budget in [0, 1, 12, 100, text.len(), text.len() + 10] {
                let capped = cap(&text, gap, budget);
                assert!(
                    capped.text.len() <= text.len().min(budget) + marker_room(&text, gap),
                    "{gap:?} {budget}: {}",
                    capped.text
                );
            }
        }
    }

    #[test]
    fn cap_counts_the_gap_in_one_marker() {
        let text = format!("{}{}", "h\n".repeat(10), "t\n".repeat(40));
        let gap = Gap {
            at: 20,
            bytes: 1000,
        };
        // Short enough to keep: the marker stands where the gap is
        let whole = cap(&text, Some(gap), 1000);
        assert_eq!(
            whole.text,
            format!(
                "{}… 1000 bytes omitted …\n{}",
                "h\n".repeat(10),
                "t\n".repeat(40)
            )
        );
        // Too long: the cut moves to the gap, and the kept head gives its room to the tail
        let capped = cap(&text, Some(Gap { at: 4, bytes: 1000 }), 50);
        assert_eq!(capped.omitted, 1000 + (text.len() - 4 - 46) as u64);
        assert_eq!(capped.text.matches("omitted").count(), 1, "{}", capped.text);
        assert!(capped.text.starts_with("h\nh\n… "), "{}", capped.text);
    }

    #[test]
    fn captured_gap_becomes_one_marker() {
        let mut output = CapturedOutput::new(CaptureLimits { head: 6, tail: 6 });
        output.push(b"\x1b[1mhead\x1b[0m\nmiddle\nmore middle\ntail\n");
        // Captured: "\x1b[1mhe" and "\ntail\n", with 25 bytes between them
        let (text, gap) = clean_captured(&output);
        assert_eq!(gap, Some(Gap { at: 3, bytes: 25 }));
        let capped = cap(&text, gap, 100);
        assert_eq!(capped.text, "he\n… 25 bytes omitted …\n\ntail\n");
    }

    #[test]
    fn budgets_give_what_short_texts_leave_to_long_ones() {
        assert_eq!(budgets(&[10, 1000, 1000], 400, 900), [300, 400, 400]);
        assert_eq!(budgets(&[10, 1000, 1000], 1000, 900), [300, 445, 445]);
        assert_eq!(budgets(&[], 100, 100), Vec::<usize>::new());
    }
}
