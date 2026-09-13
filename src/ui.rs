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

/// Chrome geometry: one focused session fills the 80% main pane (with a
/// one-row tab strip pinned to its top), the sidebar keeps 20%, and two
/// bottom rows hold the session bar plus the status bar. Tiny terminals
/// sacrifice chrome for content.
pub struct ChromeAreas {
    pub main: Rect,
    pub topbar: Rect,
    pub sidebar: Rect,
    pub session_bar: Rect,
    pub status: Rect,
}

pub fn chrome_areas(area: Rect) -> ChromeAreas {
    let (bar_h, status_h) = if area.height >= 3 { (1, 1) } else { (0, 0) };
    let content_h = area.height.saturating_sub(bar_h + status_h);
    let topbar_h = if content_h > 4 { 1 } else { 0 };
    let main_w = area.width * 4 / 5;
    ChromeAreas {
        main: Rect::new(
            area.x,
            area.y + topbar_h,
            main_w,
            content_h.saturating_sub(topbar_h),
        ),
        topbar: Rect::new(area.x, area.y, main_w, topbar_h),
        sidebar: Rect::new(area.x + main_w, area.y, area.width.saturating_sub(main_w), content_h),
        session_bar: Rect::new(area.x, area.y + content_h, area.width, bar_h),
        status: Rect::new(area.x, area.y + content_h + bar_h, area.width, status_h),
    }
}

/// One per-session tab: the agent CLI tab plus the human terminal tab.
#[derive(Clone, Debug, Default)]
pub struct TopTab {
    pub label: String,
    pub active: bool,
}

/// Per-session tab strip: empty when no session is focused.
#[derive(Clone, Debug, Default)]
pub struct TopBar {
    pub tabs: Vec<TopTab>,
}

/// A laid-out top-bar button: area-relative column span.
pub struct TopButton {
    pub index: usize,
    pub start: u16,
    pub end: u16,
}

/// Lay tab buttons left to right with two-space gaps, clipping at the edge.
pub fn layout_topbar(bar: Rect, tabs: &[TopTab]) -> Vec<TopButton> {
    let mut buttons = Vec::new();
    let mut col = bar.x.saturating_add(1);
    let edge = bar.x + bar.width;
    for (index, tab) in tabs.iter().enumerate() {
        let label = format!("[{}]", tab.label);
        let end = col.saturating_add(label.len() as u16);
        if col >= edge || end > edge {
            break;
        }
        buttons.push(TopButton { index, start: col, end });
        col = end.saturating_add(2);
    }
    buttons
}

/// Button index under an area-relative column, if any.
pub fn topbar_at(buttons: &[TopButton], col: u16) -> Option<usize> {
    buttons
        .iter()
        .find(|b| col >= b.start && col < b.end)
        .map(|b| b.index)
}

/// One session-bar entry: 1-based number plus title, with the primary
/// communication group (if any) and its stable palette index.
#[derive(Clone, Debug)]
pub struct SessionTab {
    pub title: String,
    pub live: bool,
    pub focused: bool,
    pub group: Option<String>,
    pub group_color: Option<usize>,
}

/// Group-chip palette: named ANSI colors only (theme roles stay semantic),
/// cycling forever so late groups still get a chip.
pub fn group_palette(index: usize) -> Color {
    const PALETTE: [Color; 8] = [
        Color::Cyan,
        Color::Magenta,
        Color::Blue,
        Color::LightCyan,
        Color::LightMagenta,
        Color::LightGreen,
        Color::White,
        Color::LightBlue,
    ];
    PALETTE[index % PALETTE.len()]
}

/// One rendered chunk of the sessions bar: literal text, its style, and the
/// tab it selects when clicked (`None` for group headers, which never
/// switch sessions).
pub struct BarSegment {
    pub text: String,
    pub style: Style,
    pub index: Option<usize>,
}

