//! Which-key hotkey help: pure data, layout, timing, and render.
//!
//! The command table mirrors the `Ctrl-b` prefix map in [`crate::input`];
//! dispatch authority stays there. This module only describes those
//! bindings for display, decides when the HUD appears, and paints it.

use std::time::{Duration, Instant};

use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

/// Delay between the prefix key and the HUD appearing. Fast typists
/// finish their sequence inside this window and never see it.
pub const WHICHKEY_DELAY_MS: u64 = 250;

/// One displayed binding: the key as typed after `Ctrl-b`, a short
/// human label, and its section in the HUD.
#[derive(Clone, Copy, Debug)]
pub struct WhichEntry {
    pub key: &'static str,
    pub desc: &'static str,
    pub group: &'static str,
}

/// Display rows for the HUD, built from the canonical binding table
/// in [`crate::input::PREFIX_BINDINGS`]: adding a shortcut there
/// lists it here with no second edit. Order is the display order:
/// entries stay grouped, groups in first-seen order.
pub fn entries() -> Vec<WhichEntry> {
    crate::input::PREFIX_BINDINGS
        .iter()
        .map(|b| WhichEntry { key: b.label, desc: b.desc, group: b.group })
        .collect()
}

/// HUD visibility plus the pending-prefix clock. Owned by `AppState`;
/// the TUI loop feeds it prefix transitions and ticks it every frame.
pub struct WhichKeyHud {
    visible: bool,
    pinned: bool,
    pending_since: Option<Instant>,
}

impl WhichKeyHud {
    pub fn new() -> Self {
        WhichKeyHud { visible: false, pinned: false, pending_since: None }
    }

    pub fn visible(&self) -> bool {
        self.visible
    }

    pub fn pinned(&self) -> bool {
        self.pinned
    }

    /// Explicit `Ctrl-b ?`: open at once and stay open for browsing.
    pub fn show_pinned(&mut self) {
        self.visible = true;
        self.pinned = true;
        self.pending_since = None;
    }

    /// Close from any state: commands, Esc, invalid keys.
    pub fn hide(&mut self) {
        self.visible = false;
        self.pinned = false;
        self.pending_since = None;
    }

    /// The prefix key landed; the clock starts (never while pinned —
    /// the HUD is already up).
    pub fn note_pending(&mut self, now: Instant) {
        if !self.pinned {
            self.pending_since = Some(now);
        }
    }

    /// The sequence resolved fast (or was cancelled): no overlay.
    pub fn note_resolved(&mut self) {
        self.pending_since = None;
        if !self.pinned {
            self.visible = false;
        }
    }

    /// One frame tick: show once the delay elapses with the prefix
    /// still pending; hide the moment it is gone. Pinned HUDs ignore
    /// the clock entirely. Returns true when visibility changed, so
    /// the loop knows a repaint is due.
    pub fn poll(&mut self, prefix_pending: bool, now: Instant) -> bool {
        if self.pinned {
            return false;
        }
        if prefix_pending {
            if !self.visible {
                if let Some(since) = self.pending_since {
                    if now.duration_since(since) >= Duration::from_millis(WHICHKEY_DELAY_MS) {
                        self.visible = true;
                        return true;
                    }
                }
            }
            false
        } else {
            self.pending_since = None;
            let was = self.visible;
            self.visible = false;
            was
        }
    }
}

impl Default for WhichKeyHud {
    fn default() -> Self {
        Self::new()
    }
}

/// Cells of side padding inside the bordered panel.
const SIDE_PAD: u16 = 2;
/// Gap between entry columns.
const COL_GAP: u16 = 2;
/// Spaces between a key and its description.
const KEY_GAP: &str = "  ";

/// One group's share of the panel: column count, row count, and the
/// padded cell width every column takes.
pub struct GroupLayout {
    pub name: &'static str,
    pub cols: usize,
    pub rows: usize,
    cell_w: u16,
}

/// The whole panel: per-group columns plus outer size in cells,
/// borders included.
pub struct HudLayout {
    pub groups: Vec<GroupLayout>,
    pub width: u16,
    pub height: u16,
}

fn display_width(text: &str) -> u16 {
    text.chars().count().min(u16::MAX as usize) as u16
}

