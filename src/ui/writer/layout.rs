//! Writer overlay geometry: the L2 layout, pill rects shared with
//! mouse dispatch, and the pill-span atoms the painters build.

use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Span;

use crate::ui::theme::{button_chrome, pill_left, pill_rest_cap, pill_right, style, Role};

/// Narrow-terminal cutoff: below this width the assistant panel shows
/// a fixed widen notice instead of the thread.
pub const WIDE_COLS: u16 = 100;

/// Fixed chat box height: selection chip row, `> ` input row, hint row.
pub const CHAT_ROWS: u16 = 3;

/// Every painted region of the Writer overlay, derived from one
/// content area. The 65/35 editor/panel split and all fixed slots are
/// identical at every width; only the panel *content* swaps below
/// [`WIDE_COLS`] terminal columns (the caller threads the terminal
pub struct WriterLayout {
    pub title: Rect,
    pub body: Rect,
    pub gutter: Rect,
    pub editor: Rect,
    pub panel: Rect,
    pub chat: Rect,
    pub status: Rect,
    pub action: Rect,
    pub error: Rect,
}

pub fn writer_layout(area: Rect, panel_visible: bool) -> WriterLayout {
    // One cell of border all around, then 2 cells of side padding
    // and 1 row of top padding inside it; 1 row of bottom padding so
    // content never touches the chrome.
    let inner_x = area.x.saturating_add(3);
    let inner_y = area.y.saturating_add(2);
    let inner_w = area.width.saturating_sub(6);
    let inner_h = area.height.saturating_sub(4);
    let title = Rect::new(inner_x, inner_y, inner_w, 1);
    // Bottom-anchored stack, no gaps: the error slot owns the last
    // row, the status sits pinned directly above it, the action row
    // above the status only while the panel is visible. The chat box
    // is panel chrome too: hidden, it keeps no rows and the body runs
    // down to the status.
    let error = Rect::new(inner_x, inner_y.saturating_add(inner_h.saturating_sub(1)), inner_w, 1);
    let status = Rect::new(inner_x, inner_y.saturating_add(inner_h.saturating_sub(2)), inner_w, 1);
    let action_h = u16::from(panel_visible);
    let action = Rect::new(
        inner_x,
        inner_y.saturating_add(inner_h.saturating_sub(3)),
        inner_w,
        action_h,
    );
    let chat_h = if panel_visible { CHAT_ROWS } else { 0 };
    let chat = Rect::new(
        inner_x,
        inner_y.saturating_add(inner_h.saturating_sub(6)),
        inner_w,
        chat_h,
    );
    let body_y = inner_y.saturating_add(1);
    let body_bottom = if panel_visible {
        chat.y
    } else {
        status.y
    };
    let body_h = body_bottom.saturating_sub(body_y);
    let body = Rect::new(inner_x, body_y, inner_w, body_h);
    let editor_w = if panel_visible {
        inner_w * 65 / 100
    } else {
        inner_w
    };
    let gutter = Rect::new(inner_x, body_y, 1.min(editor_w), body_h);
    let editor = Rect::new(
        inner_x.saturating_add(1),
        body_y,
        editor_w.saturating_sub(1),
        body_h,
    );
    // A visible panel leaves one divider column between itself and
    // the editor; hidden, the editor takes the full width and the
    // panel rect stays empty.
    let (panel_x, panel_w) = if panel_visible {
        (
            inner_x.saturating_add(editor_w).saturating_add(1),
            inner_w.saturating_sub(editor_w).saturating_sub(1),
        )
    } else {
        (inner_x.saturating_add(editor_w), inner_w.saturating_sub(editor_w))
    };
    // Panel content starts below the toolbar rule, like the editor
    // head (rule + one padding row); paint, skip math and click
    // mapping all derive from this rect, so they stay agreed.
    let panel = Rect::new(
        panel_x,
        body_y.saturating_add(2),
        panel_w,
        body_h.saturating_sub(2),
    );
    WriterLayout {
        title,
        body,
        gutter,
        editor,
        panel,
        chat,
        status,
        action,
        error,
    }
}

/// Whether the assistant panel collapses to the widen notice: keyed
/// off terminal columns (the same ≥100 rule as the topbar icons),
/// never the post-sidebar content width.
pub fn panel_collapsed(term_cols: u16) -> bool {
    term_cols < WIDE_COLS
}

