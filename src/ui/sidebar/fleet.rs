//! UI sidebar fleet: rows, windowing, attention paint.

use ratatui::style::Modifier;
use ratatui::text::{Line, Span};

use crate::infra::safe_text;

use super::focused_block_lines;
use super::{FleetTier, SidebarInfo};
use super::super::text::{cells, cut_cells, wrap_cells};
use super::super::theme;

/// Whole blocks that fit the cell budget starting at `scroll`:
/// scroll saturates at the tail, and the first block always shows even
/// when it alone overflows the budget.
pub fn fleet_cell_window(scroll: usize, heights: &[u16], budget: u16) -> (usize, usize) {
    if heights.is_empty() {
        return (0, 0);
    }
    let start = scroll.min(heights.len() - 1);
    let mut used = 0u16;
    let mut end = start;
    while end < heights.len() {
        let h = heights[end].max(1);
        if end > start && used + h > budget {
            break;
        }
        used += h;
        end += 1;
    }
    (start, end)
}

/// Painted height per fleet block in row order: name, wrapped reason,
/// breathing room. Render, hit-testing, and cursor-following share
/// this, so variable heights can never desync them.
pub fn fleet_block_heights(info: &SidebarInfo, width: u16) -> Vec<u16> {
    info.sessions
        .iter()
        .map(|row| {
            1 + wrap_cells(
                &safe_text::encode_for_display(&row.reason),
                fleet_wrap_width(width),
            )
            .len() as u16
                + 1
        })
        .collect()
}

/// One rendered fleet item: a zone header or a session block by row
/// index. Headers travel with their blocks through the same budgeted
/// window, so a header can never strand without its rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FleetItem {
    AttnHeader,
    SessionsHeader,
    Block(usize),
}

/// Grouped window over the fleet: attention blocks under their header,
/// the rest under theirs, whole items while they fit the cell budget.
/// A trailing stranded header is dropped; an empty window means the
/// list region is too small for even one header.
pub fn fleet_window_items(
    info: &SidebarInfo,
    width: u16,
    items_budget: u16,
) -> Vec<FleetItem> {
    let n = info.sessions.len();
    if n == 0 {
        return Vec::new();
    }
    let start = info.fleet_scroll.min(n - 1);
    let heights = fleet_block_heights(info, width);
    let mut items = Vec::new();
    let attn: Vec<usize> = (0..n)
        .filter(|&i| i >= start && info.sessions[i].tier == FleetTier::Attention)
        .collect();
    let rest: Vec<usize> = (0..n)
        .filter(|&i| i >= start && info.sessions[i].tier != FleetTier::Attention)
        .collect();
    if !attn.is_empty() {
        items.push(FleetItem::AttnHeader);
        items.extend(attn.into_iter().map(FleetItem::Block));
    }
    if !rest.is_empty() {
        items.push(FleetItem::SessionsHeader);
        items.extend(rest.into_iter().map(FleetItem::Block));
    }
    let height_of = |item: &FleetItem| -> u16 {
        match item {
            FleetItem::AttnHeader | FleetItem::SessionsHeader => 1,
            FleetItem::Block(i) => heights[*i].max(1),
        }
    };
    let mut used = 0u16;
    let mut end = 0;
    for (k, item) in items.iter().enumerate() {
        if k > 0 && used + height_of(item) > items_budget {
            break;
        }
        used += height_of(item);
        end = k + 1;
    }
    while end > 0 && matches!(items[end - 1], FleetItem::AttnHeader | FleetItem::SessionsHeader) {
        end -= 1;
    }
    items.truncate(end);
    items
}

/// Fleet items budget inside the list region: list height minus the
/// brand, focused block, gap row, and title row above the items.
pub fn fleet_items_budget(info: &SidebarInfo, rich: bool, width: u16, list_h: u16) -> u16 {
    let top = (if rich { 3 } else { 0 }) + focused_block_lines(info, rich, width).len() + 1;
    list_h.saturating_sub(top as u16 + 1)
}

/// Reason wrap width: two indent cells plus a margin off the edge.
fn fleet_wrap_width(width: u16) -> usize {
    (width as usize).saturating_sub(4).max(8)
}

/// Name budget on the block headline: selector, mark, emoji, gaps.
fn fleet_name_budget(width: u16, emoji: &str) -> usize {
    (width as usize).saturating_sub(6 + cells(emoji)).max(4)
}

