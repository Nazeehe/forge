//! Testable view layer: grid geometry plus rendering from plain view models.
//!
//! Content generation stays separate from frame rendering: [`render_grid`]
//! takes snapshots, so the whole grid is assertable through a test backend
//! without a terminal.

pub mod board;
pub mod dialogs;
pub mod help;
pub mod layout;
pub mod session_bar;
pub mod sidebar;
pub mod tetris;
pub mod text;
pub mod theme;
pub mod topbar;
pub mod visual;
pub mod whichkey;
pub mod writer;

#[cfg(test)]
mod test_support;

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::{
    infra::safe_text,
    session::pty::{CellColor, CellFormat, FormattedCell},
};

use self::board::BoardView;
use self::layout::{chrome_areas, cursor_screen_pos, grid_area, grid_cells, pane_grid_area, ChromeAreas};
use self::session_bar::SessionTab;
use self::sidebar::{FleetRow, SessionDetail};
use self::topbar::TopBar;

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
    // Faint ghost/prediction text (SGR 2): real terminals render this dim
    // gray instead of full-bright.
    if format.dim {
        style = style.add_modifier(Modifier::DIM);
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

/// Chrome snapshots: session-bar tabs plus sidebar state.
pub struct Chrome {
    pub tabs: Vec<SessionTab>,
    pub topbar: TopBar,
    pub detail: Option<SessionDetail>,
    /// Fleet router rows plus cursor/scroll/active for the sidebar slot.
    pub sessions: Vec<FleetRow>,
    pub active: Option<crate::session::SessionId>,
    pub fleet_cursor: Option<crate::session::SessionId>,
    pub fleet_scroll: usize,
    pub other_timers: usize,
    pub pending: usize,
    pub mode: &'static str,
    /// Board view open: highlights the sidebar Kanban button.
    pub board_open: bool,
    /// Tetris view open: highlights the sidebar Tetris button.
    pub tetris_open: bool,
    /// Live game snapshot; painted in the sidebar list region when open.
    pub tetris: Option<crate::tetris::TetrisGame>,
    /// Global kanban content; painted in the main area when open.
    pub board: Option<BoardView>,
    /// "on" or "off": Telegram mobile transport state for the sidebar row.
    pub telegram: &'static str,
    /// Latest `message_user` badge as `(session, text)`, if any.
    pub telegram_badge: Option<(String, String)>,
    /// Grid mode: the main area shows every session in framed cells and
    /// the sidebar hides for full-width tiles.
    pub grid: bool,
    /// Pill session tabs: rounded Nerd Font ends around each tab button.
    pub pills: bool,
}

/// Clip one styled row to a cell width, preserving per-span styles.
fn clip_spans(row: &[SpanView], width: usize) -> Vec<Span<'static>> {
    let mut out = Vec::new();
    let mut room = width;
    for span in row {
        if room == 0 {
            break;
        }
        let take: String = span.text.chars().take(room).collect();
        room -= take.chars().count();
        if !take.is_empty() {
            out.push(Span::styled(take, span.style));
        }
    }
    out
}

/// Grid mode: every session in its own framed cell across the full
/// width, name in the frame, the focused frame highlighted. Cells show
/// the tail of each pane (latest output first); panes are never resized,
/// so entering grid never reflows an agent's terminal.
fn render_grid(frame: &mut Frame, area: Rect, panes: &[PaneView], _chrome: &Chrome) {
    let grid = grid_area(area);
    let cells = grid_cells(grid, panes.len());
    if cells.is_empty() {
        let hint =
            Paragraph::new("No sessions yet — press Ctrl-b c to create one.\nCtrl-b q quits.")
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                            .border_type(crate::ui::theme::border_type())
                        .border_style(theme::style(theme::Role::BorderUnfocused))
                        .title(" forge "),
                );
        frame.render_widget(hint, grid);
        return;
    }
    for (view, cell) in panes.iter().zip(cells.iter()) {
        let (glyph, _role) = if view.live {
            theme::status_glyph_running()
        } else {
            theme::status_glyph_exited()
        };
        let title = format!("{} {} ", glyph, safe_text::encode_for_display(&view.title));
        let (border, name_style) = if view.focused {
            (
                theme::style(theme::Role::BorderFocused),
                theme::style(theme::Role::Focus).add_modifier(Modifier::REVERSED),
            )
        } else {
            (theme::style(theme::Role::BorderUnfocused), theme::style(theme::Role::Text))
        };
        let block = Block::default()
            .borders(Borders::ALL)
                .border_type(crate::ui::theme::border_type())
            .border_style(border)
            .title(Span::styled(title, name_style));
        let inner = block.inner(*cell);
        frame.render_widget(block, *cell);
        if inner.width < 1 || inner.height < 1 {
            continue;
        }
        let h = inner.height as usize;
        let rows: Vec<Line> = view
            .lines
            .iter()
            .rev()
            .take(h)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .map(|row| Line::from(clip_spans(row, inner.width as usize)))
            .collect();
        frame.render_widget(Paragraph::new(Text::from(rows)), inner);
        if view.focused {
            if let Some((row, col)) = view.cursor {
                // Rows are bottom-anchored: shift pane coordinates down
                // by the skipped head rows before placing the cursor.
                let skipped = view.lines.len().saturating_sub(h) as u16;
                if let Some(pos) =
                    cursor_screen_pos(*cell, Some((row.saturating_sub(skipped), col)))
                {
                    frame.set_cursor_position(pos);
                }
            }
        }
    }
}

