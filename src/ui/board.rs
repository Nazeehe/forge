//! UI board: views, layout, hit-testing, paint.

use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use super::text::truncate_cells;
use super::theme;

/// Kanban button under a sidebar click, if any.
pub fn board_at(area: &Rect, col: u16, row: u16) -> bool {
    area.width > 0 && row == area.y && col >= area.x && col < area.x + area.width
}

/// Kanban content model: the board view renders this, never live
/// state, so geometry and selection are assertable off-screen.
pub struct BoardCardView {
    pub title: String,
    pub meta: String,
    pub selected: bool,
}

pub struct BoardColumnView {
    pub name: String,
    pub wip_limit: u32,
    pub count: usize,
    pub selected: bool,
    pub cards: Vec<BoardCardView>,
}

pub struct BoardView {
    /// Block title, e.g. `"Kanban · team"`.
    pub title: String,
    pub columns: Vec<BoardColumnView>,
    pub focus_col: usize,
    /// Pinned footer hint lines (long or short by measured width).
    pub hints: Vec<String>,
    pub notice: Option<String>,
    /// Open text entry, rendered as the first footer line when set.
    pub draft: Option<String>,
    /// Set when no board exists: hint text replaces the columns.
    pub empty: Option<String>,
}

/// Board column geometry shared by paint and hit-testing: equal
/// columns when they fit (30+ cells each), otherwise the focused
/// column alone with its position.
enum BoardCells {
    Wide { col_w: u16 },
    Narrow,
}

fn board_cells(inner_w: u16, ncols: usize) -> BoardCells {
    if ncols > 0 && inner_w / ncols.max(1) as u16 >= 30 {
        BoardCells::Wide { col_w: inner_w / ncols as u16 }
    } else {
        BoardCells::Narrow
    }
}

fn board_header_text(col: &BoardColumnView, pos: Option<(usize, usize)>) -> String {
    let count = if col.wip_limit > 0 {
        format!("{}/{}", col.count, col.wip_limit)
    } else {
        format!("{}", col.count)
    };
    match pos {
        Some((i, n)) => format!("{} ({}) ({}/{})", col.name, count, i + 1, n),
        None => format!("{} ({})", col.name, count),
    }
}

/// Footer rows reserved at the bottom of the board view: draft
/// entry, notice, then hints. Paint and hit-testing share it so
/// clicks never land on a footer row.
fn board_footer_height(view: &BoardView) -> usize {
    view.hints.len()
        + usize::from(view.notice.is_some())
        + usize::from(view.draft.is_some())
}