/// Build bar segments in manager order so `N` numbering (and `Ctrl-b N`)
/// never shifts: consecutive tabs sharing a group get one colored
/// `group:` header; a group split by outsiders repeats its header rather
/// than reordering anyone.
pub fn session_bar_segments(tabs: &[SessionTab]) -> Vec<BarSegment> {
    let mut segments = Vec::new();
    let mut prev_group: Option<&str> = None;
    for (n, tab) in tabs.iter().enumerate() {
        if let Some(group) = tab.group.as_deref() {
            if prev_group != Some(group) {
                segments.push(BarSegment {
                    text: format!("{group}:"),
                    style: Style::default()
                        .fg(group_palette(tab.group_color.unwrap_or(0)))
                        .add_modifier(Modifier::BOLD),
                    index: None,
                });
            }
        }
        segments.push(BarSegment {
            text: format!("{} {}", n + 1, tab.title),
            style: if tab.focused {
                Style::default().add_modifier(Modifier::BOLD).fg(Color::Yellow)
            } else {
                Style::default()
            },
            index: Some(n),
        });
        prev_group = tab.group.as_deref();
    }
    segments
}

/// A laid-out session button: label plus area-relative column span.
/// Group headers lay out like buttons but carry no index, so clicks on
/// them never switch sessions.
pub struct SessionButton {
    pub index: Option<usize>,
    pub label: String,
    pub start: u16,
    pub end: u16,
}

/// Lay session segments left to right, clipping at the bar edge instead
/// of wrapping. Buttons keep two-space gaps; a group header takes one
/// trailing space so the run reads `group: 1 a 2 b`.
pub fn layout_session_bar(bar: Rect, segments: &[BarSegment]) -> Vec<SessionButton> {
    let mut buttons = Vec::new();
    let mut col = bar.x;
    let edge = bar.x + bar.width;
    for segment in segments {
        let end = col.saturating_add(segment.text.len() as u16);
        if col >= edge || end > edge {
            break;
        }
        let gap = if segment.index.is_none() { 1 } else { 2 };
        buttons.push(SessionButton {
            index: segment.index,
            label: segment.text.clone(),
            start: col,
            end,
        });
        col = end.saturating_add(gap);
    }
    buttons
}

/// Button index under an area-relative column, if any. Group headers are
/// skipped: clicking one selects nothing.
pub fn session_at(buttons: &[SessionButton], col: u16) -> Option<usize> {
    buttons
        .iter()
        .find(|b| col >= b.start && col < b.end)
        .and_then(|b| b.index)
}

/// Sidebar content source: the focused session's detail plus stats,
/// pending hooks, and the permission mode buttons.
#[derive(Clone, Debug)]
pub struct SessionDetail {
    pub name: String,
    pub cli_tool: String,
    pub cwd: String,
    pub state: String,
    pub uptime_secs: u64,
    pub tool_calls: u32,
    pub approvals: u32,
    pub denials: u32,
}

pub struct SidebarInfo {
    pub session: Option<SessionDetail>,
    pub pending: usize,
    /// "off" or "yolo": drives which settings button highlights.
    pub mode: &'static str,
}

/// Compact uptime: 45s, 3m, 2h, 1d 4h.
pub fn format_uptime(secs: u64) -> String {
    const MINUTE: u64 = 60;
    const HOUR: u64 = 60 * MINUTE;
    const DAY: u64 = 24 * HOUR;
    if secs < MINUTE {
        format!("{secs}s")
    } else if secs < HOUR {
        format!("{}m", secs / MINUTE)
    } else if secs < DAY {
        format!("{}h", secs / HOUR)
    } else {
        format!("{}d {}h", secs / DAY, (secs % DAY) / HOUR)
    }
}

/// Fixed sidebar rows: detail block, stats, then the bottom settings row
/// carrying the clickable mode buttons. Hit-testing assumes this layout.
pub const SETTINGS_ROW: u16 = 11;