/// Widest the panel ever grows: past this it stops being a scannable
/// HUD and starts wallpapering the terminal. Columns derive from the
/// capped width, so wide terminals reuse the compact two-column plan.
const PANEL_MAX_W: u16 = 72;

/// Column plan for `term_w`: groups flow into as many columns as fit,
/// so wider terminals grow sideways instead of down — up to the panel
/// cap. Narrow terminals stack the big groups and keep two columns
/// only where a pair still fits, so nothing overflows horizontally.
pub fn layout(term_w: u16) -> HudLayout {
    let panel_cap = term_w.saturating_sub(4).max(1).min(PANEL_MAX_W);
    let inner_cap = panel_cap.saturating_sub(2 + 2 * SIDE_PAD).max(1);
    let all = entries();
    let mut groups = Vec::new();
    let mut order: Vec<&'static str> = Vec::new();
    for entry in &all {
        if !order.contains(&entry.group) {
            order.push(entry.group);
        }
    }
    for name in order {
        let items: Vec<&WhichEntry> =
            all.iter().filter(|e| e.group == name).collect();
        let key_w = items.iter().map(|e| display_width(e.key)).max().unwrap_or(0);
        let desc_w = items.iter().map(|e| display_width(e.desc)).max().unwrap_or(0);
        let cell_w = key_w + display_width(KEY_GAP) + desc_w;
        let cols = ((inner_cap + COL_GAP) / (cell_w + COL_GAP).max(1)).max(1) as usize;
        // Two columns stay scannable at any width: wider groups wrap
        // to more rows instead of sprawling sideways.
        let cols = cols.min(items.len()).max(1).min(2);
        let rows = items.len().div_ceil(cols);
        groups.push(GroupLayout { name, cols, rows, cell_w });
    }
    let content_w = groups
        .iter()
        .map(|g| g.cols as u16 * g.cell_w + (g.cols as u16).saturating_sub(1) * COL_GAP)
        .max()
        .unwrap_or(0);
    let width = (content_w + 2 + 2 * SIDE_PAD).min(panel_cap).max(1);
    // Borders + one top pad row + one header per group + entry rows +
    // one hint row.
    let body: u16 = groups
        .iter()
        .map(|g| 1 + g.rows as u16)
        .sum::<u16>()
        .saturating_add(1)
        .saturating_add(1);
    let height = body.saturating_add(2).max(1);
    HudLayout { groups, width, height }
}

/// Panel rect: centered, bottom-anchored just above the session bar,
/// clamped into tiny terminals.
pub fn whichkey_area(term: Rect) -> Rect {
    let plan = layout(term.width);
    let session_bar_h = if term.height >= 3 { 1 } else { 0 };
    let room_h = term.height.saturating_sub(session_bar_h);
    let h = plan.height.min(room_h.max(1));
    let w = plan.width.min(term.width.max(1));
    let x = term.x + term.width.saturating_sub(w) / 2;
    let y = term.y + room_h.saturating_sub(h);
    Rect::new(x, y, w, h)
}

