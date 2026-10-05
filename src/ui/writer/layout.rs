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

pub fn writer_layout(area: Rect) -> WriterLayout {
    // One cell of border all around, then 2 cells of side padding
    // and 1 row of top padding inside it; 1 row of bottom padding so
    // content never touches the chrome.
    let inner_x = area.x.saturating_add(3);
    let inner_y = area.y.saturating_add(2);
    let inner_w = area.width.saturating_sub(6);
    let inner_h = area.height.saturating_sub(4);
    let title = Rect::new(inner_x, inner_y, inner_w, 1);
    let chat = Rect::new(inner_x, inner_y.saturating_add(inner_h.saturating_sub(6)), inner_w, 3);
    let status = Rect::new(inner_x, inner_y.saturating_add(inner_h.saturating_sub(3)), inner_w, 1);
    let action = Rect::new(inner_x, inner_y.saturating_add(inner_h.saturating_sub(2)), inner_w, 1);
    let error = Rect::new(inner_x, inner_y.saturating_add(inner_h.saturating_sub(1)), inner_w, 1);
    let body_y = inner_y.saturating_add(1);
    let body_h = inner_h.saturating_sub(8);
    let body = Rect::new(inner_x, body_y, inner_w, body_h);
    let editor_w = inner_w * 65 / 100;
    let gutter = Rect::new(inner_x, body_y, 1.min(editor_w), body_h);
    let editor = Rect::new(
        inner_x.saturating_add(1),
        body_y,
        editor_w.saturating_sub(1),
        body_h,
    );
    let panel = Rect::new(
        inner_x.saturating_add(editor_w),
        body_y,
        inner_w.saturating_sub(editor_w),
        body_h,
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
pub(super) fn pill_spans(label: &str, emphasized: bool, default_mark: bool) -> Vec<Span<'static>> {
    let (fill, left, right) = button_chrome(
        emphasized,
        style(Role::TabActive),
        style(Role::TabInactive),
        pill_rest_cap(),
    );
    vec![
        Span::styled(pill_left().to_string(), Style::default().fg(left)),
        Span::styled(
            format!("{}{label}", if default_mark { "*" } else { "" }),
            fill,
        ),
        Span::styled(pill_right().to_string(), Style::default().fg(right)),
    ]
}

/// Cell width of one rendered pill: two caps plus label and mark.
pub fn pill_width(label: &str, default_mark: bool) -> u16 {
    (label.chars().count() + 2 + usize::from(default_mark)) as u16
}

/// Hit rects for the empty-state pills, centered in the body width.
/// Paint and mouse dispatch share these, so clicks can never desync.
/// Empty rects when the body has no room.
pub fn empty_pill_rects(body: Rect) -> (Rect, Rect) {
    let new_width = pill_width("New document", true);
    let open_width = pill_width("Open…", false);
    let gap: u16 = 2;
    let row_width = new_width.saturating_add(gap).saturating_add(open_width);
    let y = body.y.saturating_add(1);
    if body.width < row_width || body.height < 2 {
        return (Rect::default(), Rect::default());
    }
    let x = body.x.saturating_add(body.width.saturating_sub(row_width) / 2);
    (
        Rect::new(x, y, new_width, 1),
        Rect::new(x.saturating_add(new_width).saturating_add(gap), y, open_width, 1),
    )
}

/// Hit rects for the action-row pills, left-aligned to the grid.
/// Paint and mouse dispatch share these.
pub fn action_pill_rects(action: Rect) -> (Rect, Rect) {
    let rephrase = pill_width("Rephrase", false);
    let save = pill_width("Save", false);
    let gap: u16 = 2;
    (
        Rect::new(action.x, action.y, rephrase.min(action.width), 1),
        Rect::new(
            action.x.saturating_add(rephrase).saturating_add(gap),
            action.y,
            save,
            1,
        ),
    )
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