/// One pill button: themed caps with an accent fill when emphasized.
/// The marker is explicit: `*` means default action, `✓` means a
/// pressed toggle. The two never share a glyph.
pub(super) fn pill_spans(label: &str, emphasized: bool, mark: Option<char>) -> Vec<Span<'static>> {
    let (fill, left, right) = button_chrome(
        emphasized,
        style(Role::TabActive),
        style(Role::TabInactive),
        pill_rest_cap(),
    );
    vec![
        Span::styled(pill_left().to_string(), Style::default().fg(left)),
        Span::styled(
            format!("{}{label}", mark.map(|m| m.to_string()).unwrap_or_default()),
            fill,
        ),
        Span::styled(pill_right().to_string(), Style::default().fg(right)),
    ]
}

/// Cell width of one rendered pill: two caps plus label and mark.
pub fn pill_width(label: &str, default_mark: bool) -> u16 {
    (label.chars().count() + 2 + usize::from(default_mark)) as u16
}

/// Hit rect for the action-row pill, left-aligned to the grid.
/// Paint and mouse dispatch share it. The row holds `(Rephrase)`
/// alone since E2 (Save moved to the toolbar).
pub fn action_pill_rect(action: Rect) -> Rect {
    Rect::new(action.x, action.y, pill_width("Rephrase", false).min(action.width), 1)
}

/// First recent row offset from the body top: separator, header pair,
/// Start triple, blank, Recent head.
pub const RECENT_FIRST_ROW_OFF: u16 = 10;

/// First prompt-block row offset from the body top in the empty state
/// (the prompt replaces Start at +5).
pub const PROMPT_ORIGIN_OFF: u16 = 5;

/// Hit rects for the empty-state Start pills, which paint inline in
/// the Start rows. Paint and mouse dispatch share these.
pub fn start_new_rect(x: u16, y: u16) -> Rect {
    Rect::new(x, y, pill_width("New document", true), 1)
}

/// Ditto for the Open entry.
pub fn start_open_rect(x: u16, y: u16) -> Rect {
    Rect::new(x, y, pill_width("Open…", false), 1)
}

/// One always-visible toolbar button (E2): the same row in the
/// empty and open states, text pills, never icons.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolbarButton {
    New,
    Open,
    Save,
    SaveAs,
    Close,
    Preview,
    Assistant,
    /// More-menu-only toggle for the editor gutter numbers (E8):
    /// never in the toolbar row itself.
    LineNumbers,
    /// Narrow-mode overflow holding Save-as, Preview, Assistant.
    More,
}

/// Visible toolbar buttons: the full row, or the short set plus the
/// More overflow below [`WIDE_COLS`] terminal columns.
pub fn toolbar_buttons(narrow: bool) -> Vec<ToolbarButton> {
    use ToolbarButton as B;
    if narrow {
        vec![B::New, B::Open, B::Save, B::Close, B::More]
    } else {
        vec![
            B::New,
            B::Open,
            B::Save,
            B::SaveAs,
            B::Close,
            B::Preview,
            B::Assistant,
        ]
    }
}

/// The toolbar collapses keyed off terminal columns, the same ≥100
/// rule as the assistant panel.
pub fn toolbar_narrow(term_cols: u16) -> bool {
    term_cols < WIDE_COLS
}

/// Pill label per button. Save-as keeps its space (`Save as`);
/// Assistant carries the pressed `*` mark while the panel shows.
pub fn toolbar_label(button: ToolbarButton) -> &'static str {
    use ToolbarButton as B;
    match button {
        B::New => "New",
        B::Open => "Open",
        B::Save => "Save",
        B::SaveAs => "Save as",
        B::Close => "Close",
        B::Preview => "Preview",
        B::Assistant => "Assistant",
        B::LineNumbers => "Line numbers",
        B::More => "▾ More",
    }
}

/// Marker for one toolbar pill: `✓` while a toggle (Assistant,
/// Preview, line numbers) is pressed, `*` on a fresh New (no doc,
/// no prompt). Paint and hit rects share this, so a marked pill
/// never outpaints its rect and a later pill never drifts.
pub fn toolbar_mark(
    session: &crate::app::writer::WriterSession,
    button: ToolbarButton,
) -> Option<char> {
    if button == ToolbarButton::Assistant && session.panel_visible {
        Some('✓')
    } else if button == ToolbarButton::Preview && session.preview {
        Some('✓')
    } else if button == ToolbarButton::LineNumbers && session.line_numbers {
        Some('✓')
    } else if matches!(button, ToolbarButton::New)
        && session.doc.is_none()
        && session.open_prompt.is_none()
    {
        Some('*')
    } else {
        None
    }
}

