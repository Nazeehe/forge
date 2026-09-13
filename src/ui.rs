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

/// Chrome geometry: one focused session fills the 80% main pane, the
/// sidebar keeps 20%, and two bottom rows hold the session bar plus the
/// status bar. Tiny terminals sacrifice chrome for content.
pub struct ChromeAreas {
    pub main: Rect,
    pub sidebar: Rect,
    pub session_bar: Rect,
    pub status: Rect,
}

pub fn chrome_areas(area: Rect) -> ChromeAreas {
    let (bar_h, status_h) = if area.height >= 3 { (1, 1) } else { (0, 0) };
    let content_h = area.height.saturating_sub(bar_h + status_h);
    let main_w = area.width * 4 / 5;
    ChromeAreas {
        main: Rect::new(area.x, area.y, main_w, content_h),
        sidebar: Rect::new(area.x + main_w, area.y, area.width.saturating_sub(main_w), content_h),
        session_bar: Rect::new(area.x, area.y + content_h, area.width, bar_h),
        status: Rect::new(area.x, area.y + content_h + bar_h, area.width, status_h),
    }
}

/// One session-bar entry: 1-based number plus title.
#[derive(Clone, Debug)]
pub struct SessionTab {
    pub title: String,
    pub live: bool,
    pub focused: bool,
}

/// A laid-out session button: label plus area-relative column span.
pub struct SessionButton {
    pub index: usize,
    pub label: String,
    pub start: u16,
    pub end: u16,
}

/// Lay session buttons left to right with two-space gaps, clipping at the
/// bar edge instead of wrapping.
pub fn layout_session_bar(bar: Rect, titles: &[String]) -> Vec<SessionButton> {
    let mut buttons = Vec::new();
    let mut col = bar.x;
    let edge = bar.x + bar.width;
    for (index, title) in titles.iter().enumerate() {
        let label = format!("{} {title}", index + 1);
        let end = col.saturating_add(label.len() as u16);
        if col >= edge || end > edge {
            break;
        }
        buttons.push(SessionButton { index, label, start: col, end });
        col = end.saturating_add(2);
    }
    buttons
}

/// Button index under an area-relative column, if any.
pub fn session_at(buttons: &[SessionButton], col: u16) -> Option<usize> {
    buttons
        .iter()
        .find(|b| col >= b.start && col < b.end)
        .map(|b| b.index)
}

/// Sidebar content source: session list plus pending approvals and mode.
pub struct SidebarInfo {
    pub sessions: Vec<SessionTab>,
    pub pending: usize,
    pub mode: &'static str,
}

/// Sidebar lines. Never blank: with no sessions it still guides.
pub fn sidebar_lines(info: &SidebarInfo) -> Vec<String> {
    let mut lines = vec!["Sessions".to_string()];
    if info.sessions.is_empty() {
        lines.push("  none yet".to_string());
        lines.push("  Ctrl-b c creates one".to_string());
    }
    for (i, tab) in info.sessions.iter().enumerate() {
        let (glyph, _) = if tab.live {
            theme::status_glyph_running()
        } else {
            theme::status_glyph_exited()
        };
        let mark = if tab.focused { "▸" } else { " " };
        lines.push(format!(
            "{mark} {glyph} {} {}",
            i + 1,
            safe_text::encode_for_display(&tab.title)
        ));
    }
    lines.push(String::new());
    lines.push(format!("Pending: {}", info.pending));
    lines.push(format!("Mode: {}", info.mode));
    lines
}

/// Chrome snapshots: session-bar tabs plus sidebar and status text.
pub struct Chrome {
    pub tabs: Vec<SessionTab>,
    pub pending: usize,
    pub mode: &'static str,
    pub status: String,
}