pub fn render(frame: &mut Frame, area: Rect, panes: &[PaneView], chrome: &Chrome) {
    let areas = chrome_areas(area);
    if chrome.grid {
        render_grid(frame, area, panes, chrome);
    } else if let Some(board) = chrome.board.as_ref() {
        // Global kanban view: the board owns the main area while the
        // topbar, sidebar (with its Kanban button), and session bar
        // stay live around it.
        board::render_board(frame, areas.main, board);
        if areas.topbar.height > 0 {
            topbar::render_topbar(frame, areas.topbar, &chrome.topbar.tabs, chrome.pills);
        }
        sidebar::render_sidebar(frame, &areas, chrome);
    } else {
        render_focused(frame, &areas, panes, chrome.detail.as_ref());
        if areas.topbar.height > 0 {
            topbar::render_topbar(frame, areas.topbar, &chrome.topbar.tabs, chrome.pills);
        }
        sidebar::render_sidebar(frame, &areas, chrome);
    }
    session_bar::render_session_bar(frame, &areas, chrome);
}

/// Focused-session view: one framed pane plus sidebar. The border
/// title carries the context line — name, live state, sticky status —
/// mirroring the fleet block's reason without spending a pane row.
fn render_focused(
    frame: &mut Frame,
    areas: &ChromeAreas,
    panes: &[PaneView],
    detail: Option<&SessionDetail>,
) {
    let focused = panes.iter().find(|p| p.focused).or(panes.first());
    match focused {
        Some(view) => {
            let (glyph, _role) = if view.live {
                theme::status_glyph_running()
            } else {
                theme::status_glyph_exited()
            };
            let title = match detail {
                Some(d) => {
                    let mut t = format!(
                        "{} {} · {}",
                        glyph,
                        safe_text::encode_for_display(&d.name),
                        safe_text::encode_for_display(&d.state)
                    );
                    if let Some(status) = d.status.as_deref() {
                        t.push_str(&format!(
                            " · {}",
                            safe_text::encode_for_display(status)
                        ));
                    }
                    t.push(' ');
                    t
                }
                None => format!("{} {} ", glyph, safe_text::encode_for_display(&view.title)),
            };
            let block = Block::default()
                .borders(Borders::ALL)
                    .border_type(crate::ui::theme::border_type())
                .border_style(theme::style(theme::Role::BorderFocused))
                .title(title);
            let mut text = pane_text(view);
            if areas.topbar.y > areas.main.y {
                text.lines.insert(0, Line::from(""));
            }
            frame.render_widget(Paragraph::new(text).block(block), areas.main);
            if let Some(pos) = cursor_screen_pos(pane_grid_area(&areas), view.cursor) {
                frame.set_cursor_position(pos);
            }
        }
        None => {
            let hint =
                Paragraph::new("No sessions yet — press Ctrl-b c to create one.\nCtrl-b q quits.")
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                                .border_type(crate::ui::theme::border_type())
                            .border_style(theme::style(theme::Role::BorderUnfocused))
                            .title(" forge "),
                    );
            frame.render_widget(hint, areas.main);
        }
    }
}

fn pane_text(view: &PaneView) -> Text<'static> {
    Text::from(
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
    )
}