/// Whether a toolbar button fires now. Save needs a dirty doc;
/// Save-as, Close, Preview and line numbers need a doc; the rest
/// (including the Assistant toggle) always fire. Disabled pills
/// render dimmed with `░` markers — never color alone — and never
/// fire.
pub fn toolbar_enabled(
    session: &crate::app::writer::WriterSession,
    button: ToolbarButton,
) -> bool {
    use ToolbarButton as B;
    match button {
        B::New | B::Open | B::Assistant | B::More => true,
        B::Save => session.doc.as_ref().is_some_and(|d| d.dirty),
        B::SaveAs | B::Close | B::Preview | B::LineNumbers => session.doc.is_some(),
    }
}

/// Hit rects for the toolbar pills, left-aligned in the row with
/// 1-cell gaps and the `│` separator between Close and Preview.
/// Paint and mouse dispatch share these. Pills past the row edge
/// come back empty so stray clicks die.
pub fn toolbar_pill_rects(
    row: Rect,
    narrow: bool,
    session: &crate::app::writer::WriterSession,
) -> Vec<(Rect, ToolbarButton)> {
    let mut out = Vec::new();
    let mut x = row.x;
    let end = row.x.saturating_add(row.width);
    let mut first = true;
    for button in toolbar_buttons(narrow) {
        if !first && button == ToolbarButton::Preview {
            // ` │ ` between the file group and the view group.
            x = x.saturating_add(2);
        }
        first = false;
        let mark = toolbar_mark(session, button);
        let label = toolbar_label(button);
        let enabled = toolbar_enabled(session, button);
        let width = if enabled {
            pill_width(label, mark.is_some())
        } else {
            pill_width_disabled(label)
        };
        if x.saturating_add(width) > end {
            out.push((Rect::default(), button));
            continue;
        }
        out.push((Rect::new(x, row.y, width, 1.min(row.height)), button));
        x = x.saturating_add(width).saturating_add(1);
    }
    out
}

/// Cell width of a disabled pill: the caps plus `░`-wrapped label.
pub fn pill_width_disabled(label: &str) -> u16 {
    (label.chars().count() + 4) as u16
}

/// A disabled pill: muted caps with the label wrapped in `░` markers,
/// so the state reads without color.
pub fn pill_spans_disabled(label: &str) -> Vec<Span<'static>> {
    let cap = pill_rest_cap();
    vec![
        Span::styled(pill_left().to_string(), Style::default().fg(cap)),
        Span::styled(format!("░{label}░"), style(Role::Muted)),
        Span::styled(pill_right().to_string(), Style::default().fg(cap)),
    ]
}

/// Confirm/error-slot action label: short verbs, first action default.
pub fn confirm_label(action: &crate::app::writer::ConfirmAction) -> &'static str {
    use crate::app::writer::ConfirmAction as A;
    match action {
        A::Overwrite(_) => "Overwrite",
        A::SaveAndClose => "Save & close",
        A::DiscardClose => "Discard",
        A::Cancel => "Cancel",
        A::OpenInstead(_) => "Open",
        A::CreateInstead(_) => "Create",
    }
}

/// Hit rects for the confirm pills: two cells past the message, then
/// two-cell gaps, first pill default-marked. Pills past the slot edge
/// come back empty. Paint and mouse dispatch share these.
pub fn confirm_pill_rects(
    slot: Rect,
    message: &str,
    actions: &[crate::app::writer::ConfirmAction],
) -> Vec<Rect> {
    let mut out = Vec::new();
    let mut x = slot
        .x
        .saturating_add(message.chars().count() as u16)
        .saturating_add(2);
    let end = slot.x.saturating_add(slot.width);
    for (index, action) in actions.iter().enumerate() {
        let width = pill_width(confirm_label(action), index == 0);
        if x.saturating_add(width) > end {
            out.push(Rect::default());
            continue;
        }
        out.push(Rect::new(x, slot.y, width, 1.min(slot.height)));
        x = x.saturating_add(width).saturating_add(2);
    }
    out
}

/// The More overflow menu: pill rows under the More pill, wide
/// enough for the longest row plus side padding.
pub fn more_menu_rect(more: Rect) -> Rect {
    if more.width == 0 {
        return Rect::default();
    }
    Rect::new(more.x, more.y.saturating_add(1), 22, 4)
}

