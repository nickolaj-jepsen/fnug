use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use parking_lot::Mutex;
use std::sync::Arc;

/// Translate a crossterm `KeyEvent` into bytes to send to the PTY.
///
/// Returns None if the key shouldn't be forwarded.
pub fn translate_key_event(key: &KeyEvent, parser: &Arc<Mutex<vt100::Parser>>) -> Option<Vec<u8>> {
    let screen = parser.lock();
    let app_cursor = screen.screen().application_cursor();
    drop(screen);

    match key.code {
        KeyCode::Char(c) => {
            if key.modifiers.contains(KeyModifiers::CONTROL) {
                // Ctrl+letter → \x01..\x1a
                let ctrl_byte = (c as u8).wrapping_sub(b'a').wrapping_add(1);
                if (1..=26).contains(&ctrl_byte) {
                    Some(vec![ctrl_byte])
                } else {
                    None
                }
            } else if key.modifiers.contains(KeyModifiers::ALT) {
                let mut bytes = vec![0x1b];
                let mut buf = [0u8; 4];
                let s = c.encode_utf8(&mut buf);
                bytes.extend_from_slice(s.as_bytes());
                Some(bytes)
            } else {
                let mut buf = [0u8; 4];
                let s = c.encode_utf8(&mut buf);
                Some(s.as_bytes().to_vec())
            }
        }
        KeyCode::Enter => Some(vec![b'\r']),
        KeyCode::Backspace => Some(vec![0x7f]),
        KeyCode::Tab => Some(vec![b'\t']),
        KeyCode::BackTab => Some(vec![0x1b, b'[', b'Z']),
        KeyCode::Esc => Some(vec![0x1b]),
        KeyCode::Up => {
            if app_cursor {
                Some(b"\x1bOA".to_vec())
            } else {
                Some(b"\x1b[A".to_vec())
            }
        }
        KeyCode::Down => {
            if app_cursor {
                Some(b"\x1bOB".to_vec())
            } else {
                Some(b"\x1b[B".to_vec())
            }
        }
        KeyCode::Right => {
            if app_cursor {
                Some(b"\x1bOC".to_vec())
            } else {
                Some(b"\x1b[C".to_vec())
            }
        }
        KeyCode::Left => {
            if app_cursor {
                Some(b"\x1bOD".to_vec())
            } else {
                Some(b"\x1b[D".to_vec())
            }
        }
        KeyCode::Home => Some(b"\x1b[H".to_vec()),
        KeyCode::End => Some(b"\x1b[F".to_vec()),
        KeyCode::PageUp => Some(b"\x1b[5~".to_vec()),
        KeyCode::PageDown => Some(b"\x1b[6~".to_vec()),
        KeyCode::Insert => Some(b"\x1b[2~".to_vec()),
        KeyCode::Delete => Some(b"\x1b[3~".to_vec()),
        KeyCode::F(n) => {
            let seq = match n {
                1 => b"\x1bOP".to_vec(),
                2 => b"\x1bOQ".to_vec(),
                3 => b"\x1bOR".to_vec(),
                4 => b"\x1bOS".to_vec(),
                5 => b"\x1b[15~".to_vec(),
                6 => b"\x1b[17~".to_vec(),
                7 => b"\x1b[18~".to_vec(),
                8 => b"\x1b[19~".to_vec(),
                9 => b"\x1b[20~".to_vec(),
                10 => b"\x1b[21~".to_vec(),
                11 => b"\x1b[23~".to_vec(),
                12 => b"\x1b[24~".to_vec(),
                _ => return None,
            };
            Some(seq)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use parking_lot::Mutex;

    use super::translate_key_event;

    const NONE: KeyModifiers = KeyModifiers::NONE;
    const CTRL: KeyModifiers = KeyModifiers::CONTROL;
    const ALT: KeyModifiers = KeyModifiers::ALT;

    /// What the PTY gets for the key, with the program in application cursor key mode or not.
    fn translate(code: KeyCode, modifiers: KeyModifiers, app_cursor: bool) -> Option<Vec<u8>> {
        let parser = Arc::new(Mutex::new(vt100::Parser::new(24, 80, 0)));
        if app_cursor {
            // DECCKM, which programs such as vim and less set
            parser.lock().process(b"\x1b[?1h");
        }
        translate_key_event(&KeyEvent::new(code, modifiers), &parser)
    }

    #[test]
    fn keys_become_xterm_bytes() {
        let cases: &[(KeyCode, KeyModifiers, &[u8])] = &[
            (KeyCode::Char('a'), NONE, b"a"),
            (KeyCode::Char('A'), KeyModifiers::SHIFT, b"A"),
            (KeyCode::Char('é'), NONE, "é".as_bytes()),
            (KeyCode::Char('a'), CTRL, b"\x01"),
            (KeyCode::Char('c'), CTRL, b"\x03"),
            (KeyCode::Char('z'), CTRL, b"\x1a"),
            (KeyCode::Char('x'), ALT, b"\x1bx"),
            (KeyCode::Char('é'), ALT, "\x1bé".as_bytes()),
            (KeyCode::Enter, NONE, b"\r"),
            (KeyCode::Backspace, NONE, b"\x7f"),
            (KeyCode::Tab, NONE, b"\t"),
            (KeyCode::BackTab, KeyModifiers::SHIFT, b"\x1b[Z"),
            (KeyCode::Esc, NONE, b"\x1b"),
            (KeyCode::Home, NONE, b"\x1b[H"),
            (KeyCode::End, NONE, b"\x1b[F"),
            (KeyCode::PageUp, NONE, b"\x1b[5~"),
            (KeyCode::PageDown, NONE, b"\x1b[6~"),
            (KeyCode::Insert, NONE, b"\x1b[2~"),
            (KeyCode::Delete, NONE, b"\x1b[3~"),
        ];
        for &(code, modifiers, bytes) in cases {
            assert_eq!(
                translate(code, modifiers, false).as_deref(),
                Some(bytes),
                "{code:?} with {modifiers:?}"
            );
        }
    }

    #[test]
    fn function_keys_become_xterm_bytes() {
        let expected: [&[u8]; 12] = [
            b"\x1bOP",
            b"\x1bOQ",
            b"\x1bOR",
            b"\x1bOS",
            b"\x1b[15~",
            b"\x1b[17~",
            b"\x1b[18~",
            b"\x1b[19~",
            b"\x1b[20~",
            b"\x1b[21~",
            b"\x1b[23~",
            b"\x1b[24~",
        ];
        for (n, bytes) in (1..).zip(expected) {
            assert_eq!(
                translate(KeyCode::F(n), NONE, false).as_deref(),
                Some(bytes),
                "F{n}"
            );
        }
    }

    #[test]
    fn arrows_follow_the_cursor_key_mode() {
        let cases: [(KeyCode, &[u8], &[u8]); 4] = [
            (KeyCode::Up, b"\x1b[A", b"\x1bOA"),
            (KeyCode::Down, b"\x1b[B", b"\x1bOB"),
            (KeyCode::Right, b"\x1b[C", b"\x1bOC"),
            (KeyCode::Left, b"\x1b[D", b"\x1bOD"),
        ];
        for (code, normal, application) in cases {
            assert_eq!(translate(code, NONE, false).as_deref(), Some(normal));
            assert_eq!(translate(code, NONE, true).as_deref(), Some(application));
        }
    }

    #[test]
    fn keys_without_a_sequence_are_not_forwarded() {
        for (code, modifiers) in [
            // Only letters have a control code here
            (KeyCode::Char('1'), CTRL),
            (KeyCode::F(13), NONE),
            (KeyCode::CapsLock, NONE),
            (KeyCode::Null, NONE),
        ] {
            assert_eq!(
                translate(code, modifiers, false),
                None,
                "{code:?} with {modifiers:?}"
            );
        }
    }
}
