//! Testable view layer: grid geometry plus rendering from plain view models.
//!
//! Content generation stays separate from frame rendering: [`render_grid`]
//! takes snapshots, so the whole grid is assertable through a test backend
//! without a terminal.

use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::{
    pty::{CellColor, CellFormat, FormattedCell},
    safe_text, theme,
};

/// One styled run of pane text.
#[derive(Clone, Debug)]
pub struct SpanView {
    pub text: String,
    pub style: Style,
}

/// Map one parsed cell format to a render style. App colors pass through
/// raw; the semantic theme stays chrome-only by design.
pub fn style_for(format: &CellFormat) -> Style {
    let mut style = Style::default();
    if let Some(fg) = map_color(format.fg) {
        style = style.fg(fg);
    }
    if let Some(bg) = map_color(format.bg) {
        style = style.bg(bg);
    }
    if format.bold {
        style = style.add_modifier(Modifier::BOLD);
    }
    if format.italic {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if format.underline {
        style = style.add_modifier(Modifier::UNDERLINED);
    }
    if format.inverse {
        style = style.add_modifier(Modifier::REVERSED);
    }
    style
}

/// Encode one cell for display and attach its style. Cells never contain
/// newlines (rows are structural now), so single-line encoding is exact.
pub fn span_for(cell: &FormattedCell) -> SpanView {
    SpanView {
        text: safe_text::encode_for_display(&cell.text),
        style: style_for(&cell.format),
    }
}

fn map_color(color: CellColor) -> Option<Color> {
    match color {
        CellColor::Default => None,
        CellColor::Indexed(i) => Some(Color::Indexed(i)),
        CellColor::Rgb(r, g, b) => Some(Color::Rgb(r, g, b)),
    }
}

/// One pane's renderable snapshot.
pub struct PaneView {
    pub title: String,
    pub lines: Vec<Vec<SpanView>>,
    pub live: bool,
    pub focused: bool,
    /// Visible cursor as 0-based (row, col) in terminal-grid coordinates.
    pub cursor: Option<(u16, u16)>,
}

/// Translate outer 0-based mouse coordinates into 1-based pane-grid cells.
/// `None` when the event lands on borders, the status bar, or outside the
/// pane: chrome keeps those events.
pub fn translate_mouse(area: Rect, col: u16, row: u16) -> Option<(u16, u16)> {
    if area.width < 3 || area.height < 3 {
        return None;
    }
    let (ix, iy) = (area.x + 1, area.y + 1);
    let (iw, ih) = (area.width - 2, area.height - 2);
    if col < ix || row < iy || col >= ix + iw || row >= iy + ih {
        return None;
    }
    Some((col - ix + 1, row - iy + 1))
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
        let text = Text::from(
            view.lines
                .iter()
                .map(|line| {
                    Line::from(
                        line.iter()
                            .map(|span| Span::styled(span.text.clone(), span.style))
                            .collect::<Vec<_>>(),
                    )
                })
                .collect::<Vec<_>>(),
        );
        frame.render_widget(Paragraph::new(text).block(block), *rect);
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
            lines: body
                .split('\n')
                .map(|line| {
                    vec![SpanView {
                        text: line.to_string(),
                        style: Style::default(),
                    }]
                })
                .collect(),
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
    fn style_for_maps_sgr_attributes() {
        use crate::pty::{CellColor, CellFormat};
        let f = CellFormat {
            fg: CellColor::Indexed(1),
            bg: CellColor::Rgb(10, 20, 30),
            bold: true,
            italic: false,
            underline: true,
            inverse: true,
        };
        let s = style_for(&f);
        assert_eq!(s.fg, Some(Color::Indexed(1)));
        assert_eq!(s.bg, Some(Color::Rgb(10, 20, 30)));
        assert!(s.add_modifier.contains(Modifier::BOLD));
        assert!(s.add_modifier.contains(Modifier::UNDERLINED));
        assert!(s.add_modifier.contains(Modifier::REVERSED));
        assert!(!s.add_modifier.contains(Modifier::ITALIC));
        let plain = style_for(&CellFormat::plain());
        assert_eq!(plain.fg, None);
        assert_eq!(plain.bg, None);
        assert!(plain.add_modifier.is_empty());
    }

    #[test]
    fn render_shows_sgr_colors() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let red = Style::default().fg(Color::Indexed(1));
        let view = PaneView {
            title: "sh".to_string(),
            lines: vec![vec![SpanView {
                text: "RED".to_string(),
                style: red,
            }]],
            live: true,
            focused: true,
            cursor: None,
        };
        terminal
            .draw(|f| render_grid(f, area(), &[view], "status"))
            .unwrap();
        let buf = terminal.backend().buffer();
        let w = buf.area.width as usize;
        let cell = &buf.content[1 * w + 1];
        assert_eq!(cell.symbol(), "R");
        assert_eq!(cell.fg, Color::Indexed(1));
    }

    #[test]
    fn mouse_translates_to_pane_cells() {
        let area = Rect::new(0, 0, 80, 24);
        assert_eq!(translate_mouse(area, 1, 1), Some((1, 1)));
        assert_eq!(translate_mouse(area, 10, 5), Some((10, 5)));
        // Borders and outside belong to chrome, not the pane.
        assert_eq!(translate_mouse(area, 0, 0), None);
        assert_eq!(translate_mouse(area, 79, 23), None);
        assert_eq!(translate_mouse(area, 200, 200), None);
        assert_eq!(translate_mouse(Rect::new(40, 0, 40, 24), 41, 1), Some((1, 1)));
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
