//! Forge regression tests for the scrollback viewport.
//!
//! A viewport offset deeper than the screen height used to underflow
//! `Grid::visible_rows` (`attempt to subtract with overflow`), panicking
//! the render path when wheeling deep into a long scrollback.

fn feed_history(parser: &mut vt100::Parser, lines: usize) {
    for i in 0..lines {
        parser.process(format!("line-{i}\n").as_bytes());
    }
}

#[test]
fn offset_beyond_screen_rows_shows_older_history() {
    let mut parser = vt100::Parser::new(24, 80, 1000);
    feed_history(&mut parser, 200);
    // 100 rows up: deeper than the 24-row screen, the shape that panicked.
    parser.screen_mut().set_scrollback(100);
    let rows: Vec<String> = parser.screen().rows(0, 80).collect();
    assert_eq!(rows.len(), 24, "viewport always fills the screen");
    assert!(
        rows.iter().any(|r| r.contains("line-")),
        "viewport shows history, not blanks"
    );
    assert!(
        !rows.iter().any(|r| r.contains("line-199")),
        "live tail scrolled out of view"
    );
}

#[test]
fn offset_past_all_history_clamps_without_panic() {
    let mut parser = vt100::Parser::new(24, 80, 1000);
    feed_history(&mut parser, 200);
    parser.screen_mut().set_scrollback(100_000);
    let rows: Vec<String> = parser.screen().rows(0, 80).collect();
    assert_eq!(rows.len(), 24, "viewport always fills the screen");
}

#[test]
fn live_tail_unchanged_at_zero_offset() {
    let mut parser = vt100::Parser::new(24, 80, 1000);
    feed_history(&mut parser, 200);
    parser.screen_mut().set_scrollback(100);
    parser.screen_mut().set_scrollback(0);
    let rows: Vec<String> = parser.screen().rows(0, 80).collect();
    assert_eq!(rows.len(), 24);
    assert!(
        rows.iter().any(|r| r.contains("line-199")),
        "reset viewport returns to the live tail"
    );
}