/// Paint the HUD: one bordered panel, group sections with aligned
/// `key → meaning` columns, a pinned hint row. Over-tall content
/// clips instead of overflowing; over-wide cells clip per line.
/// `term` is the full terminal (for the column plan); `area` is the
/// panel rect from [`whichkey_area`].
pub fn render_whichkey(frame: &mut Frame, term: Rect, area: Rect) {
    if area.width < 5 || area.height < 3 {
        return;
    }
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(crate::theme::border_type())
        .border_style(crate::theme::style(crate::theme::Role::BorderModal))
        .title(" Ctrl-b keys ");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let key_style = crate::theme::style(crate::theme::Role::KeyHint)
        .add_modifier(Modifier::BOLD);
    let desc_style = crate::theme::style(crate::theme::Role::KeyDesc);
    let group_style = crate::theme::style(crate::theme::Role::Muted);
    let mut lines: Vec<Line<'static>> = vec![Line::from("")];
    let all = entries();
    for group in &layout(term.width).groups {
        lines.push(Line::from(vec![
            Span::raw(" ".repeat(SIDE_PAD as usize)),
            Span::styled(group.name.to_string(), group_style),
        ]));
        let items: Vec<&WhichEntry> =
            all.iter().filter(|e| e.group == group.name).collect();
        let key_w = items.iter().map(|e| display_width(e.key)).max().unwrap_or(0) as usize;
        for row in 0..group.rows {
            let mut spans = vec![Span::raw(" ".repeat(SIDE_PAD as usize))];
            for col in 0..group.cols {
                // Column-major: entries run top to bottom, then next
                // column, so the eye scans down each short list.
                if let Some(entry) = items.get(col * group.rows + row) {
                    if col > 0 {
                        spans.push(Span::raw(" ".repeat(COL_GAP as usize)));
                    }
                    let pad = key_w.saturating_sub(display_width(entry.key) as usize);
                    spans.push(Span::styled(entry.key.to_string(), key_style));
                    spans.push(Span::raw(" ".repeat(pad)));
                    spans.push(Span::raw(KEY_GAP.to_string()));
                    spans.push(Span::styled(entry.desc.to_string(), desc_style));
                    let used = key_w + KEY_GAP.len() + display_width(entry.desc) as usize;
                    let cell_pad =
                        (group.cell_w as usize).saturating_sub(used);
                    spans.push(Span::raw(" ".repeat(cell_pad)));
                }
            }
            lines.push(Line::from(spans));
        }
    }
    // Foot row: the escape path, right-aligned with the same side
    // margin as the content. It pins to the last visible row, so
    // short terminals clip entries before they ever lose the exit.
    let width = inner.width as usize;
    let room = inner.height as usize;
    let hint = hint_line(width, group_style);
    let mut rows: Vec<Line<'static>> = lines
        .into_iter()
        .take(room.saturating_sub(1))
        .map(|line| clip_line(line, width))
        .collect();
    if room > 0 {
        rows.push(clip_line(hint, width));
    }
    frame.render_widget(Paragraph::new(rows), inner);
}

/// The `Esc close` foot row: right-aligned with a side margin, so it
/// never touches the border.
fn hint_line(width: usize, style: ratatui::style::Style) -> Line<'static> {
    let text = "Esc close";
    let pad = width
        .saturating_sub(display_width(text) as usize)
        .saturating_sub(SIDE_PAD as usize);
    Line::from(vec![
        Span::raw(" ".repeat(pad)),
        Span::styled(text.to_string(), style),
    ])
}