#[cfg(test)]
mod tests {
    use super::layout::{chrome_areas, pane_grid_area, translate_mouse};
    use super::test_support::{area, buffer_rows, buffer_text, chrome, pane, sidebar_chrome, tab};
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Position;
    use ratatui::Terminal;





































    #[test]
    fn wide_agent_tabs_sit_inside_pane_border_above_pty_content() {
        let bounds = Rect::new(0, 0, 180, 40);
        let areas = chrome_areas(bounds);
        assert_eq!(areas.main.y, 0);
        assert_eq!(areas.topbar.y, 1);
        let mut c = chrome();
        c.topbar.tabs[0].label = "Codex".into();
        let mut terminal = Terminal::new(TestBackend::new(180, 40)).unwrap();
        terminal.draw(|f| render(f, bounds, &[pane("agent", "PTY first line", true)], &c)).unwrap();
        let rows = buffer_rows(&terminal);
        assert!(rows[0].contains("agent"));
        assert!(rows[1].contains("[Codex]"));
        assert!(rows[2].contains("PTY first line"));
        assert_eq!(translate_mouse(pane_grid_area(&areas), 1, 2), Some((1, 1)));
    }
























    /// Six fleet rows with staggered reasons for overflow pressure.















    #[test]
    fn main_pane_title_carries_session_context() {
        use ratatui::{backend::TestBackend, Terminal};
        // The focused PTY border names the session plus its live state
        // and sticky status; without detail it stays the legacy title.
        let title_of = |chrome: &Chrome| -> String {
            let view = PaneView {
                title: "shell-1".to_string(),
                lines: Vec::new(),
                live: true,
                focused: true,
                cursor: None,
            };
            let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
            terminal
                .draw(|f| render(f, f.area(), &[view], chrome))
                .unwrap();
            let buf = terminal.backend().buffer();
            let areas = chrome_areas(Rect::new(0, 0, 80, 24));
            (areas.main.x..areas.main.right())
                .map(|x| buf[(x, areas.main.y)].symbol().to_string())
                .collect()
        };
        let with_detail = title_of(&sidebar_chrome());
        assert!(with_detail.contains("shell-1"), "name: {with_detail:?}");
        assert!(with_detail.contains("running"), "state: {with_detail:?}");
        assert!(
            with_detail.contains("waiting on review"),
            "status: {with_detail:?}"
        );
        let mut bare = sidebar_chrome();
        bare.detail = None;
        let legacy = title_of(&bare);
        assert!(legacy.contains("shell-1"), "legacy name: {legacy:?}");
        assert!(
            !legacy.contains("waiting on review"),
            "no status without detail: {legacy:?}"
        );
    }

