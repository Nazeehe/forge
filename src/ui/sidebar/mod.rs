//! UI sidebar: fleet, focus detail, footer, mode buttons, paint.

pub mod fleet;

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;
use tuirealm::component::Component;

use crate::infra::safe_text;

use super::layout::ChromeAreas;
use super::session_bar::render_pill;
use super::text::cut_cells;
use super::tetris::{tetris_line, tetris_lines};
use super::theme;
use super::topbar::ChromeButton;
use super::Chrome;
use fleet::{FleetItem, fleet_block_heights, fleet_block_lines, fleet_items_budget, fleet_window_items};

/// One armed self-injection timer for the sidebar: stable ID plus a
/// preformatted countdown (`9:55`), soonest first.
#[derive(Clone, Debug)]
pub struct TimerView {
    pub id: String,
    pub remaining: String,
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
    /// Kind behind `status`, for the signal emoji. `None` matches `status`.
    pub status_kind: Option<crate::session::status::StatusKind>,
    /// Armed timers of the focused session only; empty hides the section.
    pub timers: Vec<TimerView>,
}

/// Fleet attention tier. Sorts Attention first, then Working, then
/// Idle; the mark carries meaning alone, color only reinforces it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum FleetTier {
    Attention,
    Working,
    Idle,
}

impl FleetTier {
    pub fn mark(self) -> &'static str {
        match self {
            FleetTier::Attention => "!",
            FleetTier::Working => "~",
            FleetTier::Idle => " ",
        }
    }
}

/// One router row: every session, attention-first. The reason is the
/// attention/sticky text verbatim (already display-encoded).
#[derive(Clone, Debug)]
pub struct FleetRow {
    pub id: crate::session::SessionId,
    pub name: String,
    pub tier: FleetTier,
    pub state: String,
    pub reason: String,
    /// Signal emoji (`🔔` for pings, the sticky kind glyph otherwise,
    /// empty when there is nothing to signal). Marks still carry the
    /// meaning alone; this rides beside them.
    pub emoji: &'static str,
}

/// Signal emoji per sticky kind. Bare glyphs only: no variation
/// selectors, no ZWJ sequences (same rule as the topbar icons), so
/// `Line::width` measures them exactly.
pub fn status_emoji(kind: crate::session::status::StatusKind) -> &'static str {
    match kind {
        crate::session::status::StatusKind::Info => "ℹ",
        crate::session::status::StatusKind::Progress => "🔄",
        crate::session::status::StatusKind::Success => "✔",
        crate::session::status::StatusKind::Warning => "⚠",
        crate::session::status::StatusKind::Blocked => "🛑",
        crate::session::status::StatusKind::Question => "💬",
    }
}

#[derive(Clone, Debug)]
pub struct SidebarInfo {
    pub session: Option<SessionDetail>,
    /// Every session, attention-first; the fleet slot scrolls past the cap.
    pub sessions: Vec<FleetRow>,
    /// Focused session for the `*` mark.
    pub active: Option<crate::session::SessionId>,
    /// Fleet cursor for `>` plus keyboard activation.
    pub fleet_cursor: Option<crate::session::SessionId>,
    /// Clamped scroll offset into `sessions`.
    pub fleet_scroll: usize,
    /// Armed timers on non-focused sessions, collapsed to a count.
    pub other_timers: usize,
    pub pending: usize,
    /// "off" or "yolo": drives which settings button highlights.
    pub mode: &'static str,
    /// Board view open: drives the Kanban button highlight.
    pub board_open: bool,
    /// "on" or "off": Telegram mobile transport state.
    pub telegram: &'static str,
    /// Latest `message_user` badge as `(session, text)`, if any. The
    /// badge row renders only when present, last in the panel where no
    /// interactive row can shift.
    pub telegram_badge: Option<(String, String)>,
}

/// Explicit sidebar regions: the list scrolls, the footer never
/// moves. Render, mouse, and keys share this split so a repaint can
/// never desync controls from the paint.
pub struct SidebarLayout {
    pub list: Rect,
    pub footer: Rect,
}

/// Rich layout gate: wide and tall enough for the brand header,
/// status rows, and settings explainers. Calibrated to the 36-column
/// clamp — a plain `>= 40` would make rich unreachable.
pub fn sidebar_is_rich(sidebar: Rect) -> bool {
    sidebar.width >= 30 && sidebar.height >= 30
}

/// Pinned footer height: settings rows, the mode buttons, a blank gap
/// row, the Kanban row, another blank gap row, the Tetris row, plus
/// the badge row when present. Compact also spends a pending row,
/// hidden when there is nothing pending (rich pending lives in the
/// focused block instead).
pub fn sidebar_footer_height(info: &SidebarInfo, rich: bool) -> u16 {
    let base = if rich { 10 } else { 9 };
    base + u16::from(info.telegram_badge.is_some())
        - u16::from(!rich && info.pending == 0)
}

pub fn sidebar_layout(sidebar: Rect, footer_h: u16) -> SidebarLayout {
    let footer_h = footer_h.min(sidebar.height);
    SidebarLayout {
        footer: Rect::new(
            sidebar.x,
            sidebar.y + sidebar.height - footer_h,
            sidebar.width,
            footer_h,
        ),
        list: Rect::new(sidebar.x, sidebar.y, sidebar.width, sidebar.height - footer_h),
    }
}

/// Click areas for fleet blocks: name plus reason rows over the same
/// window the render paints, so clicks can never desync from what is
/// on screen. Gap rows stay dead.
pub fn sidebar_session_rects(
    sidebar: Rect,
    info: &SidebarInfo,
    rich: bool,
) -> Vec<(crate::session::SessionId, Rect)> {
    let width = sidebar.width;
    let layout = sidebar_layout(sidebar, sidebar_footer_height(info, rich));
    let items = fleet_window_items(
        info,
        width,
        fleet_items_budget(info, rich, width, layout.list.height),
    );
    let heights = fleet_block_heights(info, width);
    // Title row sits after the brand, focused block, and gap row.
    let title_idx =
        (if rich { 3 } else { 0 }) + focused_block_lines(info, rich, width).len() + 1;
    let mut y = layout.list.y + 1 + title_idx as u16 + 1;
    let mut out = Vec::new();
    for item in &items {
        match item {
            FleetItem::AttnHeader | FleetItem::SessionsHeader => {
                y += 1;
            }
            FleetItem::Block(i) => {
                let h = heights[*i];
                // The click area covers name plus reason rows; the gap
                // and headers stay dead.
                let w = width.saturating_sub(2);
                out.push((
                    info.sessions[*i].id,
                    Rect::new(sidebar.x + 1, y, w, h.saturating_sub(1).max(1)),
                ));
                y += h;
            }
        }
    }
    out
}

/// Compact sidebar mode-button row. Taller sidebars pin it to the bottom.
pub const SETTINGS_ROW: u16 = 11;

/// Compact countdown: `0:07`, `9:55`, `1:00:00`. Past-due saturates.
pub fn format_countdown(remaining: std::time::Duration) -> String {
    let secs = remaining.as_secs();
    let (hours, mins, secs) = (secs / 3600, secs % 3600 / 60, secs % 60);
    if hours > 0 {
        format!("{hours}:{mins:02}:{secs:02}")
    } else {
        format!("{mins}:{secs:02}")
    }
}

/// Sidebar lines. Never blank: with no sessions it still guides. The
/// active mode button renders highlighted. Width 24 stands in for the
/// narrowest compact sidebar.
pub fn sidebar_lines(info: &SidebarInfo) -> Vec<Line<'static>> {
    sidebar_lines_at(info, false, 24, 30).0
}

