//! Testable view layer: grid geometry plus rendering from plain view models.
//!
//! Content generation stays separate from frame rendering: [`render_grid`]
//! takes snapshots, so the whole grid is assertable through a test backend
//! without a terminal.

use ratatui::layout::{Position, Rect};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::{safe_text, theme};

/// One pane's renderable snapshot.
pub struct PaneView {
    pub title: String,
    pub body: String,
    pub live: bool,
    pub focused: bool,
    /// Visible cursor as 0-based (row, col) in terminal-grid coordinates.
    pub cursor: Option<(u16, u16)>,
}

/// Translate a terminal-grid cursor into outer-frame coordinates, clamped
/// inside the pane's borders. `None` when the pane hides its cursor or the
/// pane is too small for an inner area.
pub fn cursor_screen_pos(area: Rect, cursor: Option<(u16, u16)>) -> Option<Position> {
    let (row, col) = cursor?;
    if area.width < 3 || area.height < 3 {
        return None;
    }
    let x = area
        .x
        .saturating_add(1)
        .saturating_add(col)
        .min(area.x + area.width - 2);
    let y = area
        .y
        .saturating_add(1)
        .saturating_add(row)
        .min(area.y + area.height - 2);
    Some(Position::new(x, y))
}

/// Split `area` for up to nine sessions, reserving the last row for the
/// status bar: 1x1, 2x1, 2x2, 3x2, then 3x3. Extra sessions are not shown.
pub fn grid_areas(count: usize, area: Rect) -> Vec<Rect> {
    let (cols, rows) = grid_shape(count);
    if cols == 0 || rows == 0 {
        return Vec::new();
    }
    let grid_h = area.height.saturating_sub(1); // last row belongs to status
    let (w, h) = (area.width / cols, grid_h / rows);
    (0..count.min(9))
        .map(|i| {
            let i = i as u16;
            Rect::new(area.x + (i % cols) * w, area.y + (i / cols) * h, w, h)
        })
        .collect()
}

/// Render the session grid plus a one-row status bar. Titles and bodies are
/// untrusted PTY output, so both pass through display encoding: raw escape
/// sequences must never reach the outer terminal.
pub fn render_grid(frame: &mut Frame, area: Rect, panes: &[PaneView], status: &str) {
    if panes.is_empty() {
        let hint = Paragraph::new("No sessions yet — press Ctrl-b c to create one.\nCtrl-b q quits.")
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(theme::style(theme::Role::BorderUnfocused))
                    .title(" forge "),
            );
        frame.render_widget(hint, area);
    }
    let areas = grid_areas(panes.len(), area);
    // Ratatui places a single hardware cursor per frame: the focused pane
    // owns it, since keyboard input goes there.
    let mut focused_cursor = None;
    for (view, rect) in panes.iter().zip(areas.iter()) {
        if view.focused {
            focused_cursor = cursor_screen_pos(*rect, view.cursor);
        }
        let (glyph, _role) = if view.live {
            theme::status_glyph_running()
        } else {
            theme::status_glyph_exited()
        };
        let border = if view.focused {
            theme::Role::BorderFocused
        } else {
            theme::Role::BorderUnfocused
        };
        let title = format!("{} {} ", glyph, safe_text::encode_for_display(&view.title));
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme::style(border))
            .title(title);
        frame.render_widget(
            Paragraph::new(safe_text::encode_multiline_for_display(&view.body)).block(block),
            *rect,
        );
    }
    if let Some(pos) = focused_cursor {
        frame.set_cursor_position(pos);
    }
    if area.height > 0 {
        let bar = Rect::new(area.x, area.y + area.height - 1, area.width, 1);
        frame.render_widget(Paragraph::new(status), bar);
    }
}