/// Fleet slot lines: header plus the visible window, one airy block
/// per session — name headline, wrapped reason, breathing room.
/// `>` marks the cursor, `*` the focused session, otherwise the tier
/// mark; emoji ride beside the marks, never alone.
/// Fleet slot lines from a grouped window: zoned title, attention
/// zone with a Danger accent, then the sessions zone. `>` marks the
/// cursor, `*` the focused session, otherwise the tier mark; emoji
/// ride beside the marks, never alone. The cursor row takes full-row
/// Focus reverse; idle rows sink to Muted; the active name is Brand.
pub(super) fn fleet_block_lines(info: &SidebarInfo, width: u16, items: &[FleetItem]) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let total = info.sessions.len();
    if total == 0 {
        lines.push(Line::from(Span::styled(
            " sessions · 0",
            theme::style(theme::Role::Brand),
        )));
        lines.push(Line::from("  none yet"));
        lines.push(Line::from("  Ctrl-b c creates one"));
        return lines;
    }
    let shown: Vec<usize> = items
        .iter()
        .filter_map(|it| match it {
            FleetItem::Block(i) => Some(*i),
            FleetItem::AttnHeader | FleetItem::SessionsHeader => None,
        })
        .collect();
    // Window position over the sorted fleet: min–max, since grouping
    // can interleave the shown blocks out of index order.
    let title = match (shown.iter().min(), shown.iter().max()) {
        (Some(first), Some(last)) if shown.len() < total => {
            format!(" Fleet ({total}) · {}–{}", first + 1, last + 1)
        }
        _ => format!(" Fleet ({total})"),
    };
    lines.push(Line::from(Span::styled(title, theme::style(theme::Role::Brand))));
    for item in items {
        match item {
            FleetItem::AttnHeader => {
                let k = items
                    .iter()
                    .filter(|it| matches!(it, FleetItem::Block(i) if info.sessions[*i].tier == FleetTier::Attention))
                    .count();
                lines.push(Line::from(Span::styled(
                    format!(" NEEDS ATTENTION {k}"),
                    theme::style(theme::Role::Brand),
                )));
            }
            FleetItem::SessionsHeader => {
                lines.push(Line::from(Span::styled(
                    " SESSIONS".to_string(),
                    theme::style(theme::Role::Brand),
                )));
            }
            FleetItem::Block(i) => {
                let row = &info.sessions[*i];
                let cursor = info.fleet_cursor == Some(row.id);
                let sel = if cursor {
                    ">"
                } else if info.active == Some(row.id) {
                    "*"
                } else {
                    " "
                };
                let raw_name = safe_text::encode_for_display(&row.name);
                let name = cut_cells(&raw_name, fleet_name_budget(width, row.emoji));
                let headline = format!("{}{}{} {}", sel, row.tier.mark(), row.emoji, name);
                if cursor {
                    let style = theme::style(theme::Role::Focus)
                        .add_modifier(Modifier::REVERSED);
                    lines.push(Line::from(vec![
                        Span::styled(format!(" {headline}"), style),
                    ]));
                    for wrapped in wrap_cells(
                        &safe_text::encode_for_display(&row.reason),
                        fleet_wrap_width(width),
                    ) {
                        lines.push(Line::from(vec![
                            Span::styled(format!("  {wrapped}"), style),
                        ]));
                    }
                } else {
                    let name_style = if info.active == Some(row.id) {
                        theme::style(theme::Role::Brand)
                    } else if row.tier == FleetTier::Idle {
                        theme::style(theme::Role::Muted)
                    } else {
                        theme::style(theme::Role::Text)
                    };
                    if row.tier == FleetTier::Attention {
                        lines.push(Line::from(vec![
                            Span::styled("┃", theme::style(theme::Role::Danger)),
                            Span::styled(headline, name_style),
                        ]));
                        for wrapped in wrap_cells(
                            &safe_text::encode_for_display(&row.reason),
                            fleet_wrap_width(width),
                        ) {
                            lines.push(Line::from(vec![
                                Span::styled("┃", theme::style(theme::Role::Danger)),
                                Span::styled(format!(" {wrapped}"), name_style),
                            ]));
                        }
                    } else {
                        lines.push(Line::from(vec![
                            Span::raw(" "),
                            Span::styled(headline, name_style),
                        ]));
                        for wrapped in wrap_cells(
                            &safe_text::encode_for_display(&row.reason),
                            fleet_wrap_width(width),
                        ) {
                            lines.push(Line::from(vec![
                                Span::styled(format!("  {wrapped}"), name_style),
                            ]));
                        }
                    }
                }
                lines.push(Line::from(""));
            }
        }
    }
    lines
}


