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
use tui_realm_stdlib::components::Label;
use tuirealm::command::{Cmd, CmdResult};
use tuirealm::component::Component;
use tuirealm::props::{AttrValue, Attribute, QueryResult};
use tuirealm::state::State;

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

/// Translate outer 0-based mouse coordinates into 1-based pane-grid cells.
/// `None` when the event lands on borders, the session bar, or outside
/// the pane: chrome keeps those events.
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
/// one-row tab strip pinned to its top), the sidebar keeps 20%, and one
/// bottom row holds the session bar. There is no status bar: dialogs
/// carry their own key hints. Tiny terminals sacrifice chrome for
/// content.
pub struct ChromeAreas {
    pub main: Rect,
    pub topbar: Rect,
    pub sidebar: Rect,
    pub session_bar: Rect,
}

pub fn chrome_areas(area: Rect) -> ChromeAreas {
    let bar_h = if area.height >= 3 { 1 } else { 0 };
    let content_h = area.height.saturating_sub(bar_h);
    let topbar_h = if content_h > 4 { 1 } else { 0 };
    let main_w = if area.width >= 160 { area.width * 3 / 4 } else { area.width * 4 / 5 };
    let inset_tabs = area.width >= 160 && topbar_h > 0;
    ChromeAreas {
        main: if inset_tabs {
            Rect::new(area.x, area.y, main_w, content_h)
        } else {
            Rect::new(area.x, area.y + topbar_h, main_w, content_h.saturating_sub(topbar_h))
        },
        topbar: Rect::new(area.x, area.y + inset_tabs as u16, main_w, topbar_h),
        sidebar: Rect::new(area.x + main_w, area.y, area.width.saturating_sub(main_w), content_h),
        session_bar: Rect::new(area.x, area.y + content_h, area.width, bar_h),
    }
}

/// For wide layouts the tab strip occupies the first inner pane row;
/// mouse/cursor/PTY sizing begin one row below it.
pub fn pane_grid_area(areas: &ChromeAreas) -> Rect {
    if areas.topbar.height > 0 && areas.topbar.y > areas.main.y {
        Rect::new(areas.main.x, areas.main.y + 1, areas.main.width,
            areas.main.height.saturating_sub(1))
    } else {
        areas.main
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

/// A small chrome button backed by a tui-realm Label. The standard
/// library has labels and selectors but no button component, so this
/// wrapper gives the label a Submit command for mouse activation.
pub struct ChromeButton {
    label: Label,
}

impl ChromeButton {
    pub fn new(text: &str, style: Style) -> Self {
        Self {
            label: Label::default()
                .text(safe_text::encode_for_display(text))
                .style(style),
        }
    }

    pub fn click(&mut self, col: u16, row: u16, area: Rect) -> bool {
        col >= area.x && col < area.right() && row >= area.y && row < area.bottom()
            && matches!(self.perform(Cmd::Submit), CmdResult::Submit(_))
    }
}

impl Component for ChromeButton {
    fn view(&mut self, frame: &mut Frame, area: Rect) {
        self.label.view(frame, area);
    }
    fn query<'a>(&'a self, attr: Attribute) -> Option<QueryResult<'a>> {
        self.label.query(attr)
    }
    fn attr(&mut self, attr: Attribute, value: AttrValue) {
        self.label.attr(attr, value);
    }
    fn state(&self) -> State {
        State::None
    }
    fn perform(&mut self, cmd: Cmd) -> CmdResult {
        if matches!(cmd, Cmd::Submit) {
            CmdResult::Submit(self.state())
        } else {
            CmdResult::Invalid(cmd)
        }
    }
}

/// Lay tab buttons left to right with two-space gaps, clipping at the edge.
pub fn layout_topbar(bar: Rect, tabs: &[TopTab]) -> Vec<TopButton> {
    let mut buttons = Vec::new();
    let mut col = bar.x.saturating_add(1);
    let edge = bar.x + bar.width;
    for (index, tab) in tabs.iter().enumerate() {
        let label = format!("[{}]", safe_text::encode_for_display(&tab.label));
        let width = Line::from(label).width().min(u16::MAX as usize) as u16;
        let end = col.saturating_add(width);
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
    pub accent: Option<Color>,
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
                    text: format!("{}:", safe_text::encode_for_display(group)),
                    style: Style::default()
                        .fg(group_palette(tab.group_color.unwrap_or(0)))
                        .add_modifier(Modifier::BOLD),
                    index: None,
                    accent: tab.group_color.map(group_palette),
                });
            }
        }
        segments.push(BarSegment {
            text: format!("{} {}", n + 1, safe_text::encode_for_display(&tab.title)),
            style: if tab.focused {
                let mut style = theme::style(theme::Role::Focus);
                if let Some(color) = tab.group_color {
                    style = style.bg(group_palette(color));
                }
                style
            } else if let Some(color) = tab.group_color {
                Style::default().fg(group_palette(color))
            } else {
                Style::default()
            },
            index: Some(n),
            accent: tab.group_color.map(group_palette),
        });
        prev_group = tab.group.as_deref();
    }
    segments
}

