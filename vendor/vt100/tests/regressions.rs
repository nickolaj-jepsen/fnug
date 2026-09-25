use fnug_vt100 as vt100;

#[test]
fn decrc_after_shrink_clamps_saved_cursor() {
    let mut parser = vt100::Parser::new(24, 80, 100);
    parser.process(b"\x1b[20;70H\x1b7");
    parser.set_size(10, 40);
    parser.process(b"\x1b8X");

    assert_eq!(parser.screen().cell(9, 39).unwrap().contents(), "X");
}

#[test]
fn decrc_after_shrink_clamps_saved_row() {
    let mut parser = vt100::Parser::new(24, 80, 100);
    parser.process(b"\x1b[20;10H\x1b7");
    parser.set_size(10, 40);
    parser.process(b"\x1b8");

    assert_eq!(parser.screen().cursor_position(), (9, 9));
}

#[test]
fn alternate_screen_exit_after_shrink_clamps_saved_cursor() {
    let mut parser = vt100::Parser::new(24, 80, 100);
    parser.process(b"\x1b[20;70H\x1b[?1049h");
    parser.set_size(10, 40);
    parser.process(b"\x1b[?1049lX");

    assert!(!parser.screen().alternate_screen());
    assert_eq!(parser.screen().cell(9, 39).unwrap().contents(), "X");
}

/// Every row currently in scrollback, oldest first.
fn scrollback_rows(parser: &mut vt100::Parser) -> Vec<String> {
    let (_, cols) = parser.screen().size();
    let len = parser.screen().scrollback_len();
    let rows = (1..=len)
        .rev()
        .map(|offset| {
            parser.set_scrollback(offset);
            parser.screen().rows(0, cols).next().unwrap()
        })
        .collect();
    parser.set_scrollback(0);
    rows
}

fn numbered_lines(range: std::ops::RangeInclusive<usize>) -> String {
    range.map(|i| format!("l{i}\r\n")).collect()
}

#[test]
fn shrink_keeps_newest_rows() {
    let mut parser = vt100::Parser::new(10, 20, 100);
    parser.process(numbered_lines(1..=12).as_bytes());
    parser.process(b"prompt");
    parser.set_size(5, 20);

    assert_eq!(
        scrollback_rows(&mut parser),
        ["l1", "l2", "l3", "l4", "l5", "l6", "l7", "l8"]
    );
    assert_eq!(parser.screen().contents(), "l9\nl10\nl11\nl12\nprompt");
    assert_eq!(parser.screen().cursor_position(), (4, 6));
}

#[test]
fn shrink_moves_saved_cursor() {
    let mut parser = vt100::Parser::new(10, 20, 100);
    parser.process(numbered_lines(1..=12).as_bytes());
    parser.process(b"prompt");
    // save the cursor at the start of l10 (screen row 7), then go back
    parser.process(b"\x1b[7;1H\x1b7\x1b[10;7H");
    parser.set_size(5, 20);
    parser.process(b"\x1b8");

    assert_eq!(parser.screen().cursor_position(), (1, 0));
    assert_eq!(parser.screen().rows(0, 20).nth(1).unwrap(), "l10");
}

#[test]
fn shrink_keeps_scrollback_view_anchored() {
    let mut parser = vt100::Parser::new(10, 20, 100);
    parser.process(numbered_lines(1..=12).as_bytes());
    parser.process(b"prompt");
    parser.set_scrollback(1);
    parser.set_size(5, 20);

    assert_eq!(parser.screen().scrollback(), 6);
    assert_eq!(parser.screen().rows(0, 20).next().unwrap(), "l3");
}

#[test]
fn shrink_with_scroll_region_truncates() {
    let mut parser = vt100::Parser::new(10, 20, 100);
    parser.process(numbered_lines(1..=9).as_bytes());
    parser.process(b"l10\x1b[2;9r\x1b[10;4H");
    parser.set_size(5, 20);

    assert_eq!(parser.screen().scrollback_len(), 0);
    assert_eq!(parser.screen().contents(), "l1\nl2\nl3\nl4\nl5");
    assert_eq!(parser.screen().cursor_position(), (4, 3));
}