/// Render one focused session in the main pane with sidebar, session bar,
/// and status bar. Titles and bodies are untrusted PTY output, so both pass
/// through display encoding: raw escape sequences must never reach the
/// outer terminal.
pub fn render(frame: &mut Frame, area: Rect, panes: &[PaneView], chrome: &Chrome) {
    let areas = chrome_areas(area);
    let focused = panes.iter().find(|p| p.focused).or(panes.first());
    match focused {
        Some(view) => {
            let (glyph, _role) = if view.live {
                theme::status_glyph_running()
            } else {
                theme::status_glyph_exited()
            };
            let title = format!("{} {} ", glyph, safe_text::encode_for_display(&view.title));
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(theme::style(theme::Role::BorderFocused))
                .title(title);
            frame.render_widget(Paragraph::new(pane_text(view)).block(block), areas.main);
            if let Some(pos) = cursor_screen_pos(areas.main, view.cursor) {
                frame.set_cursor_position(pos);
            }
        }
        None => {
            let hint =
                Paragraph::new("No sessions yet — press Ctrl-b c to create one.\nCtrl-b q quits.")
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .border_style(theme::style(theme::Role::BorderUnfocused))
                            .title(" forge "),
                    );
            frame.render_widget(hint, areas.main);
        }
    }
    if areas.sidebar.width > 0 && areas.sidebar.height > 0 {
        let info = SidebarInfo {
            sessions: chrome.tabs.clone(),
            pending: chrome.pending,
            mode: chrome.mode,
        };
        let side = Paragraph::new(sidebar_lines(&info).join("\n")).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(theme::style(theme::Role::BorderUnfocused))
                .title(" status "),
        );
        frame.render_widget(side, areas.sidebar);
    }
    if areas.session_bar.height > 0 {
        let titles: Vec<String> = chrome.tabs.iter().map(|t| t.title.clone()).collect();
        let buttons = layout_session_bar(areas.session_bar, &titles);
        let mut spans = Vec::new();
        for button in &buttons {
            let style = if chrome.tabs.get(button.index).is_some_and(|t| t.focused) {
                Style::default().add_modifier(Modifier::BOLD).fg(Color::Yellow)
            } else {
                Style::default()
            };
            if button.start > areas.session_bar.x {
                spans.push(Span::raw("  "));
            }
            spans.push(Span::styled(button.label.clone(), style));
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), areas.session_bar);
    }
    if areas.status.height > 0 {
        frame.render_widget(Paragraph::new(chrome.status.clone()), areas.status);
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
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn area() -> Rect {
        Rect::new(0, 0, 80, 24)
    }

    #[test]
    fn chrome_splits_main_sidebar_and_bars() {
        let c = chrome_areas(Rect::new(0, 0, 80, 24));
        assert_eq!(c.main, Rect::new(0, 0, 64, 22));
        assert_eq!(c.sidebar, Rect::new(64, 0, 16, 22));
        assert_eq!(c.session_bar, Rect::new(0, 22, 80, 1));
        assert_eq!(c.status, Rect::new(0, 23, 80, 1));
        let wide = chrome_areas(Rect::new(0, 0, 120, 40));
        assert_eq!(wide.main, Rect::new(0, 0, 96, 38));
        assert_eq!(wide.sidebar, Rect::new(96, 0, 24, 38));
        // Tiny terminals keep content over chrome.
        let tiny = chrome_areas(Rect::new(0, 0, 80, 2));
        assert_eq!(tiny.main.height, 2);
        assert_eq!(tiny.status.height, 0);
    }

    #[test]
    fn session_bar_buttons_number_left_to_right() {
        let bar = Rect::new(0, 22, 80, 1);
        let buttons = layout_session_bar(bar, &["shell-1".to_string(), "shell-2".to_string()]);
        assert_eq!(buttons.len(), 2);
        assert_eq!(buttons[0].label, "1 shell-1");
        assert_eq!((buttons[0].start, buttons[0].end), (0, 9));
        assert_eq!(buttons[1].label, "2 shell-2");
        assert_eq!((buttons[1].start, buttons[1].end), (11, 20));
        // Hit-test lands on labels, not gaps or borders.
        assert_eq!(session_at(&buttons, 0), Some(0));
        assert_eq!(session_at(&buttons, 8), Some(0));
        assert_eq!(session_at(&buttons, 9), None);
        assert_eq!(session_at(&buttons, 11), Some(1));
        assert_eq!(session_at(&buttons, 79), None);
        // Overflow clips instead of wrapping.
        let narrow = layout_session_bar(Rect::new(0, 0, 10, 1), &["shell-1".to_string(), "shell-2".to_string()]);
        assert_eq!(narrow.len(), 1);
    }

    #[test]
    fn sidebar_lists_sessions_pending_and_mode() {
        let info = SidebarInfo {
            sessions: vec![
                SessionTab { title: "shell-1".to_string(), live: true, focused: true },
                SessionTab { title: "old".to_string(), live: false, focused: false },
            ],
            pending: 3,
            mode: "safe-only",
        };
        let lines = sidebar_lines(&info);
        assert!(lines.iter().any(|l| l.contains("Sessions")), "header: {lines:?}");
        assert!(lines.iter().any(|l| l.contains('▸') && l.contains("shell-1")), "focus: {lines:?}");
        assert!(lines.iter().any(|l| l.contains('×') && l.contains("old")), "exited: {lines:?}");
        assert!(lines.iter().any(|l| l.contains("Pending: 3")), "pending: {lines:?}");
        assert!(lines.iter().any(|l| l.contains("safe-only")), "mode: {lines:?}");
        // Never blank: empty state still guides.
        let empty = sidebar_lines(&SidebarInfo { sessions: vec![], pending: 0, mode: "off" });
        assert!(empty.iter().any(|l| l.contains("Ctrl-b c")), "guides: {empty:?}");
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

    fn chrome() -> Chrome {
        Chrome {
            tabs: vec![SessionTab {
                title: "sh".to_string(),
                live: true,
                focused: true,
            }],
            pending: 0,
            mode: "off",
            status: "status".to_string(),
        }
    }

    #[test]
    fn render_shows_titles_bodies_and_status() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut c = chrome();
        c.tabs = vec![SessionTab {
            title: "agent-1".to_string(),
            live: true,
            focused: true,
        }];
        c.status = "2 sessions | prefix Ctrl-b".to_string();
        terminal
            .draw(|f| render(f, area(), &[pane("agent-1", "hello out", true)], &c))
            .unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("agent-1"), "title visible");
        assert!(text.contains("hello out"), "body visible");
        assert!(text.contains("prefix Ctrl-b"), "status visible");
        assert!(text.contains("1 agent-1"), "session button visible");
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
            .draw(|f| render(f, area(), &[pane("sh", "line1\nline2", true)], &chrome()))
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
            .draw(|f| render(f, area(), &[p], &chrome()))
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
            .draw(|f| render(f, area(), &[view], &chrome()))
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
            SessionTab { title: "old".to_string(), live: false, focused: false },
            SessionTab { title: "new".to_string(), live: true, focused: true },
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