/// The reference strip gives every session its own status dot, group
/// swatch, and numbered click target. Keep the compact strip on small
/// terminals so labels remain usable there.
pub fn session_bar_segments_for_area(tabs: &[SessionTab], bar: Rect) -> Vec<BarSegment> {
    if bar.width < 160 {
        return session_bar_segments(tabs);
    }
    tabs.iter().enumerate().map(|(index, tab)| {
        let status = if tab.live { "●" } else { "○" };
        BarSegment {
            text: format!("{status} ■ [{}] {}  │", index + 1,
                safe_text::encode_for_display(&tab.title)),
            style: if tab.focused {
                theme::style(theme::Role::Focus).add_modifier(Modifier::REVERSED)
            } else {
                theme::style(theme::Role::Text)
            },
            index: Some(index),
            accent: tab.group_color.map(group_palette),
        }
    }).collect()
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
        let width = Line::from(segment.text.as_str())
            .width()
            .min(u16::MAX as usize) as u16;
        let end = col.saturating_add(width);
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
    /// Caller sticky status text (`kind: message`), if one is set.
    pub status: Option<String>,
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

/// Compact sidebar mode-button row. Taller sidebars pin it to the bottom.
pub const SETTINGS_ROW: u16 = 11;

/// Sidebar lines. Never blank: with no sessions it still guides. The
/// active mode button renders highlighted.
pub fn sidebar_lines(info: &SidebarInfo) -> Vec<Line<'static>> {
    sidebar_lines_at(info, SETTINGS_ROW.saturating_sub(1) as usize)
}

fn sidebar_lines_at(info: &SidebarInfo, mode_row: usize) -> Vec<Line<'static>> {
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
            if let Some(status) = detail.status.as_deref() {
                lines.push(Line::from(format!(
                    "  {}",
                    safe_text::encode_for_display(status)
                )));
            }
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
        }
    }
    while lines.len() < mode_row.saturating_sub(1) {
        lines.push(Line::from(""));
    }
    lines.push(Line::from("Settings"));
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

fn rich_sidebar_lines(info: &SidebarInfo, mode_row: usize, width: u16) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(Span::styled("Forge", theme::style(theme::Role::Brand))),
        Line::from(Span::styled("Session Control Plane", theme::style(theme::Role::Muted))),
        Line::from(""),
        Line::from(Span::styled("Session", theme::style(theme::Role::Text).add_modifier(Modifier::BOLD))),
    ];
    match &info.session {
        Some(detail) => {
            lines.push(Line::from(Span::styled(
                safe_text::encode_for_display(&detail.name), theme::style(theme::Role::Focus))));
            lines.push(Line::from(safe_text::encode_for_display(&detail.cli_tool)));
            lines.push(Line::from(safe_text::encode_for_display(&detail.cwd)));
            if let Some(status) = detail.status.as_deref() {
                lines.push(Line::from(safe_text::encode_for_display(status)));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::raw("Status  "),
                Span::styled(format!("● {}", safe_text::encode_for_display(&detail.state)),
                    theme::style(theme::Role::Running)),
            ]));
            lines.push(Line::from(format!("Pending hooks: {}", info.pending)));
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled("Stats", theme::style(theme::Role::Text).add_modifier(Modifier::BOLD))));
            lines.push(stat_line("◷ Uptime", &format_uptime(detail.uptime_secs), width));
            lines.push(Line::from("─".repeat(width.saturating_sub(4) as usize)));
            lines.push(stat_line("Tool calls", &detail.tool_calls.to_string(), width));
            lines.push(Line::from("─".repeat(width.saturating_sub(4) as usize)));
            lines.push(Line::from("Tool Approvals"));
            lines.push(Line::from(vec![
                Span::styled(format!("✓ {}", detail.approvals), theme::style(theme::Role::Success)),
                Span::raw("   "),
                Span::styled(format!("× {}", detail.denials), theme::style(theme::Role::Danger)),
            ]));
            lines.push(Line::from("─".repeat(width.saturating_sub(4) as usize)));
        }
        None => {
            lines.push(Line::from("No session selected"));
            lines.push(Line::from("Ctrl-b c creates one"));
        }
    }
    while lines.len() < mode_row.saturating_sub(2) { lines.push(Line::from("")); }
    lines.push(Line::from(Span::styled("Global Settings", theme::style(theme::Role::Brand))));
    lines.push(Line::from("Autopilot"));
    lines.push(Line::from("")); // button widgets own this row
    lines.push(Line::from(Span::styled("Ctrl-b shortcuts", theme::style(theme::Role::KeyHint))));
    lines
}