/// Focused-session block for the compact layout: detail first, fleet
/// below. Shared by the render and the fleet-offset math so clicks
/// track the paint.
fn compact_focused_lines(info: &SidebarInfo, _width: u16) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(" Session")];
    let Some(detail) = info.session.as_ref() else {
        lines.push(Line::from(" No session selected"));
        return lines;
    };
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
        let glyph = detail
            .status_kind
            .map(|k| format!("{} ", status_emoji(k)))
            .unwrap_or_default();
        lines.push(Line::from(format!(
            "  {glyph}{}",
            safe_text::encode_for_display(status)
        )));
    }
    if !detail.timers.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(" Scheduled"));
        for timer in &detail.timers {
            lines.push(Line::from(format!("  ◷ in {}", timer.remaining)));
        }
    }
    if info.other_timers > 0 {
        lines.push(Line::from(format!("  ◷ +{} elsewhere", info.other_timers)));
    }
    lines
}

/// Painted sidebar: exactly `height` rows. The list part (brand,
/// focused block, budgeted fleet window) is clipped to the list
/// region; the footer is always appended whole, so long content clips
/// the list and never the controls. Render, hit-testing, and buttons
/// share this one builder.
pub struct SidebarPaint {
    pub lines: Vec<Line<'static>>,
    /// Content row of the mode-button widgets (footer-relative).
    pub btn_row: usize,
    /// Content row of the Kanban button: always `btn_row + 2`, with a
    /// blank gap row breathing between it and the autopilot buttons.
    pub kanban_row: usize,
    /// Content row of the Tetris button: always `kanban_row + 2`,
    /// with a blank gap row breathing between it and Kanban.
    pub tetris_row: usize,
}

pub fn sidebar_paint(
    info: &SidebarInfo,
    rich: bool,
    width: u16,
    height: u16,
    pills: bool,
) -> SidebarPaint {
    let footer_h = sidebar_footer_height(info, rich) as usize;
    let height = height as usize;
    let list_h = height.saturating_sub(footer_h);
    let mut list = Vec::new();
    if rich {
        list.push(Line::from(Span::styled(" Forge", theme::style(theme::Role::Brand))));
        list.push(Line::from(Span::styled(
            " Session Control Plane",
            theme::style(theme::Role::Muted),
        )));
        list.push(Line::from(""));
    }
    list.extend(focused_block_lines(info, rich, width));
    list.push(Line::from(""));
    let items = fleet_window_items(
        info,
        width,
        fleet_items_budget(info, rich, width, list_h as u16),
    );
    list.extend(fleet_block_lines(info, width, &items));
    list.truncate(list_h);
    while list.len() < list_h {
        list.push(Line::from(""));
    }
    let btn_row = list_h + if rich { 3 } else { 1 };
    let mut lines = list;
    lines.extend(footer_lines(info, rich, pills));
    SidebarPaint {
        lines,
        kanban_row: btn_row + 2,
        tetris_row: btn_row + 4,
        btn_row,
    }
}

/// Kanban button row, one blank gap row below the mode buttons. The
/// line carries the same text the overlay widget paints, so content
/// readers agree with the buffer; an open board fills like the active
/// mode pill.
fn kanban_line(info: &SidebarInfo, pills: bool) -> Line<'static> {
    if pills {
        let (style, cap) = if info.board_open {
            (theme::style(theme::Role::TabActive), Color::Yellow)
        } else {
            (theme::style(theme::Role::TabInactive), Color::DarkGray)
        };
        Line::from(vec![
            Span::raw("  "),
            Span::styled(crate::ui::theme::pill_left().to_string(), Style::default().fg(cap)),
            Span::styled(" Kanban ", style),
            Span::styled(crate::ui::theme::pill_right().to_string(), Style::default().fg(cap)),
        ])
    } else if info.board_open {
        Line::from(vec![
            Span::raw("  "),
            Span::styled(
                "[Kanban]",
                Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED),
            ),
        ])
    } else {
        Line::from(vec![Span::raw("  "), Span::styled("[Kanban]", Style::default())])
    }
}

/// Pinned footer rows, exactly `sidebar_footer_height` long: settings,
/// mode buttons, a blank gap row, Kanban, another blank gap row,
/// Tetris, shortcuts, transport, badge. The button lines carry the
/// same text the overlay widgets paint, so content readers agree with
/// the buffer.
fn footer_lines(info: &SidebarInfo, rich: bool, pills: bool) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if rich {
        lines.push(Line::from(Span::styled(
            " Global Settings",
            theme::style(theme::Role::Brand),
        )));
        lines.push(Line::from(" Autopilot"));
        lines.push(Line::from(""));
    } else {
        lines.push(Line::from(" Settings"));
    }
    let (off_style, yolo_style) = if info.mode == "yolo" {
        (Style::default(), Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED))
    } else {
        (Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED), Style::default())
    };
    lines.push(if pills {
        // Pill mode buttons: rounded ends, centered labels, the active
        // mode filled. The overlay widgets below paint the same cells.
        let (off_style, yolo_style) = if info.mode == "yolo" {
            (
                theme::style(theme::Role::TabInactive),
                theme::style(theme::Role::TabActive),
            )
        } else {
            (
                theme::style(theme::Role::TabActive),
                theme::style(theme::Role::TabInactive),
            )
        };
        let (off_cap, yolo_cap) = if info.mode == "yolo" {
            (Color::DarkGray, Color::Yellow)
        } else {
            (Color::Yellow, Color::DarkGray)
        };
        Line::from(vec![
            Span::raw("  "),
            Span::styled(crate::ui::theme::pill_left().to_string(), Style::default().fg(off_cap)),
            Span::styled(" Off ", off_style),
            Span::styled(crate::ui::theme::pill_right().to_string(), Style::default().fg(off_cap)),
            Span::raw(" "),
            Span::styled(crate::ui::theme::pill_left().to_string(), Style::default().fg(yolo_cap)),
            Span::styled(" Yolo ", yolo_style),
            Span::styled(crate::ui::theme::pill_right().to_string(), Style::default().fg(yolo_cap)),
        ])
    } else {
        Line::from(vec![
            Span::raw("  "),
            Span::styled("[Off]", off_style),
            Span::raw(" "),
            Span::styled("[Yolo]", yolo_style),
        ])
    });
    lines.push(Line::from(""));
    lines.push(kanban_line(info, pills));
    lines.push(Line::from(""));
    lines.push(tetris_line(pills));
    if rich {
        lines.push(Line::from(Span::styled(
            " Ctrl-b shortcuts",
            theme::style(theme::Role::KeyHint),
        )));
    } else {
        lines.push(Line::from(""));
        // Zero pending hides: the footer only spends a row when there
        // is something to show.
        if info.pending > 0 {
            lines.push(Line::from(format!(" Pending: {}", info.pending)));
        }
    }
    lines.push(telegram_line(info.telegram));
    if let Some(badge) = telegram_badge_line(&info.telegram_badge) {
        lines.push(badge);
    }
    lines
}

pub(super) fn sidebar_lines_at(
    info: &SidebarInfo,
    pills: bool,
    width: u16,
    height: u16,
) -> (Vec<Line<'static>>, usize) {
    let paint = sidebar_paint(info, false, width, height, pills);
    (paint.lines, paint.btn_row)
}

/// Mode-button hit areas on the pinned footer row: the paint puts the
/// buttons at the same footer-relative row, so clicks never desync
/// from what is on screen no matter how long the list above gets.
pub fn footer_mode_buttons(
    sidebar: Rect,
    info: &SidebarInfo,
    rich: bool,
    pills: bool,
) -> ModeButtons {
    let layout = sidebar_layout(sidebar, sidebar_footer_height(info, rich));
    // Content row i paints at screen y+1+i (border consumes the first
    // row), so the footer-relative button row shifts one down.
    mode_button_areas_at(
        sidebar,
        pills,
        layout.footer.y + 1 + if rich { 3 } else { 1 },
    )
}