/// Paint the global kanban view into the main area: bordered block,
/// column grid, pinned footer. `area` includes the border.
pub fn render_board(frame: &mut Frame, area: Rect, view: &BoardView) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(crate::ui::theme::border_type())
        .border_style(theme::style(theme::Role::BorderFocused))
        .title(format!(" {} ", view.title));
    let inner = block.inner(area);
    let mut lines: Vec<Line<'static>> = vec![Line::from("")];
    if let Some(empty) = view.empty.as_deref() {
        lines.push(Line::from(Span::styled(
            format!("  {empty}"),
            theme::style(theme::Role::Muted),
        )));
    } else {
        let ncols = view.columns.len();
        match board_cells(inner.width, ncols) {
            BoardCells::Wide { col_w } => {
                let cw = col_w.max(1) as usize;
                let mut header: Vec<Span<'static>> = Vec::new();
                for col in &view.columns {
                    let text = truncate_cells(&board_header_text(col, None), cw);
                    let pad = cw.saturating_sub(text.chars().count());
                    let style = if col.selected {
                        theme::style(theme::Role::Focus).add_modifier(Modifier::BOLD)
                    } else {
                        theme::style(theme::Role::Text)
                    };
                    header.push(Span::styled(format!(" {text}{}", " ".repeat(pad.saturating_sub(1))), style));
                }
                lines.push(Line::from(header));
                // Row-major: every screen row carries all columns, so a
                // click's x always resolves to the painted column.
                let depth = view.columns.iter().map(|c| c.cards.len()).max().unwrap_or(0);
                for k in 0..depth {
                    let mut titles: Vec<Span<'static>> = Vec::new();
                    let mut metas: Vec<Span<'static>> = Vec::new();
                    for col in &view.columns {
                        let (marker, title, style) = match col.cards.get(k) {
                            Some(card) if card.selected => {
                                ("▸ ", truncate_cells(&card.title, cw.saturating_sub(2)), theme::focus_row())
                            }
                            Some(card) => {
                                ("  ", truncate_cells(&card.title, cw.saturating_sub(2)), theme::style(theme::Role::Text))
                            }
                            None => ("  ", String::new(), theme::style(theme::Role::Text)),
                        };
                        let pad = cw.saturating_sub(2 + title.chars().count());
                        titles.push(Span::styled(
                            format!("{marker}{title}{}", " ".repeat(pad)),
                            style,
                        ));
                        let meta = col.cards.get(k).map(|c| c.meta.as_str()).unwrap_or("");
                        let meta = truncate_cells(meta, cw.saturating_sub(2));
                        let pad = cw.saturating_sub(2 + meta.chars().count());
                        metas.push(Span::styled(
                            format!("  {meta}{}", " ".repeat(pad)),
                            theme::style(theme::Role::Muted),
                        ));
                    }
                    lines.push(Line::from(titles));
                    lines.push(Line::from(metas));
                }
            }
            BoardCells::Narrow => {
                let w = inner.width.max(1) as usize;
                let focus = view.focus_col.min(ncols.saturating_sub(1));
                if let Some(col) = view.columns.get(focus) {
                    let text = truncate_cells(&board_header_text(col, Some((focus, ncols))), w.saturating_sub(1));
                    lines.push(Line::from(Span::styled(
                        format!(" {text}"),
                        theme::style(theme::Role::Focus).add_modifier(Modifier::BOLD),
                    )));
                    for card in &col.cards {
                        let (marker, style) = if card.selected {
                            ("▸ ", theme::focus_row())
                        } else {
                            ("  ", theme::style(theme::Role::Text))
                        };
                        lines.push(Line::from(Span::styled(
                            format!("{marker}{}", truncate_cells(&card.title, w.saturating_sub(2))),
                            style,
                        )));
                        lines.push(Line::from(Span::styled(
                            format!("  {}", truncate_cells(&card.meta, w.saturating_sub(2))),
                            theme::style(theme::Role::Muted),
                        )));
                    }
                }
            }
        }
    }
    if let Some(draft) = view.draft.as_deref() {
        lines.push(Line::from(Span::styled(
            format!("  {draft}"),
            theme::style(theme::Role::Focus).add_modifier(Modifier::BOLD),
        )));
    }
    if let Some(notice) = view.notice.as_deref() {
        lines.push(Line::from(Span::styled(
            format!("  {notice}"),
            theme::style(theme::Role::Warning),
        )));
    }
    for hint in &view.hints {
        lines.push(Line::from(Span::styled(
            format!("  {hint}"),
            theme::style(theme::Role::Muted),
        )));
    }
    // Pin the footer: pad short bodies so hints sit on the same rows
    // whatever the card count holds.
    let foot_h = board_footer_height(view);
    let body_h = inner.height as usize;
    if lines.len() < body_h && foot_h <= body_h {
        let pad = body_h - foot_h - (lines.len() - foot_h);
        for _ in 0..pad {
            lines.insert(lines.len() - foot_h, Line::from(""));
        }
    }
    lines.truncate(body_h);
    frame.render_widget(Paragraph::new(Text::from(lines)).block(block), area);
}