/// Hit rects for the More-menu rows: Save-as, Preview, Assistant,
/// line numbers. Paint and mouse dispatch share these.
pub fn more_menu_item_rects(
    menu: Rect,
    session: &crate::app::writer::WriterSession,
) -> [(Rect, ToolbarButton); 4] {
    use ToolbarButton as B;
    let row = |i: u16, label: &str, mark: bool| {
        Rect::new(
            menu.x.saturating_add(1),
            menu.y.saturating_add(i),
            pill_width(label, mark).min(menu.width.saturating_sub(2)),
            1,
        )
    };
    [
        (row(0, "Save as", false), B::SaveAs),
        (
            row(1, "Preview", toolbar_mark(session, B::Preview).is_some()),
            B::Preview,
        ),
        (
            row(2, "Assistant", toolbar_mark(session, B::Assistant).is_some()),
            B::Assistant,
        ),
        (
            row(3, "Line numbers", toolbar_mark(session, B::LineNumbers).is_some()),
            B::LineNumbers,
        ),
    ]
}

/// Indent of the Variant A body column from its column edge.
pub const EMPTY_INDENT: u16 = 2;

/// Prompt-block offset in the document view: the A6 head (toolbar
/// rule + one padding row). The doc paint and the mouse dispatch
/// share it, so suggestion/button/input rows always agree.
pub const DOC_PROMPT_OFF: u16 = 2;

/// Visible recent window: the first `room` rows, or the last `room`
/// ending at the keyboard selection, so the selected row never hides
/// below the fold. Returns (start, count).
pub fn recent_window(total: usize, sel: usize, room: usize) -> (usize, usize) {
    if total <= room {
        return (0, total);
    }
    let end = (sel + 1).max(room).min(total);
    (end - room, room)
}

/// Suggestions for the prompt: cached recent rels extending the typed
/// prefix, opened-first (the cache heads opened), at most 4 in the
/// fixed slot. The adapter's Tab completion walks opened, then the
/// cache, then the live filesystem, so a cache first hit agrees with
/// the row; a disk-only hit completes with no row preview (paint never
/// reads the disk).
pub fn prompt_suggestions(
    session: &crate::app::writer::WriterSession,
    prefix: &str,
) -> Vec<String> {
    let mut out = Vec::new();
    for rel in session.recent_cache.iter().map(|e| e.rel.as_str()) {
        if out.len() >= 4 {
            break;
        }
        if rel.starts_with(prefix) && rel.len() > prefix.len() && !out.contains(&rel.to_string()) {
            out.push(rel.to_string());
        }
    }
    out
}

/// Submit pill label per prompt kind.
pub fn prompt_submit_label(kind: &crate::app::writer::PromptKind) -> &'static str {
    match kind {
        crate::app::writer::PromptKind::New => "Create",
        crate::app::writer::PromptKind::Open => "Open",
        crate::app::writer::PromptKind::SaveAs => "Save",
    }
}

/// Submit verb per prompt kind for the hint row.
pub fn prompt_submit_verb(kind: &crate::app::writer::PromptKind) -> &'static str {
    match kind {
        crate::app::writer::PromptKind::New => "create",
        crate::app::writer::PromptKind::Open => "open",
        crate::app::writer::PromptKind::SaveAs => "save",
    }
}

/// Hit rects for the prompt's submit + Cancel pills, left-aligned at
/// the buttons row with a two-cell gap. Paint and mouse share these.
pub fn prompt_button_rects(
    x: u16,
    y: u16,
    width: u16,
    kind: &crate::app::writer::PromptKind,
) -> (Rect, Rect) {
    let submit = pill_width(prompt_submit_label(kind), true);
    let cancel = pill_width("Cancel", false);
    (
        Rect::new(x, y, submit.min(width), 1),
        Rect::new(x.saturating_add(submit).saturating_add(2), y, cancel, 1),
    )
}

/// Hit rect of one prompt suggestion row: the full column width, so
/// the row clicks like a list.
pub fn prompt_sugg_rect(x: u16, origin_y: u16, width: u16, index: usize) -> Rect {
    Rect::new(x, origin_y.saturating_add(2 + index as u16), width, 1)
}

/// Middle-ellipsis: overlong paths keep head and tail around `…`.
pub fn ellipsize_middle(text: &str, max: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max || max < 4 {
        return text.to_string();
    }
    let tail = (max - 1) / 2;
    let head = max - 1 - tail;
    format!("{}…{}", chars[..head].iter().collect::<String>(), chars[chars.len() - tail..].iter().collect::<String>())
}

/// Hit rect of the chat chip's `(✕)` detach, if a selection is live.
/// Paint and mouse dispatch share it.
pub fn chip_detach_rect(chat: Rect, selection: Option<&std::ops::Range<usize>>) -> Option<Rect> {
    let range = selection?;
    use ratatui::text::Line as TextLine;
    let prefix = TextLine::from(format!("[{}–{}] ", range.start, range.end)).width() as u16;
    let x = chat.x.saturating_add(prefix);
    if x.saturating_add(3) > chat.x.saturating_add(chat.width) {
        return None;
    }
    Some(Rect::new(x, chat.y, 3, 1))
}