/// Focused-session block for the rich layout, shared by the render
/// and the fleet-offset math so clicks track the paint.
fn rich_focused_lines(info: &SidebarInfo, width: u16) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(Span::styled(" Session", theme::style(theme::Role::Text).add_modifier(Modifier::BOLD))),
    ];
    let Some(detail) = info.session.as_ref() else {
        lines.push(Line::from(" No session selected"));
        return lines;
    };
    lines.push(Line::from(Span::styled(
        format!(" {}", safe_text::encode_for_display(&detail.name)), theme::style(theme::Role::Focus))));
    lines.push(Line::from(format!(" {}", safe_text::encode_for_display(&detail.cli_tool))));
    lines.push(Line::from(format!(" {}", safe_text::encode_for_display(&detail.cwd))));
    if let Some(status) = detail.status.as_deref() {
        let glyph = detail
            .status_kind
            .map(|k| format!("{} ", status_emoji(k)))
            .unwrap_or_default();
        lines.push(Line::from(format!(
            " {glyph}{}",
            safe_text::encode_for_display(status)
        )));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::raw(" Status  "),
        Span::styled(format!("● {}", safe_text::encode_for_display(&detail.state)),
            theme::style(theme::Role::Running)),
    ]));
    if info.pending > 0 {
        lines.push(Line::from(format!(" Pending hooks: {}", info.pending)));
    }
    lines.push(Line::from(""));
    if !detail.timers.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            " Scheduled",
            theme::style(theme::Role::Text).add_modifier(Modifier::BOLD),
        )));
        for timer in &detail.timers {
            lines.push(Line::from(format!("  ◷ in {}", timer.remaining)));
        }
    }
    if info.other_timers > 0 {
        lines.push(Line::from(format!("  ◷ +{} elsewhere", info.other_timers)));
    }
    lines.push(rule_line(width));
    lines
}

/// Focused block dispatcher for the fleet offset: same branch the
/// render takes, so the header index always matches the paint.
pub(super) fn focused_block_lines(info: &SidebarInfo, rich: bool, width: u16) -> Vec<Line<'static>> {
    if rich {
        rich_focused_lines(info, width)
    } else {
        compact_focused_lines(info, width)
    }
}

pub(super) fn rich_sidebar_lines(info: &SidebarInfo, width: u16, height: u16) -> Vec<Line<'static>> {
    // Text keeps one indent cell off the border; rules share the
    // narrower measure so the right edge stays aligned.
    sidebar_paint(info, true, width, height, false).lines
}

/// Telegram transport row: state plus the settings key. Appended after
/// the pinned rows so it never shifts mode buttons or shortcuts.
fn telegram_line(state: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!(" Telegram {state}"), theme::style(theme::Role::Text)),
        Span::styled(" · Ctrl-b m", theme::style(theme::Role::KeyHint)),
    ])
}

/// Latest operator badge row, if any: session plus escaped text capped
/// at 48 chars. Raw input stays the policy input; only the display is
/// encoded (see [`safe_text::encode_for_display`]).
fn telegram_badge_line(badge: &Option<(String, String)>) -> Option<Line<'static>> {
    let (session, text) = badge.as_ref()?;
    let raw = format!("{session}: {text}");
    let cut = cut_cells(&raw, 48);
    Some(Line::from(format!(" {}", safe_text::encode_for_display(&cut))))
}

/// Divider rule: one indent cell, then dashes to the common right edge.
fn rule_line(width: u16) -> Line<'static> {
    Line::from(format!(" {}", "─".repeat(width.saturating_sub(6) as usize)))
}

/// Click areas for the mode buttons, relative to the sidebar rect. Row is
/// fixed by [`SETTINGS_ROW`]; columns match the rendered button spans.
pub struct ModeButtons {
    pub off: Rect,
    pub yolo: Rect,
}

/// Cancel-button rects for the painted Scheduled rows, keyed by timer
/// ID. Recomputed from live info by the render and mouse paths alike,
/// so both always agree on which row cancels which timer. The header
/// matches exactly plus a timer-row lookahead, so a session literally
/// named "Scheduled" can never hijack it; clipped rows get no button.
pub(crate) fn timer_cancel_rects(
    sidebar: Rect,
    info: &SidebarInfo,
    pills: bool,
) -> Vec<(String, Rect)> {
    let Some(detail) = info.session.as_ref() else {
        return Vec::new();
    };
    if detail.timers.is_empty() {
        return Vec::new();
    }
    // Row math only needs the y, which pills never move.
    let rich = sidebar_is_rich(sidebar);
    let lines = sidebar_paint(info, rich, sidebar.width, sidebar.height, false).lines;
    let mut header = None;
    for (idx, line) in lines.iter().enumerate() {
        // Headers carry one indent cell now; the timer-row lookahead
        // below still keeps a session literally named Scheduled from
        // matching.
        let exact = line.spans.len() == 1 && line.spans[0].content.trim() == "Scheduled";
        let next_row = lines.get(idx + 1).is_some_and(|next| {
            let text: String = next.spans.iter().map(|s| s.content.as_ref()).collect();
            text.starts_with("  ◷ in ") || text.starts_with("◷ in ")
        });
        if exact && next_row {
            header = Some(idx);
            break;
        }
    }
    let Some(header) = header else {
        return Vec::new();
    };
    // "[Cancel]" is 8 cells, the pill 10. Rich sidebars right-align it
    // to the divider edge (4 shy of the rect); compact ones have no
    // rules, so the button hugs the border instead.
    let edge = if sidebar_is_rich(sidebar) {
        sidebar.right().saturating_sub(3)
    } else {
        sidebar.right().saturating_sub(1)
    };
    let wide = if pills { 10 } else { 8 };
    let x = edge.saturating_sub(wide).max(sidebar.x + 1);
    if edge.saturating_sub(x) < wide {
        return Vec::new();
    }
    detail
        .timers
        .iter()
        .enumerate()
        .filter_map(|(i, timer)| {
            let row = header + 1 + i;
            if row >= lines.len() {
                return None;
            }
            let y = sidebar.y + 1 + row as u16;
            if y + 1 >= sidebar.bottom() {
                return None;
            }
            Some((timer.id.clone(), Rect::new(x, y, wide, 1)))
        })
        .collect()
}

/// Mode-button hit areas: `[Off]`/`[Yolo]` legacy, pill containers
/// (` Off ` 7 wide, ` Yolo ` 8 wide) when `pills`. Rows never move.
/// Tall sidebars sit six rows above the bottom edge: gap, Kanban,
/// gap, Tetris, shortcuts, and transport fill the rows below.
pub fn mode_button_areas(sidebar: Rect, pills: bool) -> ModeButtons {
    let y = if sidebar.height >= 30 {
        sidebar.bottom().saturating_sub(6)
    } else {
        sidebar.y + SETTINGS_ROW
    };
    mode_button_areas_at(sidebar, pills, y)
}

