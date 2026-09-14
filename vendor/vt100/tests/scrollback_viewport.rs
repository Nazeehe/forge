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

fn feed_numbered(parser: &mut vt100::Parser, lines: usize) {
    // CRLF: a bare LF keeps the column and staircases across the row,
    // wrapping instead of scrolling the region one line per line.
    for i in 0..lines {
        parser.process(format!("hist-{i:03}\r\n").as_bytes());
    }
}

fn visible_text(parser: &vt100::Parser) -> String {
    parser
        .screen()
        .rows(0, 80)
        .collect::<Vec<String>>()
        .join("\n")
}

#[test]
fn top_anchored_region_scroll_feeds_scrollback() {
    // Codex drives its transcript through a top-anchored DECSTBM region;
    // lines scrolling off the region top must land in scrollback so a
    // wheel viewport can reveal them (upstream/xterm discards them).
    let mut parser = vt100::Parser::new(24, 80, 1000);
    parser.process(b"\x1b[1;10r");
    feed_numbered(&mut parser, 30);
    parser.process(b"\x1b[r");
    // 30 lines through a 10-row region: 20 preserved, so offset 20
    // leaves the whole live tail out of view. (hist-000 itself sits
    // one row above the 24-row window; hist-001 is the oldest visible.)
    parser.screen_mut().set_scrollback(20);
    let text = visible_text(&parser);
    assert!(
        text.contains("hist-001"),
        "region-scrolled history in view: {text:?}"
    );
    assert!(
        !text.contains("hist-029"),
        "live tail scrolled out of view: {text:?}"
    );
}

#[test]
fn bottom_anchored_region_scroll_still_discards() {
    // Churn below a lowered scroll top (composer/status areas) is redraw
    // artifact, not history: it must not pollute scrollback.
    let mut parser = vt100::Parser::new(24, 80, 1000);
    parser.process(b"\x1b[15;24r");
    feed_numbered(&mut parser, 30);
    parser.process(b"\x1b[r");
    // No scrollback accumulated: any offset clamps to the live tail.
    parser.screen_mut().set_scrollback(100);
    let text = visible_text(&parser);
    assert!(
        text.contains("hist-029"),
        "live tail in view: {text:?}"
    );
    assert!(
        !text.contains("hist-000"),
        "discarded region churn must not resurface: {text:?}"
    );
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