#[test]
fn shrink_alternate_screen_drops_top_rows() {
    let mut parser = vt100::Parser::new(10, 20, 100);
    parser.process(b"\x1b[?1049h");
    parser.process(numbered_lines(1..=9).as_bytes());
    parser.process(b"l10");
    parser.set_size(5, 20);

    assert_eq!(parser.screen().contents(), "l6\nl7\nl8\nl9\nl10");
    parser.process(b"\x1b[?1049l");
    assert_eq!(parser.screen().scrollback_len(), 0);
}

#[test]
fn scrollback_rows_are_trimmed() {
    let mut parser = vt100::Parser::new(24, 200, 1000);
    parser.process(numbered_lines(1..=100).as_bytes());

    assert!(parser.screen().cell(0, 150).is_some());
    parser.set_scrollback(50);
    assert_eq!(parser.screen().cell(0, 0).unwrap().contents(), "l");
    assert!(parser.screen().cell(0, 3).is_none());
    assert!(parser.screen().cell(0, 150).is_none());
}

#[test]
fn trim_keeps_wide_and_bg_cells() {
    let mut parser = vt100::Parser::new(2, 20, 10);
    parser.process("ab中\r\nx\x1b[41m\x1b[K\x1b[m\r\n\r\n".as_bytes());
    parser.set_scrollback(2);

    let screen = parser.screen();
    assert_eq!(screen.cell(0, 2).unwrap().contents(), "中");
    assert!(screen.cell(0, 3).unwrap().is_wide_continuation());
    assert!(screen.cell(0, 4).is_none());
    assert_eq!(screen.cell(1, 19).unwrap().bgcolor(), vt100::Color::Idx(1));
}

#[test]
fn trimmed_empty_row_keeps_one_cell() {
    let mut parser = vt100::Parser::new(2, 20, 10);
    parser.process(b"\r\n\r\n");
    parser.set_scrollback(1);

    assert!(parser.screen().cell(0, 0).is_some());
    assert!(parser.screen().cell(0, 1).is_none());
}

/// 100 short lines in scrollback under a screen of long lines.
fn short_scrollback_long_screen() -> vt100::Parser {
    let mut parser = vt100::Parser::new(24, 200, 1000);
    parser.process(numbered_lines(1..=100).as_bytes());
    for i in 1..=23 {
        parser.process(format!("long{i} {}\r\n", "x".repeat(150)).as_bytes());
    }
    parser
}

#[test]
fn trimmed_rows_formatted_no_panic() {
    let mut parser = short_scrollback_long_screen();
    let live = parser.screen().clone();
    parser.set_scrollback(50);
    let screen = parser.screen();

    assert!(!screen.contents_formatted().is_empty());
    assert!(!screen.contents_diff(&live).is_empty());
    assert!(!live.contents_diff(screen).is_empty());
    assert_eq!(screen.rows_formatted(0, 200).count(), 24);
    assert_eq!(screen.rows_formatted(150, 50).count(), 24);
    assert_eq!(screen.rows_diff(&live, 150, 50).count(), 24);
    assert_eq!(live.rows_diff(screen, 150, 50).count(), 24);
}

#[test]
fn trimmed_rows_diff_roundtrips() {
    let mut parser = short_scrollback_long_screen();
    let live = parser.screen().clone();
    parser.set_scrollback(50);
    let scrolled = parser.screen().clone();

    for (from, to) in [(&live, &scrolled), (&scrolled, &live)] {
        let mut replay = vt100::Parser::new(24, 200, 0);
        replay.process(&from.contents_formatted());
        replay.process(&to.contents_diff(from));
        assert_eq!(replay.screen().contents(), to.contents());
    }
}

#[test]
fn all_contents_whole_buffer() {
    let mut parser = vt100::Parser::new(5, 20, 100);
    parser.process(numbered_lines(1..=20).as_bytes());
    parser.process(b"FINAL");
    parser.set_scrollback(3);

    let contents = parser.screen().all_contents();
    let lines: Vec<_> = contents.lines().collect();
    assert_eq!(lines.len(), 21);
    assert_eq!(lines[0], "l1");
    assert_eq!(lines[20], "FINAL");
    assert_eq!(parser.screen().scrollback(), 3);
}

#[test]
fn all_contents_joins_wrap_across_boundary() {
    let mut parser = vt100::Parser::new(3, 10, 100);
    parser.process(b"0123456789abcdef\r\nx\r\n");

    assert_eq!(parser.screen().scrollback_len(), 1);
    assert_eq!(parser.screen().all_contents(), "0123456789abcdef\nx");
}