    #[test]
    fn render_shows_titles_bodies_and_session_bar() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut c = chrome();
        c.tabs = vec![tab("agent-1", true)];
        terminal
            .draw(|f| render(f, area(), &[pane("agent-1", "hello out", true)], &c))
            .unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("agent-1"), "title visible");
        assert!(text.contains("hello out"), "body visible");
        assert!(text.contains("1 agent-1"), "session button visible");
        // No status bar: the last row is the session bar, and no help
        // line is rendered anywhere.
        let rows = buffer_rows(&terminal);
        assert!(rows[23].contains("1 agent-1"), "bar owns last row: {:?}", rows[23]);
        assert!(!text.contains("prefix Ctrl-b"), "help line is gone");
    }




    #[test]
    fn render_grid_frames_every_session_without_sidebar() {
        use ratatui::style::Modifier;
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let mut c = chrome();
        c.grid = true;
        let mut b = pane("b", "bravo output", true);
        b.focused = false;
        terminal
            .draw(|f| render(f, area(), &[pane("a", "alpha output", true), b], &c))
            .unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("alpha output"), "first cell body");
        assert!(text.contains("bravo output"), "second cell body");
        assert!(!text.contains("status"), "sidebar hidden in grid");
        assert!(!text.contains("Terminal"), "tab strip hidden in grid");
        let buf = terminal.backend().buffer();
        // Titles sit inside the top borders on row 0: `● a`, `● b`.
        assert_eq!(buf[(3, 0)].symbol(), "a");
        assert_eq!(buf[(43, 0)].symbol(), "b");
        // The focused name reverses; the other frame stays plain.
        assert!(buf[(1, 0)].modifier.contains(Modifier::REVERSED), "focused name");
        assert!(!buf[(41, 0)].modifier.contains(Modifier::REVERSED), "plain name");
        // Full-width tiles: the second frame opens mid-screen.
        assert_eq!(buf[(40, 0)].symbol(), "┌");
    }






    #[test]
    fn render_grid_empty_shows_hint() {
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let mut c = chrome();
        c.grid = true;
        terminal.draw(|f| render(f, area(), &[], &c)).unwrap();
        assert!(buffer_text(&terminal).contains("No sessions yet"));
    }


    #[test]
    fn multiline_body_renders_across_rows() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| render(f, area(), &[pane("sh", "line1\nline2", true)], &chrome()))
            .unwrap();
        let rows = buffer_rows(&terminal);
        let r1 = rows.iter().position(|r| r.contains("line1")).expect("line1 visible");
        let r2 = rows.iter().position(|r| r.contains("line2")).expect("line2 visible");
        assert_eq!(r2, r1 + 1, "row break preserved: {rows:?}");
        assert!(rows.iter().all(|r| !r.contains('⏎')), "no flattened newlines");
    }

    #[test]
    fn dim_spans_render_faint_in_the_grid() {
        // End of the ghost-text chain: parser marks dim (pty test),
        // style_for maps it (sgr test), and the grid must keep the
        // modifier so the terminal faints it instead of full-bright.
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut p = pane("sh", "ghost", true);
        p.lines = vec![vec![SpanView {
            text: "ghost".to_string(),
            style: Style::default().add_modifier(Modifier::DIM),
        }]];
        terminal
            .draw(|f| render(f, area(), &[p], &chrome()))
            .unwrap();
        let cell = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .find(|c| c.symbol() == "g")
            .expect("ghost visible");
        assert!(
            cell.modifier.contains(Modifier::DIM),
            "faint reaches the grid"
        );
    }

    #[test]
    fn focused_pane_cursor_is_placed_inside_borders() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut p = pane("sh", "hi", true);
        p.cursor = Some((2, 5));
        terminal
            .draw(|f| render(f, area(), &[p], &chrome()))
            .unwrap();
        terminal.backend_mut().assert_cursor_position(Position::new(6, 4));
    }

    #[test]
    fn style_for_maps_sgr_attributes() {
        use crate::session::pty::{CellColor, CellFormat};
        let f = CellFormat {
            fg: CellColor::Indexed(1),
            bg: CellColor::Rgb(10, 20, 30),
            bold: true,
            italic: false,
            underline: true,
            inverse: true,
            dim: true,
        };
        let s = style_for(&f);
        assert_eq!(s.fg, Some(Color::Indexed(1)));
        assert_eq!(s.bg, Some(Color::Rgb(10, 20, 30)));
        assert!(s.add_modifier.contains(Modifier::BOLD));
        assert!(s.add_modifier.contains(Modifier::UNDERLINED));
        assert!(s.add_modifier.contains(Modifier::REVERSED));
        assert!(s.add_modifier.contains(Modifier::DIM));
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
            .draw(|f| render(f, area(), &[view], &chrome()))
            .unwrap();
        let buf = terminal.backend().buffer();
        let w = buf.area.width as usize;
        let cell = &buf.content[2 * w + 1];
        assert_eq!(cell.symbol(), "R");
        assert_eq!(cell.fg, Color::Indexed(1));
    }



    #[test]
    fn empty_grid_explains_itself() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| render(f, area(), &[], &chrome()))
            .unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("Ctrl-b c"), "empty state guides: {text:?}");
    }

    #[test]
    fn render_shows_only_the_focused_session() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut old = pane("old", "bye", false);
        old.focused = false;
        let new = pane("new", "hi", true);
        let mut c = chrome();
        c.tabs = vec![
            SessionTab { title: "old".to_string(), live: false, focused: false, group: None, group_color: None },
            SessionTab { title: "new".to_string(), live: true, focused: true, group: None, group_color: None },
        ];
        terminal
            .draw(|f| render(f, area(), &[old, new], &c))
            .unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("hi"), "focused body visible");
        assert!(!text.contains("bye"), "background body hidden");
        assert!(text.contains("1 old") && text.contains("2 new"), "both buttons");
        let _ = theme::style(theme::Role::Text);
    }
}