/// Mode-button hit areas on an explicit row: the compact render and
/// mouse paths pass the content-built row so the buttons follow long
/// content instead of doubling against a fixed overlay.
pub fn mode_button_areas_at(sidebar: Rect, pills: bool, y: u16) -> ModeButtons {
    let visible = sidebar.height >= SETTINGS_ROW + 2 && sidebar.width >= 16;
    let (off_w, yolo_x, yolo_w) = if pills {
        (7, sidebar.x + 10, 8)
    } else {
        (5, sidebar.x + 8, 6)
    };
    ModeButtons {
        off: Rect::new(sidebar.x + 2, y, if visible { off_w } else { 0 }, 1),
        yolo: Rect::new(yolo_x, y, if visible { yolo_w } else { 0 }, 1),
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

/// Kanban button hit area: two content rows below the mode buttons
/// (a blank gap row breathes between) under the same visibility gate,
/// so clicks never desync from the paint. Legacy `[Kanban]` is 8
/// wide, the pill 10.
pub fn board_button_area(sidebar: Rect, info: &SidebarInfo, rich: bool, pills: bool) -> Rect {
    let mode = footer_mode_buttons(sidebar, info, rich, pills);
    let visible = sidebar.height >= SETTINGS_ROW + 2 && sidebar.width >= 16;
    Rect::new(sidebar.x + 2, mode.off.y + 2, if visible { if pills { 10 } else { 8 } } else { 0 }, 1)
}

/// Tetris button hit area: two content rows below Kanban (another
/// blank gap row breathes between) under the same visibility gate,
/// so clicks never desync from the paint. Legacy `[Tetris]` is 8
/// wide, the pill 10, exactly like Kanban.
pub fn tetris_button_area(sidebar: Rect, info: &SidebarInfo, rich: bool, pills: bool) -> Rect {
    let mode = footer_mode_buttons(sidebar, info, rich, pills);
    let visible = sidebar.height >= SETTINGS_ROW + 2 && sidebar.width >= 16;
    Rect::new(sidebar.x + 2, mode.off.y + 4, if visible { if pills { 10 } else { 8 } } else { 0 }, 1)
}

/// Sidebar panel with mode buttons and timer cancels.
pub(super) fn render_sidebar(frame: &mut Frame, areas: &ChromeAreas, chrome: &Chrome) {
    if areas.sidebar.width > 0 && areas.sidebar.height > 0 {
        let info = SidebarInfo {
            session: chrome.detail.clone(),
            sessions: chrome.sessions.clone(),
            active: chrome.active,
            fleet_cursor: chrome.fleet_cursor,
            fleet_scroll: chrome.fleet_scroll,
            other_timers: chrome.other_timers,
            pending: chrome.pending,
            mode: chrome.mode,
            board_open: chrome.board_open,
            telegram: chrome.telegram,
            telegram_badge: chrome.telegram_badge.clone(),
        };
        let rich = sidebar_is_rich(areas.sidebar);
        // One shared paint: list clipped above, footer pinned below,
        // buttons overlaid on the footer row. The mouse path rebuilds
        // the same paint, so controls track it exactly.
        let paint = sidebar_paint(
            &info,
            rich,
            areas.sidebar.width,
            areas.sidebar.height,
            chrome.pills,
        );
        let mut lines = paint.lines;
        let btn_areas =
            footer_mode_buttons(areas.sidebar, &info, rich, chrome.pills);
        let btn_row = btn_areas.off.y.saturating_sub(areas.sidebar.y + 1) as usize;
        if let Some(line) = lines.get_mut(btn_row) {
            *line = Line::from(""); // the controls below own this row
        }
        // The Kanban button owns its gap-separated row below the mode
        // buttons.
        let board_area = board_button_area(areas.sidebar, &info, rich, chrome.pills);
        let kanban_row = board_area.y.saturating_sub(areas.sidebar.y + 1) as usize;
        if let Some(line) = lines.get_mut(kanban_row) {
            *line = Line::from("");
        }
        // The Tetris button owns its gap-separated row below Kanban;
        // the game itself replaces the list region above while open.
        let tetris_area = tetris_button_area(areas.sidebar, &info, rich, chrome.pills);
        let tetris_row = tetris_area.y.saturating_sub(areas.sidebar.y + 1) as usize;
        if let Some(line) = lines.get_mut(tetris_row) {
            *line = Line::from("");
        }
        if chrome.tetris_open {
            if let Some(game) = chrome.tetris.as_ref() {
                let footer_h = sidebar_footer_height(&info, rich) as usize;
                let list_h = lines.len().saturating_sub(footer_h);
                for (i, game_line) in
                    tetris_lines(game, areas.sidebar.width, list_h as u16)
                        .into_iter()
                        .enumerate()
                {
                    if let Some(slot) = lines.get_mut(i) {
                        *slot = game_line;
                    }
                }
            }
        }
        let side = Paragraph::new(Text::from(lines)).block(
            Block::default()
                .borders(Borders::ALL)
                    .border_type(crate::ui::theme::border_type())
                .border_style(theme::style(theme::Role::BorderUnfocused))
                .title(" status "),
        );
        frame.render_widget(side, areas.sidebar);
        if board_area.width > 0 && board_area.right() <= areas.sidebar.right().saturating_sub(1) {
            if chrome.pills {
                let (style, left, right) = theme::button_chrome(
                    info.board_open,
                    theme::style(theme::Role::TabActive),
                    theme::style(theme::Role::TabInactive),
                    Color::DarkGray,
                );
                render_pill(frame, board_area, "Kanban", style, left, right);
            } else {
                let style = if info.board_open {
                    theme::style(theme::Role::Focus).add_modifier(Modifier::REVERSED)
                } else {
                    theme::style(theme::Role::Text)
                };
                ChromeButton::new("[Kanban]", style).view(frame, board_area);
            }
        }
        if tetris_area.width > 0 && tetris_area.right() <= areas.sidebar.right().saturating_sub(1) {
            if chrome.pills {
                let (style, left, right) = theme::button_chrome(
                    chrome.tetris_open,
                    theme::style(theme::Role::TabActive),
                    theme::style(theme::Role::TabInactive),
                    Color::DarkGray,
                );
                render_pill(frame, tetris_area, "Tetris", style, left, right);
            } else {
                let style = if chrome.tetris_open {
                    theme::style(theme::Role::Focus).add_modifier(Modifier::REVERSED)
                } else {
                    theme::style(theme::Role::Text)
                };
                ChromeButton::new("[Tetris]", style).view(frame, tetris_area);
            }
        }
        for (label, area, active) in [
            ("Off", btn_areas.off, info.mode == "off"),
            ("Yolo", btn_areas.yolo, info.mode == "yolo"),
        ] {
            if area.width > 0 && area.right() <= areas.sidebar.right().saturating_sub(1) {
                if chrome.pills {
                    let (style, left, right) = theme::button_chrome(
                        active,
                        theme::style(theme::Role::TabActive),
                        theme::style(theme::Role::TabInactive),
                        Color::DarkGray,
                    );
                    render_pill(frame, area, label, style, left, right);
                } else {
                    let style = if active {
                        theme::style(theme::Role::Focus).add_modifier(Modifier::REVERSED)
                    } else {
                        theme::style(theme::Role::Text)
                    };
                    ChromeButton::new(&format!("[{label}]"), style).view(frame, area);
                }
            }
        }
        // One red Cancel per armed timer, over its own countdown row.
        // The pill is the destructive variant: red container, dark label.
        for (_, area) in timer_cancel_rects(areas.sidebar, &info, chrome.pills) {
            if chrome.pills {
                render_pill(
                    frame,
                    area,
                    "Cancel",
                    Style::default().fg(Color::Black).bg(Color::Red),
                    Color::Red,
                    Color::Red,
                );
            } else {
                ChromeButton::new(
                    "[Cancel]",
                    theme::style(theme::Role::Danger).add_modifier(Modifier::REVERSED),
                )
                .view(frame, area);
            }
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use crate::ui::board::board_at;
    use crate::ui::layout::chrome_areas;
    use crate::ui::render;
    use crate::ui::session_bar::PILL_LEFT;
    use crate::ui::test_support::{area, buffer_rows, buffer_text, chrome, for_h_fleet, has, sidebar_chrome, timed_detail};

    #[test]
    fn tall_sidebar_keeps_mode_buttons_near_bottom() {
        let sidebar = chrome_areas(Rect::new(0, 0, 120, 40)).sidebar;
        let buttons = mode_button_areas(sidebar, false);
        // Six rows below: gap, Kanban, gap, Tetris, shortcuts, transport.
        assert_eq!(buttons.off.y, sidebar.bottom() - 6);
        assert_eq!(mode_at(&buttons, buttons.yolo.x, buttons.yolo.y), Some("yolo"));
    }

    #[test]
    fn pill_mode_buttons_widen_hit_areas() {
        let sidebar = chrome_areas(Rect::new(0, 0, 80, 24)).sidebar;
        let buttons = mode_button_areas(sidebar, true);
        assert_eq!(buttons.off.width, 7, "off pill");
        assert_eq!(buttons.yolo.x, buttons.off.x + 8, "one gap cell");
        assert_eq!(buttons.yolo.width, 8, "yolo pill");
        assert_eq!(buttons.off.y, buttons.yolo.y, "same row as legacy");
        assert_eq!(mode_at(&buttons, buttons.off.x, buttons.off.y), Some("off"));
        assert_eq!(mode_at(&buttons, buttons.yolo.x + 7, buttons.yolo.y), Some("yolo"), "right cap hits");
        assert_eq!(mode_at(&buttons, buttons.off.x + 7, buttons.off.y), None, "gap misses");
    }

    #[test]
    fn pill_cancel_rects_widen_and_keep_edge() {
        let info = SidebarInfo { session: Some(timed_detail()), sessions: Vec::new(), active: None, fleet_cursor: None, fleet_scroll: 0, other_timers: 0, pending: 0, mode: "off", telegram: "off", telegram_badge: None, board_open: false };
        let areas = chrome_areas(Rect::new(0, 0, 180, 40));
        let rects = timer_cancel_rects(areas.sidebar, &info, true);
        assert_eq!(rects.len(), 2);
        for (_, area) in &rects {
            assert_eq!(area.width, 10, "pill Cancel width");
            assert_eq!(area.right(), areas.sidebar.right() - 3, "keeps divider edge");
        }
    }

    #[test]
    fn render_pill_mode_buttons_and_cancel() {
        let mut c = chrome();
        c.pills = true;
        c.detail = Some(timed_detail());
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| render(f, area(), &[], &c)).unwrap();
        let rows = buffer_rows(&terminal);
        // The pinned footer puts the button row at 17 with a blank gap
        // at 18, Kanban at 19, another blank gap at 20, and Tetris at
        // 21; pills still replace the brackets there, and the fixed row
        // stays blank.
        assert!(!rows[11].contains("Off"), "no ghost row: {:?}", rows[11]);
        assert!(rows[17].contains("Off"), "off pill: {:?}", rows[17]);
        assert!(!rows[17].contains("[Off]"), "no legacy brackets");
        assert!(!rows[18].contains("Kanban"), "gap stays blank: {:?}", rows[18]);
        assert!(rows[19].contains("Kanban"), "kanban pill: {:?}", rows[19]);
        assert!(rows[21].contains("Tetris"), "tetris pill: {:?}", rows[21]);
        let cancel_y = rows.iter().position(|r| r.contains("Cancel")).expect("cancel pill");
        assert!(rows[cancel_y].contains("\u{e0b6}"));
        let buf = terminal.backend().buffer();
        assert_eq!(buf[(66, 17)].fg, Color::Yellow, "active off cap");
        assert_eq!(buf[(67, 17)].bg, Color::Yellow, "active off fill");
        let cap_byte = rows[cancel_y].find("\u{e0b6}").expect("left cap");
        let cap_x = rows[cancel_y][..cap_byte].chars().count() as u16;
        assert_eq!(buf[(cap_x, cancel_y as u16)].fg, Color::Red, "destructive caps");
        assert_eq!(buf[(cap_x + 1, cancel_y as u16)].bg, Color::Red, "destructive fill");
    }

    #[test]
    fn render_pill_mode_buttons_fill_inactive_container() {
        // The 80-col sidebar drops the Yolo overlay pill, so render wide
        // enough for both mode pills and check the resting container.
        let mut c = chrome();
        c.pills = true;
        let mut terminal = Terminal::new(TestBackend::new(160, 40)).unwrap();
        terminal.draw(|f| render(f, Rect::new(0, 0, 160, 40), &[], &c)).unwrap();
        let rows = buffer_rows(&terminal);
        let y = rows.iter().position(|r| r.contains("Yolo")).expect("yolo pill paints");
        let byte_x = rows[y].find("Yolo").expect("yolo label");
        let cell_x = rows[y][..byte_x].chars().count() as u16;
        let buf = terminal.backend().buffer();
        assert_eq!(buf[(cell_x - 1, y as u16)].bg, Color::DarkGray, "inactive yolo fill");
        assert_eq!(buf[(cell_x, y as u16)].fg, Color::Black, "inactive yolo label");
    }

    #[test]
    fn sidebar_drawn_buttons_match_click_rows_without_duplicate_glyphs() {
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| render(f, area(), &[], &chrome())).unwrap();
        let rows = buffer_rows(&terminal);
        assert_eq!(rows[17].chars().skip(66).take(12).collect::<String>(), "[Off] [Yolo]");
        assert!(rows[18].chars().skip(66).take(8).collect::<String>().trim().is_empty(), "gap blank: {:?}", rows[18]);
        assert_eq!(rows[19].chars().skip(66).take(8).collect::<String>(), "[Kanban]");
        assert_eq!(rows[21].chars().skip(66).take(8).collect::<String>(), "[Tetris]");
        assert!(!rows[12].contains("[Off]"));

        let mut tall = Terminal::new(TestBackend::new(120, 40)).unwrap();
        tall.draw(|f| render(f, Rect::new(0, 0, 120, 40), &[], &chrome())).unwrap();
        let tall_rows = buffer_rows(&tall);
        assert!(tall_rows[33].contains("[Off] [Yolo]"));
        assert!(tall_rows[35].contains("[Kanban]"));
        assert!(tall_rows[37].contains("[Tetris]"));
        assert!(!tall_rows[11].contains("[Off]"));
    }

    #[test]
    fn footer_separates_mode_kanban_and_tetris_with_blank_rows() {
        // Breathing room: a blank gap row sits between the mode
        // buttons and Kanban, and another between Kanban and Tetris.
        // Hit areas track the paint, and the gap rows hit nothing.
        let info = SidebarInfo { session: None, sessions: Vec::new(), active: None, fleet_cursor: None, fleet_scroll: 0, other_timers: 0, pending: 0, mode: "off", telegram: "off", telegram_badge: None, board_open: false };
        for rich in [false, true] {
            let (w, h) = if rich { (36u16, 40u16) } else { (16u16, 23u16) };
            let sidebar = Rect::new(64, 0, w, h);
            let paint = sidebar_paint(&info, rich, sidebar.width, sidebar.height, false);
            assert_eq!(paint.kanban_row, paint.btn_row + 2, "gap above kanban (rich {rich})");
            assert_eq!(paint.tetris_row, paint.kanban_row + 2, "gap above tetris (rich {rich})");
            for gap in [paint.btn_row + 1, paint.kanban_row + 1] {
                let text: String =
                    paint.lines[gap].spans.iter().map(|s| s.content.as_ref()).collect();
                assert!(text.trim().is_empty(), "gap paints blank (rich {rich}): {text:?}");
            }
            let board = board_button_area(sidebar, &info, rich, false);
            assert_eq!(board.y, sidebar.y + 1 + paint.kanban_row as u16, "kanban hit tracks paint (rich {rich})");
            let game = tetris_button_area(sidebar, &info, rich, false);
            assert_eq!(game.y, sidebar.y + 1 + paint.tetris_row as u16, "tetris hit tracks paint (rich {rich})");
            assert!(!board_at(&board, board.x + 1, board.y - 1), "gap above kanban is dead (rich {rich})");
            assert!(!board_at(&game, game.x + 1, game.y - 1), "gap above tetris is dead (rich {rich})");
        }
    }

    #[test]
    fn kanban_button_sits_two_rows_below_mode_buttons_and_hit_tests() {
        let info = SidebarInfo { session: None, sessions: Vec::new(), active: None, fleet_cursor: None, fleet_scroll: 0, other_timers: 0, pending: 0, mode: "off", telegram: "off", telegram_badge: None, board_open: false };
        let sidebar = Rect::new(64, 0, 16, 23);
        let rich = sidebar_is_rich(sidebar);
        let paint = sidebar_paint(&info, rich, sidebar.width, sidebar.height, false);
        assert_eq!(paint.kanban_row, paint.btn_row + 2, "gap row breathes above kanban");
        let row_text: String =
            paint.lines[paint.kanban_row].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(row_text.contains("Kanban"), "kanban row paints: {row_text:?}");
        let area = board_button_area(sidebar, &info, rich, false);
        assert_eq!(area.y, sidebar.y + 1 + paint.kanban_row as u16, "hit row tracks paint");
        assert!(board_at(&area, area.x + 1, area.y), "label clicks");
        assert!(!board_at(&area, area.x + 1, area.y + 1), "row below is dead");
        assert!(!board_at(&area, area.x + area.width + 2, area.y), "past the edge is dead");
    }

    #[test]
    fn tetris_button_sits_two_rows_below_kanban_and_hit_tests() {
        let info = SidebarInfo { session: None, sessions: Vec::new(), active: None, fleet_cursor: None, fleet_scroll: 0, other_timers: 0, pending: 0, mode: "off", telegram: "off", telegram_badge: None, board_open: false };
        let sidebar = Rect::new(64, 0, 16, 23);
        let rich = sidebar_is_rich(sidebar);
        let paint = sidebar_paint(&info, rich, sidebar.width, sidebar.height, false);
        assert_eq!(paint.tetris_row, paint.kanban_row + 2, "gap row breathes above tetris");
        let row_text: String =
            paint.lines[paint.tetris_row].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(row_text.contains("Tetris"), "tetris row paints: {row_text:?}");
        let area = tetris_button_area(sidebar, &info, rich, false);
        assert_eq!(area.y, sidebar.y + 1 + paint.tetris_row as u16, "hit row tracks paint");
        assert!(board_at(&area, area.x + 1, area.y), "label clicks");
        assert!(!board_at(&area, area.x + 1, area.y + 1), "row below is dead");
        assert!(!board_at(&area, area.x + area.width + 2, area.y), "past the edge is dead");
    }

    #[test]
    fn tetris_open_paints_game_in_sidebar_keeping_footer() {
        use ratatui::{backend::TestBackend, Terminal};
        let mut c = sidebar_chrome();
        c.tetris_open = true;
        c.tetris = Some(crate::tetris::TetrisGame::new());
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal.draw(|f| render(f, f.area(), &[], &c)).unwrap();
        let buf = terminal.backend().buffer();
        let areas = chrome_areas(Rect::new(0, 0, 120, 30));
        let row_text = |y: u16| {
            (areas.sidebar.x..areas.sidebar.right())
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect::<String>()
        };
        let body: String = (0..30).map(row_text).collect::<Vec<_>>().join("\n");
        assert!(body.contains("Tetris"), "game header paints");
        assert!(body.contains("pts"), "score paints");
        assert!(body.contains("\u{2588}"), "falling piece paints");
        assert!(body.contains("Kanban"), "kanban button stays put");
        // Fleet rows leave: no session block under the game header.
        assert!(!body.contains("shell-1"), "fleet hides while playing");
    }

    #[test]
    fn kanban_button_click_cells_hit_from_painted_buffer() {
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| render(f, area(), &[], &chrome())).unwrap();
        let rows = buffer_rows(&terminal);
        let mode_y = rows.iter().position(|r| r.contains("[Off]")).expect("mode paints");
        let kanban_y = rows.iter().position(|r| r.contains("[Kanban]")).expect("kanban paints");
        assert_eq!(kanban_y, mode_y + 2, "gap row sits between autopilot buttons and kanban");
        let areas = chrome_areas(Rect::new(0, 0, 80, 24));
        let rich = sidebar_is_rich(areas.sidebar);
        let c = chrome();
        let info = SidebarInfo { session: c.detail.clone(), sessions: c.sessions.clone(), active: c.active, fleet_cursor: c.fleet_cursor, fleet_scroll: c.fleet_scroll, other_timers: c.other_timers, pending: c.pending, mode: c.mode, telegram: c.telegram, telegram_badge: c.telegram_badge.clone(), board_open: c.board_open };
        let button = board_button_area(areas.sidebar, &info, rich, c.pills);
        let byte_x = rows[kanban_y].find("[Kanban]").expect("kanban label");
        let cell_x = rows[kanban_y][..byte_x].chars().count() as u16;
        assert!(board_at(&button, cell_x + 1, kanban_y as u16), "painted label clicks");
    }

    #[test]
    fn short_sidebar_does_not_draw_controls_on_its_border() {
        let mut terminal = Terminal::new(TestBackend::new(80, 14)).unwrap();
        terminal.draw(|f| render(f, Rect::new(0, 0, 80, 14), &[], &chrome())).unwrap();
        let rows = buffer_rows(&terminal);
        // Taller sidebar earns its button rows; the bottom border stays clean.
        assert!(rows[7].contains("[Off]"), "buttons visible: {:?}", rows[7]);
        assert!(rows[9].contains("[Kanban]"), "kanban follows the gap: {:?}", rows[9]);
        assert!(rows[11].contains("[Tetris]"), "tetris follows the gap: {:?}", rows[11]);
        assert!(!rows[12].contains("[Off]"), "border clean: {:?}", rows[12]);
    }

    #[test]
    fn wide_sidebar_matches_control_plane_sections() {
        let areas = chrome_areas(Rect::new(0, 0, 180, 40));
        assert_eq!(areas.sidebar.width, 36);
        assert_eq!(areas.main.width, 144, "main keeps the clamped remainder");
        let mut c = chrome();
        c.detail = Some(SessionDetail {
            name: "jarvis_senior".into(), cli_tool: "Codex".into(),
            cwd: "/work/jarvis".into(), state: "PROGRESS".into(),
            status: None, status_kind: None, timers: Vec::new(),
        });
        let mut terminal = Terminal::new(TestBackend::new(180, 40)).unwrap();
        terminal.draw(|f| render(f, Rect::new(0, 0, 180, 40), &[], &c)).unwrap();
        let rows = buffer_rows(&terminal);
        let sidebar_text = rows.iter().map(|row| row.chars().skip(areas.sidebar.x as usize)
            .collect::<String>()).collect::<Vec<_>>().join("\n");
        assert!(sidebar_text.contains("Session Control Plane"));
        assert!(sidebar_text.contains("jarvis_senior"));
        assert!(sidebar_text.contains("Status"));
        assert!(!sidebar_text.contains("Tool Approvals"), "stats removed: {sidebar_text:?}");
        assert!(sidebar_text.contains("Global Settings"));
        assert!(sidebar_text.contains("[Off] [Yolo]"));
    }

    #[test]
    fn compact_status_and_timers_show_a_single_button_row() {
        use ratatui::{backend::TestBackend, Terminal};
        // Narrow terminal: 24-wide compact sidebar with status and a
        // timer armed, the combination that used to push the content
        // buttons past the fixed overlay row and paint both.
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal.draw(|f| render(f, f.area(), &[], &sidebar_chrome())).unwrap();
        let buf = terminal.backend().buffer();
        let areas = chrome_areas(Rect::new(0, 0, 120, 30));
        let row_text = |y: u16| {
            (areas.sidebar.x..areas.sidebar.right())
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect::<String>()
        };
        // The pinned footer owns the buttons now: Settings heads it
        // at row 21 whatever the list above holds.
        assert!(!row_text(11).contains(PILL_LEFT), "no ghost row: {:?}", row_text(11));
        assert!(row_text(21).contains("Settings"), "header visible: {:?}", row_text(21));
        let pill_rows: Vec<u16> = (0..30)
            .filter(|y| row_text(*y).contains(PILL_LEFT))
            .collect();
        // Mode pills, the Kanban pill, the Tetris pill, plus the
        // timer Cancel: four.
        assert_eq!(pill_rows.len(), 4, "buttons, kanban, tetris, cancel: {pill_rows:?}");
        assert!(mode_at(
            &footer_mode_buttons(areas.sidebar, &sidebar_chrome_info(), false, true),
            areas.sidebar.x + 3,
            pill_rows[1],
        ).is_some(), "mouse follows the visible buttons");
    }

    #[cfg(test)]
    fn sidebar_chrome_info() -> SidebarInfo {
        let c = sidebar_chrome();
        SidebarInfo { session: c.detail.clone(), sessions: c.sessions.clone(), active: c.active, fleet_cursor: c.fleet_cursor, fleet_scroll: c.fleet_scroll, other_timers: c.other_timers, pending: c.pending, mode: c.mode, telegram: "off", telegram_badge: None, board_open: false }
    }

    #[test]
    fn sidebar_content_keeps_border_padding() {
        use ratatui::{backend::TestBackend, Terminal};
        // Compact headers sit one cell inside the border: the focused
        // session owns the first content row, the fleet follows it.
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal.draw(|f| render(f, f.area(), &[], &sidebar_chrome())).unwrap();
        let buf = terminal.backend().buffer();
        assert_eq!(buf[(97, 1)].symbol(), " ", "border gap");
        assert_eq!(buf[(98, 1)].symbol(), "S", "Session header");
        // ...and so do rich ones (sidebar now opens at x=144).
        let mut wide = Terminal::new(TestBackend::new(180, 40)).unwrap();
        wide.draw(|f| render(f, f.area(), &[], &sidebar_chrome())).unwrap();
        let buf = wide.backend().buffer();
        assert_eq!(buf[(145, 1)].symbol(), " ");
        assert_eq!(buf[(146, 1)].symbol(), "F", "Forge header");
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
                status_kind: Some(crate::session::status::StatusKind::Blocked),
                timers: Vec::new(),
            }),
            sessions: Vec::new(),
            active: None,
            fleet_cursor: None,
            fleet_scroll: 0,
            other_timers: 0,
            pending: 3,
            mode: "off",
            telegram: "off",
            telegram_badge: None,
        board_open: false,
        };
        let lines = sidebar_lines(&info);
        assert!(has(&lines, "shell-1"), "name: {lines:?}");
        assert!(has(&lines, "shell"), "tool: {lines:?}");
        assert!(has(&lines, "/tmp/proj"), "cwd: {lines:?}");
        assert!(has(&lines, "blocked: waiting on review"), "status: {lines:?}");
        assert!(has(&lines, "running"), "state: {lines:?}");
        assert!(has(&lines, "Pending: 3"), "pending: {lines:?}");
        assert!(has(&lines, "[Off]"), "off highlighted: {lines:?}");
        assert!(has(&lines, "Yolo"), "yolo offered: {lines:?}");
        assert!(!has(&lines, "safe-only"), "no third mode: {lines:?}");
        // Armed timers add a Scheduled section; empty hides it.
        let mut timed = info.clone();
        timed.session.as_mut().unwrap().timers = vec![
            TimerView { id: "t1".to_string(), remaining: "9:55".to_string() },
            TimerView { id: "t2".to_string(), remaining: "1:00:05".to_string() },
        ];
        let timed_lines = sidebar_lines(&timed);
        assert!(has(&timed_lines, "Scheduled"), "section: {timed_lines:?}");
        assert!(has(&timed_lines, "◷ in 9:55"), "countdown: {timed_lines:?}");
        assert!(!has(&lines, "Scheduled"), "hidden when empty: {lines:?}");
        // Yolo highlights instead when active.
        let yolo = SidebarInfo { session: info.session.clone(), sessions: Vec::new(), active: None, fleet_cursor: None, fleet_scroll: 0, other_timers: 0, pending: 0, mode: "yolo", telegram: "off", telegram_badge: None, board_open: false };
        let yolo_lines = sidebar_lines(&yolo);
        assert!(has(&yolo_lines, "[Yolo]"), "yolo highlighted: {yolo_lines:?}");
        // Never blank: empty state still guides.
        let empty = sidebar_lines(&SidebarInfo { session: None, sessions: Vec::new(), active: None, fleet_cursor: None, fleet_scroll: 0, other_timers: 0, pending: 0, mode: "off", telegram: "off", telegram_badge: None, board_open: false });
        assert!(has(&empty, "Ctrl-b c"), "guides: {empty:?}");
    }

    #[test]
    fn sidebar_has_no_stats_section() {
        // Uptime, tool calls, and approval counts were dropped: the
        // sidebar shows status and schedule, never Stats.
        let info = SidebarInfo {
            session: Some(SessionDetail {
                name: "shell-1".to_string(),
                cli_tool: "shell".to_string(),
                cwd: "/tmp/proj".to_string(),
                state: "running".to_string(),
                status: Some("progress: compiling".to_string()),
                status_kind: Some(crate::session::status::StatusKind::Progress),
                timers: vec![TimerView { id: "t1".to_string(), remaining: "9:55".to_string() }],
            }),
            sessions: Vec::new(),
            active: None,
            fleet_cursor: None,
            fleet_scroll: 0,
            other_timers: 0,
            pending: 0,
            mode: "off",
            telegram: "off",
            telegram_badge: None,
        board_open: false,
        };
        let lines = sidebar_lines(&info);
        for needle in ["Stats", "Uptime", "Tool calls", "✓", "×"] {
            assert!(!has(&lines, needle), "compact leaks {needle}: {lines:?}");
        }
        let rich = rich_sidebar_lines(&info, 30, 45);
        for needle in ["Stats", "Uptime", "Tool calls", "✓", "×"] {
            assert!(!has(&rich, needle), "rich leaks {needle}: {rich:?}");
        }
        // Status and schedule survive the removal.
        assert!(has(&lines, "progress: compiling"), "status kept: {lines:?}");
        assert!(has(&lines, "Scheduled"), "schedule kept: {lines:?}");
    }

    #[test]
    fn sidebar_badge_escapes_hostile_text() {
        let info = SidebarInfo {
            session: None,
            sessions: Vec::new(),
            active: None,
            fleet_cursor: None,
            fleet_scroll: 0,
            other_timers: 0,
            pending: 0,
            mode: "off",
            telegram: "off",
            telegram_badge: Some(("agent".to_string(), "hi\u{202E}bye".to_string())), board_open: false,
        };
        let lines = sidebar_lines(&info);
        let text: String = lines.iter().flat_map(|l| l.spans.iter().map(|s| s.content.as_ref())).collect();
        assert!(!text.contains("\u{202E}"), "bidi exposed, never raw: {text:?}");
        assert!(text.contains("agent"), "session named: {text:?}");
        let bare = SidebarInfo { session: None, sessions: Vec::new(), active: None, fleet_cursor: None, fleet_scroll: 0, other_timers: 0, pending: 0, mode: "off", telegram: "off", telegram_badge: None, board_open: false };
        // The paint always fills the panel; the badge grows the pinned
        // footer by exactly one row instead.
        assert_eq!(
            sidebar_footer_height(&bare, false) + 1,
            sidebar_footer_height(&info, false),
            "badge grows the footer"
        );
    }

    #[test]
    fn sidebar_shows_telegram_state() {
        let off = sidebar_lines(&SidebarInfo { session: None, sessions: Vec::new(), active: None, fleet_cursor: None, fleet_scroll: 0, other_timers: 0, pending: 0, mode: "off", telegram: "off", telegram_badge: None, board_open: false });
        assert!(has(&off, "Telegram off"), "off state: {off:?}");
        assert!(has(&off, "Ctrl-b m"), "settings key: {off:?}");
        let on = sidebar_lines(&SidebarInfo { session: None, sessions: Vec::new(), active: None, fleet_cursor: None, fleet_scroll: 0, other_timers: 0, pending: 0, mode: "off", telegram: "on", telegram_badge: None, board_open: false });
        assert!(has(&on, "Telegram on"), "on state: {on:?}");
    }

    #[test]
    fn sidebar_width_clamps_around_thirty_six() {
        // Wide terminals stop donating a full percentage to the panel:
        // 159→160 must not steal seven columns from the main pane.
        for (cols, want) in [
            (80u16, 16u16),
            (120, 24),
            (159, 32),
            (160, 36),
            (180, 36),
        ] {
            let areas = chrome_areas(Rect::new(0, 0, cols, 40));
            assert_eq!(areas.sidebar.width, want, "cols {cols}");
            assert_eq!(
                areas.main.width + areas.sidebar.width,
                cols,
                "no gap: cols {cols}"
            );
        }
    }


    #[test]
    fn sidebar_layout_pins_a_footer() {
        let bar = Rect::new(144, 0, 36, 39);
        let layout = sidebar_layout(bar, 7);
        assert_eq!(layout.footer.height, 7);
        assert_eq!(layout.footer.y + layout.footer.height, bar.y + bar.height);
        assert_eq!(layout.list.y, bar.y);
        assert_eq!(
            layout.list.height + layout.footer.height,
            bar.height,
            "list plus footer fill the panel"
        );
    }

    #[test]
    fn sidebar_content_pins_footer_to_the_bottom() {
        // Tall content must clip the list, never the footer: the last
        // footer rows stay visible at any height.
        let mut info = SidebarInfo {
            session: None,
            sessions: Vec::new(),
            active: None,
            fleet_cursor: None,
            fleet_scroll: 0,
            other_timers: 0,
            pending: 0,
            mode: "off",
            telegram: "on",
            telegram_badge: Some(("agent".to_string(), "hi".to_string())), board_open: false,
        };
        for_h_fleet(&mut info);
        let lines = rich_sidebar_lines(&info, 45, 20);
        assert_eq!(lines.len(), 20, "content fills exactly");
        let text: Vec<String> = lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        assert!(
            text[19].contains("agent: hi"),
            "badge pinned last: {text:?}"
        );
        assert!(text[18].contains("Telegram on"), "footer intact: {text:?}");
    }

    #[test]
    fn status_emoji_has_no_joiners_or_selectors() {
        use crate::session::status::StatusKind;
        // Bare glyphs only: no VS16, no ZWJ, like the topbar icons.
        for kind in [
            StatusKind::Info,
            StatusKind::Progress,
            StatusKind::Success,
            StatusKind::Warning,
            StatusKind::Blocked,
            StatusKind::Question,
        ] {
            let e = status_emoji(kind);
            assert!(!e.contains('\u{FE0F}'), "no VS16: {e:?}");
            assert!(!e.contains('\u{200D}'), "no ZWJ: {e:?}");
            assert!(Line::from(e).width() >= 1, "paints something: {e:?}");
        }
        assert_eq!(status_emoji(StatusKind::Blocked), "🛑");
    }

    #[test]
    fn rule_line_spans_sidebar_width() {
        // One indent cell plus a width-6 span of dashes.
        for width in [30u16, 45, 60] {
            let rule = rule_line(width);
            let rule_text: String = rule.spans.iter().map(|s| s.content.as_ref()).collect();
            assert_eq!(rule_text.chars().count(), (width - 5) as usize, "rule {width}");
            assert!(rule_text.starts_with(' '), "indent");
        }
    }

    #[test]
    fn format_countdown_matches_reference_shapes() {
        use std::time::Duration;
        assert_eq!(format_countdown(Duration::from_secs(595)), "9:55");
        assert_eq!(format_countdown(Duration::from_secs(3605)), "1:00:05");
        assert_eq!(format_countdown(Duration::from_secs(7)), "0:07");
        assert_eq!(format_countdown(Duration::ZERO), "0:00");
    }

    #[test]
    fn scheduled_section_keeps_breathing_room() {
        let info = SidebarInfo { session: Some(timed_detail()), sessions: Vec::new(), active: None, fleet_cursor: None, fleet_scroll: 0, other_timers: 0, pending: 0, mode: "off", telegram: "off", telegram_badge: None, board_open: false };
        let lines = rich_sidebar_lines(&info, 30, 45);
        let header = lines
            .iter()
            .position(|l| l.spans.len() == 1 && l.spans[0].content.trim() == "Scheduled")
            .expect("section renders");
        let blank = lines[header - 1].spans.iter().all(|s| s.content.is_empty());
        assert!(blank, "breathing room above Scheduled");
    }

    #[test]
    fn scheduled_timers_render_countdowns_and_cancel_buttons() {
        let mut c = chrome();
        c.detail = Some(timed_detail());
        let mut terminal = Terminal::new(TestBackend::new(180, 40)).unwrap();
        terminal.draw(|f| render(f, Rect::new(0, 0, 180, 40), &[], &c)).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("Scheduled"), "section: {text:?}");
        assert!(text.contains("9:55"), "countdown: {text:?}");
        assert!(text.contains("[Cancel]"), "button: {text:?}");
    }

    #[test]
    fn timer_cancel_rects_match_painted_rows() {
        let info = SidebarInfo { session: Some(timed_detail()), sessions: Vec::new(), active: None, fleet_cursor: None, fleet_scroll: 0, other_timers: 0, pending: 0, mode: "off", telegram: "off", telegram_badge: None, board_open: false };
        let areas = chrome_areas(Rect::new(0, 0, 180, 40));
        let rects = timer_cancel_rects(areas.sidebar, &info, false);
        assert_eq!(rects.len(), 2);
        assert_eq!((rects[0].0.as_str(), rects[1].0.as_str()), ("t1", "t2"), "soonest first");
        assert_eq!(rects[1].1.y, rects[0].1.y + 1, "stacked rows");
        for (_, area) in &rects {
            assert_eq!(area.width, 8, "Cancel width");
            assert_eq!(
                area.right(),
                areas.sidebar.right() - 3,
                "button ends at the divider edge: {area:?}"
            );
        }
        // The painted button labels sit exactly on the rects.
        let mut terminal = Terminal::new(TestBackend::new(180, 40)).unwrap();
        let mut c = chrome();
        c.detail = Some(timed_detail());
        terminal.draw(|f| render(f, Rect::new(0, 0, 180, 40), &[], &c)).unwrap();
        let buf = terminal.backend().buffer();
        for (_, area) in &rects {
            assert_eq!(buf[(area.x, area.y)].symbol(), "[", "button at {area:?}");
        }
        // A session literally named Scheduled cannot hijack the rows.
        let mut impostor = timed_detail();
        impostor.name = "Scheduled".to_string();
        impostor.timers.clear();
        let bare = SidebarInfo { session: Some(impostor), sessions: Vec::new(), active: None, fleet_cursor: None, fleet_scroll: 0, other_timers: 0, pending: 0, mode: "off", telegram: "off", telegram_badge: None, board_open: false };
        assert!(timer_cancel_rects(areas.sidebar, &bare, false).is_empty());
    }

}