/// Clip one built row to `width` cells, preserving span styles.
fn clip_line(line: Line<'static>, width: usize) -> Line<'static> {
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut room = width;
    for span in line.spans {
        if room == 0 {
            break;
        }
        let take: String = span.content.chars().take(room).collect();
        room -= take.chars().count();
        if !take.is_empty() {
            out.push(Span::styled(take, span.style));
        }
    }
    Line::from(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    #[test]
    fn delay_is_responsive_but_not_twitchy() {
        assert!(
            (150..=400).contains(&WHICHKEY_DELAY_MS),
            "delay {WHICHKEY_DELAY_MS}ms must sit in the 150-400ms band"
        );
    }

    #[test]
    fn hud_covers_everything_the_router_accepts_and_nothing_else() {
        // Behavioral probe, not a hardcoded list: feed every plausible
        // second key through a real router and require the HUD to list
        // exactly the keys that resolve. A shortcut added anywhere but
        // the binding table fails here.
        use crate::input::{prefix_key, InputRouter, RoutedKey};
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut probes: Vec<KeyEvent> = ('a'..='z')
            .chain('A'..='Z')
            .chain('0'..='9')
            .map(|c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))
            .collect();
        probes.push(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        probes.push(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE));
        let mut fired: Vec<String> = Vec::new();
        for probe in &probes {
            let mut router = InputRouter::new();
            assert_eq!(router.feed(prefix_key()), RoutedKey::PrefixPending);
            match router.feed(*probe) {
                RoutedKey::Command(_) => {
                    let label = match probe.code {
                        KeyCode::Char('1'..='9') => "1–9".to_string(),
                        KeyCode::Char(c) => c.to_string(),
                        KeyCode::Enter => "Enter".to_string(),
                        _ => panic!("unexpected command key {probe:?}"),
                    };
                    if !fired.contains(&label) {
                        fired.push(label);
                    }
                }
                RoutedKey::Help => {
                    if !fired.contains(&"?".to_string()) {
                        fired.push("?".to_string());
                    }
                }
                RoutedKey::Forward(_) | RoutedKey::Cancelled | RoutedKey::PrefixPending => {}
            }
        }
        let shown: Vec<&str> = entries().iter().map(|e| e.key).collect();
        for label in &fired {
            assert!(
                shown.contains(&label.as_str()),
                "router fires {label:?} but the HUD never lists it"
            );
        }
        for label in &shown {
            assert!(
                fired.contains(&label.to_string()),
                "HUD lists {label:?} but the router never fires it"
            );
        }
    }

    #[test]
    fn descriptions_are_human_not_identifiers() {
        for entry in entries() {
            assert!(!entry.desc.is_empty(), "empty description for {:?}", entry.key);
            assert!(
                !entry.desc.contains('_'),
                "identifier leak in {:?}: {:?}",
                entry.key,
                entry.desc
            );
            assert!(
                entry.desc.chars().count() <= 24,
                "description too long for {:?}: {:?}",
                entry.key,
                entry.desc
            );
            let first = entry.desc.chars().next().unwrap();
            assert!(
                first.is_uppercase() || first.is_numeric(),
                "description should read as a label: {:?}",
                entry.desc
            );
        }
    }

    #[test]
    fn hud_starts_hidden_and_idle() {
        let hud = WhichKeyHud::new();
        assert!(!hud.visible());
        assert!(!hud.pinned());
    }

    #[test]
    fn transient_shows_only_after_the_delay() {
        let mut hud = WhichKeyHud::new();
        let t0 = std::time::Instant::now();
        hud.note_pending(t0);
        assert!(!hud.visible(), "nothing shows the instant prefix hits");
        hud.poll(true, t0 + std::time::Duration::from_millis(WHICHKEY_DELAY_MS - 1));
        assert!(!hud.visible(), "no flash just before the deadline");
        hud.poll(true, t0 + std::time::Duration::from_millis(WHICHKEY_DELAY_MS));
        assert!(hud.visible(), "HUD appears once the delay elapses");
        assert!(!hud.pinned(), "delay path never pins");
    }

    #[test]
    fn expert_sequence_resolves_before_any_paint() {
        let mut hud = WhichKeyHud::new();
        let t0 = std::time::Instant::now();
        hud.note_pending(t0);
        hud.poll(true, t0 + std::time::Duration::from_millis(50));
        hud.note_resolved();
        hud.poll(false, t0 + std::time::Duration::from_millis(WHICHKEY_DELAY_MS + 100));
        assert!(!hud.visible(), "fast typists never see the overlay");
    }

    #[test]
    fn explicit_help_pins_immediately() {
        let mut hud = WhichKeyHud::new();
        hud.show_pinned();
        assert!(hud.visible() && hud.pinned());
        // A pinned HUD survives ticks with no pending prefix.
        hud.poll(false, std::time::Instant::now());
        assert!(hud.visible(), "pinned HUD stays open for browsing");
    }

    #[test]
    fn hide_clears_everything() {
        let mut hud = WhichKeyHud::new();
        hud.show_pinned();
        hud.hide();
        assert!(!hud.visible() && !hud.pinned());
    }

    #[test]
    fn layout_fits_narrow_terminals_without_overflow() {
        let plan = layout(40);
        assert!(plan.width <= 40, "panel overflows: {}", plan.width);
        // The big group stacks; a pair only where it still fits.
        let session = plan.groups.iter().find(|g| g.name == "Session").expect("session group");
        assert_eq!(session.cols, 1, "the wide group must stack at 40");
        assert_eq!(plan.height, 25, "pinned plan height at 40");
    }

    #[test]
    fn narrow_panels_degrade_gracefully() {
        // 40x24 cannot hold every row: entries may clip, but the
        // panel stays in bounds, above the session bar, exit intact.
        let backend = TestBackend::new(40, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| render_whichkey(f, f.area(), whichkey_area(f.area())))
            .unwrap();
        let text = buffer_text(&terminal);
        for want in ["New session", "Ctrl-b", "Esc close"] {
            assert!(text.contains(want), "40x24 lost {want:?}\n{text}");
        }
        let area = whichkey_area(ratatui::layout::Rect::new(0, 0, 40, 24));
        assert_eq!(area.bottom(), 23, "still above the session bar");
        assert!(area.right() <= 40, "{area:?}");
    }

    #[test]
    fn layout_uses_columns_when_wide() {
        let layout = layout(80);
        assert!(layout.width <= 80 - 2, "needs side margin: {}", layout.width);
        assert!(
            layout.groups.iter().any(|g| g.cols >= 2),
            "wide panels should prefer columns over a long list"
        );
    }

    #[test]
    fn wide_panels_stay_compact() {
        let plan = layout(120);
        assert!(plan.width <= 72, "panels stop growing: {}", plan.width);
        assert!(
            plan.groups.iter().all(|g| g.cols <= 2),
            "two columns stay scannable at any width"
        );
    }

    #[test]
    fn short_terminals_keep_the_exit_row() {
        let backend = TestBackend::new(60, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| render_whichkey(f, f.area(), whichkey_area(f.area())))
            .unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("Esc close"), "clipping must spare the exit\n{text}");
        let area = whichkey_area(ratatui::layout::Rect::new(0, 0, 60, 20));
        assert_eq!(area.bottom(), 19, "still above the session bar");
    }

    #[test]
    fn area_anchors_above_the_session_bar() {
        let term = ratatui::layout::Rect::new(0, 0, 80, 24);
        let area = whichkey_area(term);
        assert_eq!(area.bottom(), 23, "one row stays for the session bar");
        assert!(area.x >= 2, "keeps a side margin");
        assert!(area.right() <= 78, "keeps a side margin");
        let center = term.x + term.width / 2;
        assert!(
            (area.x + area.width / 2).abs_diff(center) <= 1,
            "roughly centered: {area:?}"
        );
    }

    #[test]
    fn render_shows_keys_descriptions_and_prefix() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| render_whichkey(f, f.area(), whichkey_area(f.area())))
            .unwrap();
        let text = buffer_text(&terminal);
        for want in [
            "New session",
            "Next session",
            "Telegram settings",
            "Ctrl-b",
            "Esc close",
            "Session",
        ] {
            assert!(text.contains(want), "HUD missing {want:?}\n{text}");
        }
    }

    #[test]
    fn keys_are_styled_distinct_from_descriptions() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| render_whichkey(f, f.area(), whichkey_area(f.area())))
            .unwrap();
        let key_pos = find_in_buffer(&terminal, "New session").expect("desc paints");
        // The key cell sits on the same row, left of the description.
        let buffer = terminal.backend().buffer().clone();
        let row = key_pos.1;
        let mut key_styled = false;
        for x in 0..key_pos.0 {
            let cell = buffer.cell((x, row)).expect("cell in bounds");
            if cell.symbol() == "c" && cell.fg == ratatui::style::Color::Cyan {
                key_styled = true;
            }
        }
        assert!(key_styled, "key `c` paints cyan ahead of its description");
    }

    #[test]
    fn render_tiny_terminal_stays_in_bounds() {
        let backend = TestBackend::new(40, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| render_whichkey(f, f.area(), whichkey_area(f.area())))
            .unwrap();
        let area = whichkey_area(ratatui::layout::Rect::new(0, 0, 40, 10));
        assert!(area.right() <= 40 && area.bottom() <= 10, "{area:?}");
    }

    fn buffer_text(terminal: &Terminal<TestBackend>) -> String {
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    fn find_in_buffer(terminal: &Terminal<TestBackend>, needle: &str) -> Option<(u16, u16)> {
        let buffer = terminal.backend().buffer().clone();
        let (w, h) = (buffer.area.width, buffer.area.height);
        for y in 0..h {
            let row: String = (0..w)
                .map(|x| buffer.cell((x, y)).expect("cell").symbol().to_string())
                .collect();
            if let Some(x) = row.find(needle) {
                return Some((x as u16, y));
            }
        }
        None
    }
}
