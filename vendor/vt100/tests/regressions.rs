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