/// Find-bar hit rects inside the fixed error slot: the two fields
/// plus the four pills (the replace toggle is always present).
/// Paint and mouse dispatch share them, so clicks can never desync
/// from what is on screen.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FindBarRects {
    pub query: Rect,
    pub replace: Rect,
    pub case: Rect,
    pub toggle: Rect,
    pub replace_next_btn: Rect,
    pub replace_all_btn: Rect,
}

/// Match counter (or the transient note) for the bar's middle
/// slot: `3/12`, `0/0` with no hits, `10000+`-suffixed past the
/// cap, empty with no query. Paint and layout share it so the
/// reserved width is exactly what renders.
pub fn find_counter(find: &crate::app::writer::WriterFind) -> String {
    if let Some(note) = find.note.as_deref() {
        return note.to_string();
    }
    if find.query.is_empty() {
        return String::new();
    }
    let total = if find.overflow {
        "10000+".to_string()
    } else {
        find.matches.len().to_string()
    };
    if find.matches.is_empty() {
        return format!("0/{total}");
    }
    format!("{}/{}", find.current + 1, total)
}

/// Case pill label: the letter case IS the state cue (never color
/// alone) — `Aa` sensitive, `aa` not.
pub fn find_case_label(find: &crate::app::writer::WriterFind) -> &'static str {
    if find.case_sensitive {
        "Aa"
    } else {
        "aa"
    }
}

/// Lay out the bar left to right: `Find [query] count (Aa)` plus
/// the replace toggle `(Replace ▸)` (closed, with an `Alt+H
/// replace` hint) or `(Replace ▾) [text] (Replace next)
/// (Replace all)` (open).
/// Fixed segments reserve first; the fields split what is left
/// (query 3/5, replace 2/5). Anything past the slot edge clips in
/// the paint; the pills keep their rects.
pub fn find_bar_rects(
    slot: Rect,
    find: &crate::app::writer::WriterFind,
) -> FindBarRects {
    let mut out = FindBarRects::default();
    if slot.height == 0 || slot.width == 0 {
        return out;
    }
    use crate::app::writer::FindFocus;
    let y = slot.y;
    let counter = find_counter(find);
    let counter_w = counter.chars().count() as u16;
    let case_w = pill_width(find_case_label(find), find.focus == FindFocus::CaseBtn);
    // The toggle label always carries its trailing glyph, so its
    // width never moves between states.
    let toggle_w = pill_width("Replace ▸", false);
    let replace_open = find.replace_open;
    let replace_next_w = pill_width("Replace next", find.focus == FindFocus::ReplaceNextBtn);
    let replace_all_w = pill_width("Replace all", find.focus == FindFocus::ReplaceAllBtn);
    // Fixed cells after the query field: separators plus the counter,
    // the case pill, the toggle, and the replace group when open
    // (closed, the Alt+H hint instead of the group).
    let mut fixed = 1 + case_w + 1 + toggle_w;
    if !counter.is_empty() {
        fixed += 1 + counter_w;
    }
    if replace_open {
        fixed += 1 + replace_next_w + 1 + replace_all_w;
    } else {
        fixed += 14;
    }
    // Past the `Find ` label; the query field owns the rest.
    let mut x = slot.x.saturating_add(5);
    let end = slot.x.saturating_add(slot.width);
    let avail = end.saturating_sub(x).saturating_sub(fixed);
    let (query_w, replace_w) = if replace_open {
        let query_w = (avail * 3 / 5).max(2).min(avail);
        (query_w, avail.saturating_sub(query_w))
    } else {
        (avail, 0)
    };
    out.query = Rect::new(x, y, query_w, 1);
    x = x.saturating_add(query_w);
    if !counter.is_empty() {
        x = x.saturating_add(1);
        x = x.saturating_add(counter_w);
    }
    x = x.saturating_add(1);
    out.case = Rect::new(x, y, case_w, 1);
    x = x.saturating_add(case_w).saturating_add(1);
    out.toggle = Rect::new(x, y, toggle_w, 1);
    x = x.saturating_add(toggle_w);
    if replace_open {
        x = x.saturating_add(1);
        out.replace = Rect::new(x, y, replace_w, 1);
        x = x.saturating_add(replace_w).saturating_add(1);
        out.replace_next_btn = Rect::new(x, y, replace_next_w, 1);
        x = x.saturating_add(replace_next_w).saturating_add(1);
        out.replace_all_btn = Rect::new(x, y, replace_all_w, 1);
    }
    out
}