/// Board cell under a main-area click: `(column, card?)` — a header
/// or empty column space focuses the column, a card row the card.
/// Footer rows and the border are dead. Same layout as the paint.
pub fn board_cell_at(
    area: Rect,
    view: &BoardView,
    col: u16,
    row: u16,
) -> Option<(usize, Option<usize>)> {
    if view.columns.is_empty() {
        return None;
    }
    let inner = Rect::new(
        area.x.saturating_add(1),
        area.y.saturating_add(1),
        area.width.saturating_sub(2),
        area.height.saturating_sub(2),
    );
    if col < inner.x || col >= inner.right() || row < inner.y || row >= inner.bottom() {
        return None;
    }
    let foot_h = board_footer_height(view) as u16;
    if row >= inner.bottom().saturating_sub(foot_h) {
        return None;
    }
    let ncols = view.columns.len();
    let ci = match board_cells(inner.width, ncols) {
        BoardCells::Wide { col_w } => {
            let ci = (col - inner.x) / col_w.max(1);
            if ci as usize >= ncols {
                return None;
            }
            ci as usize
        }
        BoardCells::Narrow => view.focus_col.min(ncols.saturating_sub(1)),
    };
    let rel = row - inner.y;
    if rel < 2 {
        // Top pad and the header row focus the column, never a card.
        return Some((ci, None));
    }
    let k = (rel - 2) / 2;
    if (k as usize) < view.columns[ci].cards.len() {
        Some((ci, Some(k as usize)))
    } else {
        Some((ci, None))
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::render;
    use crate::ui::test_support::{buffer_rows, chrome};

    fn sample_board_view() -> BoardView {
        BoardView {
            title: "Kanban · team".to_string(),
            columns: vec![
                BoardColumnView {
                    name: "Backlog".to_string(),
                    wip_limit: 0,
                    count: 1,
                    selected: true,
                    cards: vec![BoardCardView {
                        title: "fix leak".to_string(),
                        meta: "high @kins 40%".to_string(),
                        selected: true,
                    }],
                },
                BoardColumnView {
                    name: "Doing".to_string(),
                    wip_limit: 3,
                    count: 1,
                    selected: false,
                    cards: vec![BoardCardView {
                        title: "ship pills".to_string(),
                        meta: "normal 0%".to_string(),
                        selected: false,
                    }],
                },
            ],
            focus_col: 0,
            hints: vec!["h/l/j/k move · Space shift · x done".to_string()],
            notice: None,
            draft: None,
            empty: None,
        }
    }

    #[test]
    fn board_renders_columns_selection_and_footer() {
        use ratatui::{backend::TestBackend, Terminal};
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        let view = sample_board_view();
        terminal.draw(|f| render_board(f, f.area(), &view)).unwrap();
        let rows = buffer_rows(&terminal);
        let text = rows.join("\n");
        assert!(text.contains("Kanban · team"), "titled block");
        assert!(text.contains("Backlog"), "column header");
        assert!(text.contains("Doing (1/3)"), "WIP count paints");
        assert!(text.contains("fix leak"), "card title");
        assert!(text.contains("high @kins 40%"), "card meta");
        assert!(text.contains("h/l/j/k move"), "footer hints pin");
        let buf = terminal.backend().buffer();
        let y = rows.iter().position(|r| r.contains("fix leak")).expect("card row paints");
        let byte_x = rows[y].find("fix leak").expect("card cell");
        let cell_x = rows[y][..byte_x].chars().count() as u16;
        assert!(
            buf[(cell_x.saturating_sub(2), y as u16)].modifier.contains(Modifier::REVERSED),
            "selected card reverses its marker"
        );
    }

    #[test]
    fn board_narrow_mode_shows_focused_column_only() {
        use ratatui::{backend::TestBackend, Terminal};
        let mut terminal = Terminal::new(TestBackend::new(50, 24)).unwrap();
        terminal.draw(|f| render_board(f, f.area(), &sample_board_view())).unwrap();
        let text = buffer_rows(&terminal).join("\n");
        assert!(text.contains("Backlog"), "focused column paints");
        assert!(text.contains("(1/2)"), "column position paints");
        assert!(!text.contains("ship pills"), "other columns stack away");
    }

    #[test]
    fn board_cell_hit_matches_paint() {
        use ratatui::{backend::TestBackend, Terminal};
        let area = Rect::new(0, 0, 100, 24);
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        let view = sample_board_view();
        terminal.draw(|f| render_board(f, area, &view)).unwrap();
        let rows = buffer_rows(&terminal);
        let y = rows.iter().position(|r| r.contains("fix leak")).expect("card paints");
        let byte_x = rows[y].find("fix leak").expect("card cell");
        let cell_x = rows[y][..byte_x].chars().count() as u16;
        assert_eq!(
            board_cell_at(area, &view, cell_x, y as u16),
            Some((0, Some(0))),
            "painted card clicks"
        );
        let hy = rows.iter().position(|r| r.contains("Doing")).expect("header paints");
        let hbyte = rows[hy].find("Doing").expect("header cell");
        let hcell = rows[hy][..hbyte].chars().count() as u16;
        assert_eq!(
            board_cell_at(area, &view, hcell, hy as u16),
            Some((1, None)),
            "header focuses the column"
        );
        assert_eq!(board_cell_at(area, &view, 50, 23), None, "footer is dead");
    }

    #[test]
    fn board_open_renders_board_in_main_area() {
        use ratatui::{backend::TestBackend, Terminal};
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        let mut c = chrome();
        c.board = Some(sample_board_view());
        terminal.draw(|f| render(f, f.area(), &[], &c)).unwrap();
        let text = buffer_rows(&terminal).join("\n");
        assert!(text.contains("Kanban · team"), "board owns the main area");
        assert!(text.contains("fix leak"), "cards paint, not panes");
    }

    #[test]
    fn board_renders_empty_hint_never_blank() {
        use ratatui::{backend::TestBackend, Terminal};
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        let view = BoardView {
            title: "Kanban".to_string(),
            columns: Vec::new(),
            focus_col: 0,
            hints: vec!["hint".to_string()],
            notice: None,
            draft: None,
            empty: Some("No boards yet.".to_string()),
        };
        terminal.draw(|f| render_board(f, f.area(), &view)).unwrap();
        assert!(buffer_rows(&terminal).join("\n").contains("No boards yet."));
    }

    #[test]
    fn board_draft_line_paints_above_hints() {
        use ratatui::{backend::TestBackend, Terminal};
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        let mut view = sample_board_view();
        view.draft = Some("New card: fix▌".to_string());
        terminal.draw(|f| render_board(f, f.area(), &view)).unwrap();
        let rows = buffer_rows(&terminal);
        let draft_y = rows.iter().position(|r| r.contains("New card:")).expect("draft paints");
        let hint_y = rows.iter().position(|r| r.contains("h/l/j/k move")).expect("hints paint");
        assert!(draft_y < hint_y, "draft sits above the hints");
        assert_eq!(board_cell_at(Rect::new(0, 0, 100, 24), &view, 50, draft_y as u16), None);
    }

}