fn grid_shape(count: usize) -> (u16, u16) {
    match count {
        0 => (0, 0),
        1 => (1, 1),
        2 => (2, 1),
        3 | 4 => (2, 2),
        5 | 6 => (3, 2),
        _ => (3, 3),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn area() -> Rect {
        Rect::new(0, 0, 80, 24)
    }

    #[test]
    fn grid_scales_with_count() {
        assert_eq!(grid_areas(0, area()), vec![]);
        assert_eq!(grid_areas(1, area()).len(), 1);
        let two = grid_areas(2, area());
        assert_eq!(two.len(), 2);
        assert_eq!(two[0].width, 40);
        assert_eq!(two[1].x, 40);
        let four = grid_areas(4, area());
        assert_eq!(four.len(), 4);
        assert_eq!((four[0].width, four[0].height), (40, 11));
        assert_eq!(grid_areas(6, area()).len(), 6);
        assert_eq!(grid_areas(9, area()).len(), 9);
        // Beyond capacity the grid caps instead of overflowing.
        assert_eq!(grid_areas(12, area()).len(), 9);
    }

    #[test]
    fn grid_tiles_without_overlap() {
        for n in 1..=9 {
            let areas = grid_areas(n, area());
            assert_eq!(areas.len(), n);
            for (i, a) in areas.iter().enumerate() {
                assert!(a.x + a.width <= 80 && a.y + a.height <= 24, "pane {i} fits");
                for b in &areas[i + 1..] {
                    assert!(!a.intersects(*b), "panes do not overlap");
                }
            }
        }
    }

    fn buffer_text(terminal: &Terminal<TestBackend>) -> String {
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol().to_string())
            .collect()
    }

    fn pane(title: &str, body: &str, live: bool) -> PaneView {
        PaneView {
            title: title.to_string(),
            body: body.to_string(),
            live,
            focused: true,
            cursor: None,
        }
    }

    #[test]
    fn render_shows_titles_bodies_and_status() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                render_grid(
                    f,
                    area(),
                    &[pane("agent-1", "hello out", true)],
                    "2 sessions | prefix Ctrl-b",
                )
            })
            .unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("agent-1"), "title visible");
        assert!(text.contains("hello out"), "body visible");
        assert!(text.contains("prefix Ctrl-b"), "status visible");
    }

    fn buffer_rows(terminal: &Terminal<TestBackend>) -> Vec<String> {
        let buf = terminal.backend().buffer();
        let w = buf.area.width as usize;
        buf.content
            .chunks(w)
            .map(|row| row.iter().map(|c| c.symbol().to_string()).collect())
            .collect()
    }

    #[test]
    fn multiline_body_renders_across_rows() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                render_grid(f, area(), &[pane("sh", "line1\nline2", true)], "status")
            })
            .unwrap();
        let rows = buffer_rows(&terminal);
        let r1 = rows.iter().position(|r| r.contains("line1")).expect("line1 visible");
        let r2 = rows.iter().position(|r| r.contains("line2")).expect("line2 visible");
        assert_eq!(r2, r1 + 1, "row break preserved: {rows:?}");
        assert!(rows.iter().all(|r| !r.contains('⏎')), "no flattened newlines");
    }

    #[test]
    fn focused_pane_cursor_is_placed_inside_borders() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut p = pane("sh", "hi", true);
        p.cursor = Some((2, 5));
        terminal
            .draw(|f| render_grid(f, area(), &[p], "status"))
            .unwrap();
        terminal.backend_mut().assert_cursor_position(Position::new(6, 3));
    }

    #[test]
    fn cursor_mapping_clamps_and_hides() {
        let full = Rect::new(0, 0, 80, 24);
        assert_eq!(cursor_screen_pos(full, Some((0, 0))), Some(Position::new(1, 1)));
        assert_eq!(cursor_screen_pos(full, None), None);
        assert_eq!(
            cursor_screen_pos(full, Some((500, 500))),
            Some(Position::new(78, 22))
        );
        assert_eq!(cursor_screen_pos(Rect::new(0, 0, 2, 2), Some((0, 0))), None);
    }

    #[test]
    fn empty_grid_explains_itself() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| render_grid(f, area(), &[], "status"))
            .unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("Ctrl-b c"), "empty state guides: {text:?}");
    }

    #[test]
    fn render_marks_exited_panes() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                render_grid(
                    f,
                    area(),
                    &[pane("old", "bye", false), pane("new", "hi", true)],
                    "status",
                )
            })
            .unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains('×'), "exited glyph visible");
        assert!(text.contains('●'), "running glyph visible");
        let _ = theme::style(theme::Role::Text);
    }
}
