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