/// Sidebar lines. Never blank: with no sessions it still guides. The
/// active mode button renders highlighted.
pub fn sidebar_lines(info: &SidebarInfo) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    match &info.session {
        None => {
            lines.push(Line::from("Sessions"));
            lines.push(Line::from("  none yet"));
            lines.push(Line::from("  Ctrl-b c creates one"));
        }
        Some(detail) => {
            lines.push(Line::from("Session"));
            lines.push(Line::from(format!(
                "  {}",
                safe_text::encode_for_display(&detail.name)
            )));
            lines.push(Line::from(format!(
                "  {} {}",
                safe_text::encode_for_display(&detail.cli_tool),
                safe_text::encode_for_display(&detail.state)
            )));
            lines.push(Line::from(format!(
                "  {}",
                safe_text::encode_for_display(&detail.cwd)
            )));
            lines.push(Line::from(""));
            lines.push(Line::from("Stats"));
            lines.push(Line::from(format!(
                "  Uptime {}",
                format_uptime(detail.uptime_secs)
            )));
            lines.push(Line::from(format!("  Tools {}", detail.tool_calls)));
            lines.push(Line::from(format!(
                "  ✓ {} × {}",
                detail.approvals, detail.denials
            )));
            lines.push(Line::from(""));
            lines.push(Line::from("Settings"));
        }
    }
    while lines.len() < SETTINGS_ROW as usize {
        lines.push(Line::from(""));
    }
    let (off_style, yolo_style) = if info.mode == "yolo" {
        (Style::default(), Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED))
    } else {
        (Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED), Style::default())
    };
    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled("[Off]", off_style),
        Span::raw(" "),
        Span::styled("[Yolo]", yolo_style),
    ]));
    lines.push(Line::from(""));
    lines.push(Line::from(format!("Pending: {}", info.pending)));
    lines
}

/// Click areas for the mode buttons, relative to the sidebar rect. Row is
/// fixed by [`SETTINGS_ROW`]; columns match the rendered button spans.
pub struct ModeButtons {
    pub off: Rect,
    pub yolo: Rect,
}

pub fn mode_button_areas(sidebar: Rect) -> ModeButtons {
    let y = sidebar.y + SETTINGS_ROW;
    ModeButtons {
        off: Rect::new(sidebar.x + 2, y, 5, 1),
        yolo: Rect::new(sidebar.x + 8, y, 6, 1),
    }
}

/// Permission mode under a sidebar click, if any.
pub fn mode_at(buttons: &ModeButtons, col: u16, row: u16) -> Option<&'static str> {
    if row != buttons.off.y {
        return None;
    }
    if col >= buttons.off.x && col < buttons.off.x + buttons.off.width {
        Some("off")
    } else if col >= buttons.yolo.x && col < buttons.yolo.x + buttons.yolo.width {
        Some("yolo")
    } else {
        None
    }
}