fn stat_line(label: &str, value: &str, width: u16) -> Line<'static> {
    let available = width.saturating_sub(2) as usize;
    let spaces = available.saturating_sub(label.chars().count() + value.chars().count());
    Line::from(format!("{label}{}{value}", " ".repeat(spaces.max(1))))
}

/// Click areas for the mode buttons, relative to the sidebar rect. Row is
/// fixed by [`SETTINGS_ROW`]; columns match the rendered button spans.
pub struct ModeButtons {
    pub off: Rect,
    pub yolo: Rect,
}

pub fn mode_button_areas(sidebar: Rect) -> ModeButtons {
    let y = if sidebar.height >= 30 {
        sidebar.bottom().saturating_sub(4)
    } else {
        sidebar.y + SETTINGS_ROW
    };
    let visible = sidebar.height >= SETTINGS_ROW + 2 && sidebar.width >= 16;
    ModeButtons {
        off: Rect::new(sidebar.x + 2, y, if visible { 5 } else { 0 }, 1),
        yolo: Rect::new(sidebar.x + 8, y, if visible { 6 } else { 0 }, 1),
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

/// Chrome snapshots: session-bar tabs plus sidebar state.
pub struct Chrome {
    pub tabs: Vec<SessionTab>,
    pub topbar: TopBar,
    pub detail: Option<SessionDetail>,
    pub pending: usize,
    pub mode: &'static str,
}

/// Render one focused session in the main pane with sidebar and session
/// bar. Titles and bodies are untrusted PTY output, so both pass
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
                            .border_style(theme::style(theme::Role::BorderUnfocused))
                            .title(" forge "),
                    );
            frame.render_widget(hint, areas.main);
        }
    }
    if areas.topbar.height > 0 {
        let buttons = layout_topbar(areas.topbar, &chrome.topbar.tabs);
        for button in &buttons {
            let tab = &chrome.topbar.tabs[button.index];
            let style = if tab.active {
                theme::style(theme::Role::Focus).add_modifier(Modifier::REVERSED)
            } else {
                theme::style(theme::Role::Text)
            };
            let mut control = ChromeButton::new(&format!("[{}]", tab.label), style);
            control.view(frame, Rect::new(button.start, areas.topbar.y,
                button.end - button.start, 1));
        }
    }
    if areas.sidebar.width > 0 && areas.sidebar.height > 0 {
        let info = SidebarInfo {
            session: chrome.detail.clone(),
            pending: chrome.pending,
            mode: chrome.mode,
        };
        let mode_areas = mode_button_areas(areas.sidebar);
        let mode_row = mode_areas.off.y.saturating_sub(areas.sidebar.y + 1) as usize;
        let mut lines = if areas.sidebar.width >= 40 && areas.sidebar.height >= 30 {
            rich_sidebar_lines(&info, mode_row, areas.sidebar.width)
        } else {
            sidebar_lines_at(&info, mode_row)
        };
        if let Some(line) = lines.get_mut(mode_row) {
            *line = Line::from(""); // the controls below own this row
        }
        let side = Paragraph::new(Text::from(lines)).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(theme::style(theme::Role::BorderUnfocused))
                .title(" status "),
        );
        frame.render_widget(side, areas.sidebar);
        for (label, area, active) in [
            ("[Off]", mode_areas.off, info.mode == "off"),
            ("[Yolo]", mode_areas.yolo, info.mode == "yolo"),
        ] {
            if area.width > 0 && area.right() <= areas.sidebar.right().saturating_sub(1) {
                let style = if active {
                    theme::style(theme::Role::Focus).add_modifier(Modifier::REVERSED)
                } else {
                    theme::style(theme::Role::Text)
                };
                ChromeButton::new(label, style).view(frame, area);
            }
        }
    }
    if areas.session_bar.height > 0 {
        let segments = session_bar_segments_for_area(&chrome.tabs, areas.session_bar);
        let buttons = layout_session_bar(areas.session_bar, &segments);
        for (button, segment) in buttons.iter().zip(segments.iter()) {
            let area = Rect::new(button.start, areas.session_bar.y, button.end - button.start, 1);
            if button.index.is_some() {
                ChromeButton::new(&button.label, segment.style).view(frame, area);
                if areas.session_bar.width >= 160 && area.width >= 3 {
                    let accent = segment.accent.unwrap_or(theme::style(theme::Role::Muted).fg.unwrap_or(Color::Reset));
                    Label::default().text("■").style(Style::default().fg(accent))
                        .view(frame, Rect::new(area.x + 2, area.y, 1, 1));
                }
            } else {
                Label::default()
                    .text(safe_text::encode_for_display(&button.label))
                    .style(segment.style)
                    .view(frame, area);
            }
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
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn area() -> Rect {
        Rect::new(0, 0, 80, 24)
    }

    #[test]
    fn chrome_splits_main_sidebar_and_bars() {
        // No status bar: the session bar owns the last row.
        let c = chrome_areas(Rect::new(0, 0, 80, 24));
        assert_eq!(c.topbar, Rect::new(0, 0, 64, 1));
        assert_eq!(c.main, Rect::new(0, 1, 64, 22));
        assert_eq!(c.sidebar, Rect::new(64, 0, 16, 23));
        assert_eq!(c.session_bar, Rect::new(0, 23, 80, 1));
        let wide = chrome_areas(Rect::new(0, 0, 120, 40));
        assert_eq!(wide.topbar, Rect::new(0, 0, 96, 1));
        assert_eq!(wide.main, Rect::new(0, 1, 96, 38));
        assert_eq!(wide.sidebar, Rect::new(96, 0, 24, 39));
        assert_eq!(wide.session_bar, Rect::new(0, 39, 120, 1));
        // Tiny terminals keep content over chrome.
        let tiny = chrome_areas(Rect::new(0, 0, 80, 2));
        assert_eq!(tiny.main.height, 2);
        assert_eq!(tiny.session_bar.height, 0);
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

    #[test]
    fn group_members_share_palette_color_and_focus_stays_visible() {
        let segments = session_bar_segments(&[
            grouped("a", true, "team", 2),
            grouped("b", false, "team", 2),
        ]);
        assert_eq!(segments[1].style.fg, Some(Color::Yellow));
        assert_eq!(segments[1].style.bg, Some(group_palette(2)));
        assert_eq!(segments[2].style.fg, Some(group_palette(2)));
        assert!(segments[1].style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn tall_sidebar_keeps_mode_buttons_near_bottom() {
        let sidebar = chrome_areas(Rect::new(0, 0, 120, 40)).sidebar;
        let buttons = mode_button_areas(sidebar);
        assert_eq!(buttons.off.y, sidebar.bottom() - 4);
        assert_eq!(mode_at(&buttons, buttons.yolo.x, buttons.yolo.y), Some("yolo"));
    }

    #[test]
    fn chrome_button_is_a_submittable_tuirealm_component() {
        use tuirealm::command::{Cmd, CmdResult};
        use tuirealm::component::Component;
        let mut button = ChromeButton::new("[Terminal]", theme::style(theme::Role::Focus));
        assert_eq!(button.perform(Cmd::Submit), CmdResult::Submit(tuirealm::state::State::None));
        assert_eq!(button.state(), tuirealm::state::State::None);
    }

    #[test]
    fn tab_hit_areas_follow_escaped_display_width() {
        let top = layout_topbar(Rect::new(0, 0, 40, 1), &[
            TopTab { label: "A\n界".into(), active: true },
            TopTab { label: "Terminal".into(), active: false },
        ]);
        assert_eq!((top[0].start, top[0].end), (1, 7));
        assert_eq!(top[1].start, 9);
        let segments = session_bar_segments(&[tab("a\nb", true), tab("界", false)]);
        assert_eq!(segments[0].text, "1 a⏎b");
        let buttons = layout_session_bar(Rect::new(0, 0, 40, 1), &segments);
        assert_eq!((buttons[0].start, buttons[0].end), (0, 5));
        assert_eq!(buttons[1].start, 7);
    }

    #[test]
    fn sidebar_drawn_buttons_match_click_rows_without_duplicate_glyphs() {
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| render(f, area(), &[], &chrome())).unwrap();
        let rows = buffer_rows(&terminal);
        assert_eq!(rows[11].chars().skip(66).take(12).collect::<String>(), "[Off] [Yolo]");
        assert!(!rows[12].contains("[Off]"));

        let mut tall = Terminal::new(TestBackend::new(120, 40)).unwrap();
        tall.draw(|f| render(f, Rect::new(0, 0, 120, 40), &[], &chrome())).unwrap();
        let tall_rows = buffer_rows(&tall);
        assert!(tall_rows[35].contains("[Off] [Yolo]"));
        assert!(!tall_rows[11].contains("[Off]"));
    }

    #[test]
    fn short_sidebar_does_not_draw_controls_on_its_border() {
        let mut terminal = Terminal::new(TestBackend::new(80, 14)).unwrap();
        terminal.draw(|f| render(f, Rect::new(0, 0, 80, 14), &[], &chrome())).unwrap();
        let rows = buffer_rows(&terminal);
        // Taller sidebar earns its button row; the bottom border stays clean.
        assert!(rows[11].contains("[Off]"), "buttons visible: {:?}", rows[11]);
        assert!(!rows[12].contains("[Off]"), "border clean: {:?}", rows[12]);
    }

    #[test]
    fn wide_sidebar_matches_control_plane_sections() {
        let areas = chrome_areas(Rect::new(0, 0, 180, 40));
        assert_eq!(areas.sidebar.width, 45);
        let mut c = chrome();
        c.detail = Some(SessionDetail {
            name: "jarvis_senior".into(), cli_tool: "Codex".into(),
            cwd: "/work/jarvis".into(), state: "PROGRESS".into(),
            status: None,
            uptime_secs: 3600, tool_calls: 489, approvals: 345, denials: 8,
        });
        let mut terminal = Terminal::new(TestBackend::new(180, 40)).unwrap();
        terminal.draw(|f| render(f, Rect::new(0, 0, 180, 40), &[], &c)).unwrap();
        let rows = buffer_rows(&terminal);
        let sidebar_text = rows.iter().map(|row| row.chars().skip(areas.sidebar.x as usize)
            .collect::<String>()).collect::<Vec<_>>().join("\n");
        assert!(sidebar_text.contains("Session Control Plane"));
        assert!(sidebar_text.contains("jarvis_senior"));
        assert!(sidebar_text.contains("Status"));
        assert!(sidebar_text.contains("Tool Approvals"));
        assert!(sidebar_text.contains("Global Settings"));
        assert!(sidebar_text.contains("[Off] [Yolo]"));
    }

    #[test]
    fn wide_session_strip_uses_status_group_chips_and_clickable_numbers() {
        let tabs = [grouped("jarvis_dev", false, "aura", 0), grouped("web_client", true, "aura", 0),
            grouped("gl_rev", false, "gl", 1)];
        let segments = session_bar_segments_for_area(&tabs, Rect::new(0, 38, 180, 1));
        assert_eq!(segments.len(), 3);
        assert!(segments[0].text.contains("● ■ [1] jarvis_dev"));
        assert!(segments[1].text.contains("[2] web_client"));
        assert_eq!(segments[0].accent, Some(group_palette(0)));
        assert_eq!(segments[2].accent, Some(group_palette(1)));
        let buttons = layout_session_bar(Rect::new(0, 38, 180, 1), &segments);
        assert_eq!(session_at(&buttons, buttons[1].start + 5), Some(1));
    }

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
                status: Some("blocked: waiting on review".to_string()),
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
        assert!(has(&lines, "blocked: waiting on review"), "status: {lines:?}");
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
    fn topbar_layout_measures_wide_emoji_icons() {
        // Emoji icons are two cells: hit areas must use display width,
        // not char count, or clicks land one cell off per icon.
        let tabs = vec![
            TopTab { label: "🤖 Codex".to_string(), active: true },
            TopTab { label: "💻 Terminal".to_string(), active: false },
        ];
        let buttons = layout_topbar(Rect::new(0, 0, 40, 1), &tabs);
        assert_eq!(buttons.len(), 2);
        // "[🤖 Codex]" spans 10 cells: brackets + icon + space + name.
        assert_eq!((buttons[0].start, buttons[0].end), (1, 11));
        // "[💻 Terminal]" spans 13 cells starting after the 2-cell gap.
        assert_eq!((buttons[1].start, buttons[1].end), (13, 26));
        assert_eq!(topbar_at(&buttons, 11), None, "gap is dead");
        assert_eq!(topbar_at(&buttons, 13), Some(1));
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
        use crate::pty::{CellColor, CellFormat};
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