#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;
    use ratatui::text::Text;
    use ratatui::widgets::{Block, Borders, Paragraph};
    use super::super::{FleetRow, SessionDetail};
    use crate::ui::sidebar::{rich_sidebar_lines, sidebar_lines, sidebar_lines_at, sidebar_paint};
    use crate::ui::test_support::{for_h_fleet, has, text_of, timed_detail};


    #[test]
    fn fleet_cell_window_budgets_variable_blocks() {
        // Whole blocks that fit the cell budget; the first always shows.
        assert_eq!(fleet_cell_window(0, &[3, 3, 5, 3], 6), (0, 2));
        assert_eq!(fleet_cell_window(2, &[3, 3, 5, 3], 6), (2, 3));
        assert_eq!(fleet_cell_window(999, &[3, 3], 100), (1, 2));
        assert_eq!(fleet_cell_window(0, &[], 10), (0, 0));
        assert_eq!(fleet_cell_window(0, &[9], 2), (0, 1));
    }

    #[test]
    fn fleet_rows_and_status_carry_emoji_beside_marks() {
        use crate::session::status::StatusKind;
        let id = crate::session::SessionId::fresh();
        let info = SidebarInfo {
            session: Some(SessionDetail {
                name: "muse".to_string(),
                cli_tool: "muse".to_string(),
                cwd: "/tmp/proj".to_string(),
                state: "running · Waiting".to_string(),
                status: Some("blocked: need the API key".to_string()),
                status_kind: Some(StatusKind::Blocked),
                timers: Vec::new(),
            }),
            sessions: vec![FleetRow {
                id,
                name: "muse".to_string(),
                tier: FleetTier::Attention,
                state: "running · Waiting".to_string(),
                reason: "need the API key".to_string(),
                emoji: "🔔",
            }],
            active: Some(id),
            fleet_cursor: None,
            fleet_scroll: 0,
            other_timers: 0,
            pending: 0,
            mode: "off",
            telegram: "off",
            telegram_badge: None,
        board_open: false,
        };
        // Marks still carry meaning; emoji ride beside them.
        let compact = sidebar_lines(&info);
        assert!(has(&compact, "!"), "tier mark: {compact:?}");
        assert!(has(&compact, "🔔"), "attention emoji: {compact:?}");
        assert!(has(&compact, "🛑"), "status emoji: {compact:?}");
        let rich = rich_sidebar_lines(&info, 30, 45);
        assert!(has(&rich, "*"), "active mark: {rich:?}");
        assert!(has(&rich, "need the API key"), "reason: {rich:?}");
    }

    #[test]
    fn session_block_precedes_fleet() {
        let id = crate::session::SessionId::fresh();
        let other = crate::session::SessionId::fresh();
        let info = SidebarInfo {
            session: Some(SessionDetail {
                name: "muse".to_string(),
                cli_tool: "muse".to_string(),
                cwd: "/tmp/proj".to_string(),
                state: "running".to_string(),
                status: None,
                status_kind: None,
                timers: Vec::new(),
            }),
            sessions: vec![
                FleetRow { id, name: "muse".to_string(), tier: FleetTier::Idle, state: "running".to_string(), reason: "running".to_string(), emoji: "" },
                FleetRow { id: other, name: "codex".to_string(), tier: FleetTier::Idle, state: "running".to_string(), reason: "running".to_string(), emoji: "" },
            ],
            active: Some(id),
            fleet_cursor: None,
            fleet_scroll: 0,
            other_timers: 0,
            pending: 0,
            mode: "off",
            telegram: "off",
            telegram_badge: None,
        board_open: false,
        };
        let text_of = |l: &Line| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>();
        let compact = sidebar_lines(&info);
        let session_at = compact.iter().position(|l| text_of(l).trim() == "Session").expect("session header");
        let fleet_at = compact.iter().position(|l| text_of(l).starts_with(" Fleet (")).expect("fleet header");
        assert!(session_at < fleet_at, "session above fleet");
        let rich = rich_sidebar_lines(&info, 30, 45);
        let session_at = rich.iter().position(|l| text_of(l).trim() == "Session").expect("session header");
        let fleet_at = rich.iter().position(|l| text_of(l).starts_with(" Fleet (")).expect("fleet header");
        assert!(session_at < fleet_at, "session above fleet");
    }

    #[test]
    fn fleet_blocks_breathe_with_gaps_and_wrapped_reasons() {
        let id = crate::session::SessionId::fresh();
        let other = crate::session::SessionId::fresh();
        let long = "need the production API key from the vault under the stairs";
        let info = SidebarInfo {
            session: None,
            sessions: vec![
                FleetRow { id, name: "muse".to_string(), tier: FleetTier::Attention, state: "running · Waiting".to_string(), reason: long.to_string(), emoji: "🔔" },
                FleetRow { id: other, name: "codex".to_string(), tier: FleetTier::Idle, state: "running".to_string(), reason: "running".to_string(), emoji: "" },
            ],
            active: Some(id),
            fleet_cursor: None,
            fleet_scroll: 0,
            other_timers: 0,
            pending: 0,
            mode: "off",
            telegram: "off",
            telegram_badge: None,
        board_open: false,
        };
        // Width 24: the long reason must wrap, never clip mid-word off-panel.
        let lines = sidebar_lines_at(&info, false, 24, 60).0;
        let text: Vec<String> = lines.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect()).collect();
        let name_at = text.iter().position(|l| l.contains("muse")).expect("name line");
        assert!(text[name_at].contains("!"), "tier mark on the name line");
        // Reason lines run until the gap; the whole reason survives.
        let mut end = name_at + 1;
        assert!(text[end].contains("need the production"), "reason starts its own line");
        while !text[end].trim().is_empty() {
            end += 1;
        }
        let joined = text[name_at + 1..end].join(" ");
        assert!(joined.contains("API key"), "reason wraps instead of clipping: {joined:?}");
        assert!(joined.contains("stairs"), "nothing lost: {joined:?}");
        // Zoned fleet: the gap yields to the sessions zone header, and
        // the next block follows inside its zone.
        assert!(text[end + 1].contains("SESSIONS"), "zone follows the gap");
        assert!(text[end + 2].contains("codex"), "next block inside its zone");
        for (i, l) in lines.iter().enumerate() {
            assert!(l.width() <= 24, "line {i} fits: {:?}", text[i]);
        }
    }

    #[test]
    fn pending_zero_hides_its_row() {
        let mut info = SidebarInfo {
            session: None,
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
        for line in sidebar_lines(&info) {
            assert!(!text_of(&line).contains("Pending"), "zero hides: {line:?}");
        }
        for line in rich_sidebar_lines(&info, 45, 30) {
            assert!(!text_of(&line).contains("Pending"), "zero hides: {line:?}");
        }
        info.pending = 2;
        assert!(has(&sidebar_lines(&info), "Pending: 2"));
        info.session = Some(timed_detail());
        assert!(has(&rich_sidebar_lines(&info, 45, 30), "Pending hooks: 2"));
    }

    #[test]
    fn fleet_header_shows_window_position() {
        let mut info = SidebarInfo {
            session: None,
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
        for_h_fleet(&mut info);
        let clipped = sidebar_paint(&info, false, 24, 20, false).lines;
        let header = clipped
            .iter()
            .map(text_of)
            .find(|l| l.starts_with(" Fleet ("))
            .expect("fleet header");
        assert!(header.contains("· 1–2"), "partial window shows position: {header:?}");
        let full = sidebar_paint(&info, false, 24, 60, false).lines;
        let header = full
            .iter()
            .map(text_of)
            .find(|l| l.starts_with(" Fleet ("))
            .expect("fleet header");
        assert_eq!(header, " Fleet (6)", "full window stays clean: {header:?}");
    }

    #[test]
    fn fleet_window_items_group_attention_first() {
        let a = crate::session::SessionId::fresh();
        let b = crate::session::SessionId::fresh();
        let c = crate::session::SessionId::fresh();
        let info = SidebarInfo {
            session: None,
            sessions: vec![
                FleetRow { id: a, name: "aaa".into(), tier: FleetTier::Idle, state: "running".into(), reason: "running".into(), emoji: "" },
                FleetRow { id: b, name: "bbb".into(), tier: FleetTier::Attention, state: "running · Waiting".into(), reason: "need a key".into(), emoji: "🔔" },
                FleetRow { id: c, name: "ccc".into(), tier: FleetTier::Idle, state: "running".into(), reason: "running".into(), emoji: "" },
            ],
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
        // Room for everything: attention group leads, spawn order inside.
        let full = fleet_window_items(&info, 24, 100);
        assert_eq!(
            full,
            vec![
                FleetItem::AttnHeader,
                FleetItem::Block(1),
                FleetItem::SessionsHeader,
                FleetItem::Block(0),
                FleetItem::Block(2),
            ],
            "grouped: {full:?}"
        );
        // Tight budget: the sessions group drops whole, never stranded.
        let tight = fleet_window_items(&info, 24, 5);
        assert_eq!(
            tight,
            vec![FleetItem::AttnHeader, FleetItem::Block(1)],
            "budgeted: {tight:?}"
        );
        // Starved budget strands no lonely header.
        let starved = fleet_window_items(&info, 24, 1);
        assert!(starved.is_empty(), "nothing rather than a stray header: {starved:?}");
    }

    #[test]
    fn attention_zone_carries_danger_accent() {
        use ratatui::{backend::TestBackend, Terminal};
        let id = crate::session::SessionId::fresh();
        let info = SidebarInfo {
            session: None,
            sessions: vec![
                FleetRow { id, name: "muse".into(), tier: FleetTier::Attention, state: "running · Waiting".into(), reason: "need a key".into(), emoji: "🔔" },
            ],
            active: Some(id),
            fleet_cursor: None,
            fleet_scroll: 0,
            other_timers: 0,
            pending: 0,
            mode: "off",
            telegram: "off",
            telegram_badge: None,
        board_open: false,
        };
        let lines = sidebar_paint(&info, false, 24, 30, false).lines;
        let text: Vec<String> = lines.iter().map(text_of).collect();
        assert!(text.iter().any(|l| l.contains("┃")), "accent paints: {text:?}");
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|f| {
                let paint = sidebar_paint(&info, false, 16, 23, false);
                let side = Paragraph::new(Text::from(paint.lines)).block(
                    Block::default().borders(Borders::ALL),
                );
                f.render_widget(side, f.area());
            })
            .unwrap();
        let buf = terminal.backend().buffer();
        let accent = buf
            .content
            .iter()
            .find(|c| c.symbol() == "┃")
            .expect("accent cell paints");
        assert_eq!(accent.fg, Color::Red, "danger accent");
    }

    #[test]
    fn cursor_row_takes_focus_reverse() {
        use ratatui::{backend::TestBackend, Terminal};
        let id = crate::session::SessionId::fresh();
        let other = crate::session::SessionId::fresh();
        let info = SidebarInfo {
            session: None,
            sessions: vec![
                FleetRow { id, name: "muse".into(), tier: FleetTier::Idle, state: "running".into(), reason: "running".into(), emoji: "" },
                FleetRow { id: other, name: "codex".into(), tier: FleetTier::Idle, state: "running".into(), reason: "running".into(), emoji: "" },
            ],
            active: None,
            fleet_cursor: Some(other),
            fleet_scroll: 0,
            other_timers: 0,
            pending: 0,
            mode: "off",
            telegram: "off",
            telegram_badge: None,
        board_open: false,
        };
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|f| {
                let paint = sidebar_paint(&info, false, 16, 23, false);
                let side = Paragraph::new(Text::from(paint.lines)).block(
                    Block::default().borders(Borders::ALL),
                );
                f.render_widget(side, f.area());
            })
            .unwrap();
        let buf = terminal.backend().buffer();
        // Drive off the real paint: scan for the block name rows,
        // never hand-computed geometry.
        let row_text = |y: u16| {
            (0..80)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect::<String>()
        };
        let mut cursor_y = None;
        let mut idle_y = None;
        for y in 0..24 {
            let row = row_text(y);
            if row.contains("codex") {
                cursor_y = Some(y);
            }
            if row.contains("muse") {
                idle_y = Some(y);
            }
        }
        let cy = cursor_y.expect("cursor block paints");
        let iy = idle_y.expect("idle block paints");
        assert!(
            buf[(2, cy)].modifier.contains(Modifier::REVERSED),
            "cursor reverses"
        );
        assert!(
            !buf[(2, iy)].modifier.contains(Modifier::REVERSED),
            "neighbors stay flat"
        );
    }

}