/// Chrome snapshots: session-bar tabs plus sidebar and status text.
pub struct Chrome {
    pub tabs: Vec<SessionTab>,
    pub topbar: TopBar,
    pub detail: Option<SessionDetail>,
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
    if areas.topbar.height > 0 {
        let buttons = layout_topbar(areas.topbar, &chrome.topbar.tabs);
        let mut spans = Vec::new();
        for button in &buttons {
            let tab = &chrome.topbar.tabs[button.index];
            let style = if tab.active {
                Style::default()
                    .add_modifier(Modifier::BOLD | Modifier::REVERSED)
                    .fg(Color::Yellow)
            } else {
                Style::default()
            };
            if button.start > areas.topbar.x + 1 {
                spans.push(Span::raw("  "));
            }
            spans.push(Span::styled(format!("[{}]", tab.label), style));
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), areas.topbar);
    }
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
            session: chrome.detail.clone(),
            pending: chrome.pending,
            mode: chrome.mode,
        };
        let side = Paragraph::new(Text::from(sidebar_lines(&info))).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(theme::style(theme::Role::BorderUnfocused))
                .title(" status "),
        );
        frame.render_widget(side, areas.sidebar);
    }
    if areas.session_bar.height > 0 {
        let segments = session_bar_segments(&chrome.tabs);
        let buttons = layout_session_bar(areas.session_bar, &segments);
        let mut spans = Vec::new();
        // Gaps come from laid-out positions (headers take one space,
        // buttons two), so clicks always land on what they see.
        let mut col = areas.session_bar.x;
        for (button, segment) in buttons.iter().zip(segments.iter()) {
            for _ in col..button.start {
                spans.push(Span::raw(" "));
            }
            spans.push(Span::styled(button.label.clone(), segment.style));
            col = button.end;
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
        assert_eq!(c.topbar, Rect::new(0, 0, 64, 1));
        assert_eq!(c.main, Rect::new(0, 1, 64, 21));
        assert_eq!(c.sidebar, Rect::new(64, 0, 16, 22));
        assert_eq!(c.session_bar, Rect::new(0, 22, 80, 1));
        assert_eq!(c.status, Rect::new(0, 23, 80, 1));
        let wide = chrome_areas(Rect::new(0, 0, 120, 40));
        assert_eq!(wide.topbar, Rect::new(0, 0, 96, 1));
        assert_eq!(wide.main, Rect::new(0, 1, 96, 37));
        assert_eq!(wide.sidebar, Rect::new(96, 0, 24, 38));
        // Tiny terminals keep content over chrome.
        let tiny = chrome_areas(Rect::new(0, 0, 80, 2));
        assert_eq!(tiny.main.height, 2);
        assert_eq!(tiny.status.height, 0);
    }

    fn tab(title: &str, focused: bool) -> SessionTab {
        SessionTab {
            title: title.to_string(),
            live: true,
            focused,
            group: None,
            group_color: None,
        }
    }

    fn grouped(title: &str, focused: bool, group: &str, color: usize) -> SessionTab {
        SessionTab {
            title: title.to_string(),
            live: true,
            focused,
            group: Some(group.to_string()),
            group_color: Some(color),
        }
    }

    #[test]
    fn session_bar_buttons_number_left_to_right() {
        let bar = Rect::new(0, 22, 80, 1);
        let segments = session_bar_segments(&[tab("shell-1", true), tab("shell-2", false)]);
        let buttons = layout_session_bar(bar, &segments);
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
        let narrow = layout_session_bar(
            Rect::new(0, 0, 10, 1),
            &session_bar_segments(&[tab("shell-1", true), tab("shell-2", false)]),
        );
        assert_eq!(narrow.len(), 1);
    }

    #[test]
    fn session_bar_groups_share_one_colored_header() {
        let tabs = vec![
            grouped("a1", true, "codex-proj", 0),
            grouped("a2", false, "codex-proj", 0),
            tab("solo", false),
            grouped("b1", false, "other", 1),
        ];
        let segments = session_bar_segments(&tabs);
        let texts: Vec<&str> = segments.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(
            texts,
            vec!["codex-proj:", "1 a1", "2 a2", "3 solo", "other:", "4 b1"],
            "one header per run, numbering never shifts"
        );
        // Headers are colored with the group palette, never clickable.
        assert_eq!(segments[0].style.fg, Some(group_palette(0)));
        assert_eq!(segments[0].index, None);
        assert_eq!(segments[4].style.fg, Some(group_palette(1)));
        assert_ne!(group_palette(0), group_palette(1));
        // Focused session keeps the focus style.
        assert_eq!(segments[1].style.fg, Some(Color::Yellow));
        let bar = Rect::new(0, 22, 80, 1);
        let buttons = layout_session_bar(bar, &segments);
        assert_eq!(buttons.len(), segments.len());
        // One trailing space after a header: `codex-proj: 1 a1`.
        assert_eq!((buttons[0].start, buttons[0].end), (0, 11));
        assert_eq!((buttons[1].start, buttons[1].end), (12, 16));
        // Clicking the header selects nothing; clicking a member selects it.
        assert_eq!(session_at(&buttons, buttons[0].start), None);
        assert_eq!(session_at(&buttons, buttons[1].start), Some(0));
        assert_eq!(session_at(&buttons, buttons[5].start), Some(3));
        // Palette cycles instead of running out.
        assert_eq!(group_palette(8), group_palette(0));
    }

    fn text_of(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn has(lines: &[Line], needle: &str) -> bool {
        lines.iter().any(|l| text_of(l).contains(needle))
    }

    #[test]
    fn sidebar_shows_focused_session_detail_and_mode() {
        let info = SidebarInfo {
            session: Some(SessionDetail {
                name: "shell-1".to_string(),
                cli_tool: "shell".to_string(),
                cwd: "/tmp/proj".to_string(),
                state: "running · Thinking".to_string(),
                uptime_secs: 65,
                tool_calls: 4,
                approvals: 3,
                denials: 1,
            }),
            pending: 3,
            mode: "off",
        };
        let lines = sidebar_lines(&info);
        assert!(has(&lines, "shell-1"), "name: {lines:?}");
        assert!(has(&lines, "shell"), "tool: {lines:?}");
        assert!(has(&lines, "/tmp/proj"), "cwd: {lines:?}");
        assert!(has(&lines, "running"), "state: {lines:?}");
        assert!(has(&lines, "1m"), "uptime: {lines:?}");
        assert!(has(&lines, "4"), "calls: {lines:?}");
        assert!(has(&lines, "Pending: 3"), "pending: {lines:?}");
        assert!(has(&lines, "[Off]"), "off highlighted: {lines:?}");
        assert!(has(&lines, "Yolo"), "yolo offered: {lines:?}");
        assert!(!has(&lines, "safe-only"), "no third mode: {lines:?}");
        // Yolo highlights instead when active.
        let yolo = SidebarInfo { session: info.session.clone(), pending: 0, mode: "yolo" };
        let yolo_lines = sidebar_lines(&yolo);
        assert!(has(&yolo_lines, "[Yolo]"), "yolo highlighted: {yolo_lines:?}");
        // Never blank: empty state still guides.
        let empty = sidebar_lines(&SidebarInfo { session: None, pending: 0, mode: "off" });
        assert!(has(&empty, "Ctrl-b c"), "guides: {empty:?}");
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
            tabs: vec![tab("sh", true)],
            topbar: TopBar {
                tabs: vec![
                    TopTab {
                        label: "Shell".to_string(),
                        active: true,
                    },
                    TopTab {
                        label: "Terminal".to_string(),
                        active: false,
                    },
                ],
            },
            detail: None,
            pending: 0,
            mode: "off",
            status: "status".to_string(),
        }
    }

    #[test]
    fn topbar_layout_clips_and_hit_tests() {
        let bar = Rect::new(0, 0, 20, 1);
        let tabs = vec![
            TopTab { label: "Codex".to_string(), active: true },
            TopTab { label: "Terminal".to_string(), active: false },
        ];
        let buttons = layout_topbar(bar, &tabs);
        assert_eq!(buttons.len(), 2);
        // "[Codex]" spans 1..8, gap, "[Terminal]" spans 10..20.
        assert_eq!(topbar_at(&buttons, 1), Some(0));
        assert_eq!(topbar_at(&buttons, 7), Some(0));
        assert_eq!(topbar_at(&buttons, 8), None, "gap is dead");
        assert_eq!(topbar_at(&buttons, 10), Some(1));
        assert_eq!(topbar_at(&buttons, 0), None, "margin is dead");
        // Narrow bar clips the second tab instead of wrapping.
        let narrow = layout_topbar(Rect::new(0, 0, 12, 1), &tabs);
        assert_eq!(narrow.len(), 1);
    }

    #[test]
    fn render_shows_titles_bodies_and_status() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut c = chrome();
        c.tabs = vec![tab("agent-1", true)];
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
        terminal.backend_mut().assert_cursor_position(Position::new(6, 4));
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
        let cell = &buf.content[2 * w + 1];
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