#[test]
fn all_contents_keeps_wider_old_rows() {
    let mut parser = vt100::Parser::new(3, 20, 100);
    parser.process(b"abcdefghijklmnop\r\nl2\r\nl3\r\nl4");
    parser.set_size(3, 5);

    assert_eq!(
        parser.screen().all_contents(),
        "abcdefghijklmnop\nl2\nl3\nl4"
    );
}

#[test]
fn all_contents_alt_screen() {
    let mut parser = vt100::Parser::new(3, 10, 100);
    parser.process(numbered_lines(1..=9).as_bytes());
    parser.process(b"l10\x1b[?1049h\x1b[Halt");

    assert_eq!(parser.screen().all_contents(), "alt");
    parser.process(b"\x1b[?1049l");
    assert_eq!(parser.screen().all_contents().lines().count(), 10);
}

#[test]
fn cell_chars_combining() {
    let mut parser = vt100::Parser::new(2, 10, 0);
    parser.process("e\u{301}中".as_bytes());
    let screen = parser.screen();

    let combined: Vec<_> = screen.cell(0, 0).unwrap().chars().collect();
    assert_eq!(combined, ['e', '\u{301}']);
    assert_eq!(screen.cell(0, 1).unwrap().chars().collect::<String>(), "中");
    assert_eq!(screen.cell(0, 3).unwrap().chars().count(), 0);
}

fn attrs_at(screen: &vt100::Screen, col: u16) -> (bool, bool, bool, vt100::Color) {
    let cell = screen.cell(0, col).unwrap();
    (
        cell.bold(),
        cell.dim(),
        cell.strikethrough(),
        cell.fgcolor(),
    )
}

#[test]
fn sgr_dim_strike() {
    let mut parser = vt100::Parser::new(2, 20, 0);
    parser.process(b"\x1b[2mD\x1b[9mS\x1b[0;2;9;31mX");
    let screen = parser.screen();

    let default = vt100::Color::Default;
    assert_eq!(attrs_at(screen, 0), (false, true, false, default));
    assert_eq!(attrs_at(screen, 1), (false, true, true, default));
    assert_eq!(
        attrs_at(screen, 2),
        (false, true, true, vt100::Color::Idx(1))
    );
    assert!(screen.dim());
    assert!(screen.strikethrough());
}

#[test]
fn sgr_22_clears_dim() {
    let mut parser = vt100::Parser::new(2, 20, 0);
    parser.process(b"\x1b[1;2mA\x1b[22mB");
    let screen = parser.screen();

    let default = vt100::Color::Default;
    assert_eq!(attrs_at(screen, 0), (true, true, false, default));
    assert_eq!(attrs_at(screen, 1), (false, false, false, default));
    assert!(!screen.bold());
    assert!(!screen.dim());
}

#[test]
fn sgr_29_clears_strike() {
    let mut parser = vt100::Parser::new(2, 20, 0);
    parser.process(b"\x1b[9mA\x1b[29mB");
    let screen = parser.screen();

    assert!(screen.cell(0, 0).unwrap().strikethrough());
    assert!(!screen.cell(0, 1).unwrap().strikethrough());
    assert!(!screen.strikethrough());
}

/// Every intensity and strikethrough transition, one per cell.
const INTENSITY_STEPS: &[u8] =
    b"\x1b[1mB\x1b[2mX\x1b[22;2mD\x1b[1mX\x1b[22;1mB\x1b[22mN\x1b[9mS\x1b[2mX\x1b[29mD\x1b[mN";

#[test]
fn formatted_roundtrips_dim() {
    let mut parser = vt100::Parser::new(2, 20, 0);
    let empty = parser.screen().clone();
    parser.process(INTENSITY_STEPS);
    let screen = parser.screen();

    let mut formatted = vt100::Parser::new(2, 20, 0);
    formatted.process(&screen.contents_formatted());
    let mut diffed = vt100::Parser::new(2, 20, 0);
    diffed.process(&empty.contents_formatted());
    diffed.process(&screen.contents_diff(&empty));

    for replay in [formatted.screen(), diffed.screen()] {
        for col in 0..10 {
            assert_eq!(attrs_at(replay, col), attrs_at(screen, col), "col {col}");
        }
    }
    // bold to dim-only must reset intensity, since no SGR turns off only bold
    assert!(screen
        .contents_formatted()
        .windows(7)
        .any(|w| w == b"\x1b[22;2m"));
}
