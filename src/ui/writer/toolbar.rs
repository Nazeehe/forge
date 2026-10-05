//! Writer toolbar paint: the always-visible button row (row 1 in
//! both states), the narrow-mode More menu, and the actionable
//! error-slot confirm row. Geometry lives in `layout` and is shared
//! with mouse dispatch.

use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use super::layout::{
    confirm_label, confirm_pill_rects, more_menu_item_rects, pill_spans, pill_spans_disabled,
    toolbar_enabled, toolbar_label, toolbar_narrow, toolbar_pill_rects, ToolbarButton,
};
use crate::app::writer::{PendingConfirm, WriterSession};
use crate::ui::theme::{style, Role};

/// Toolbar row: file pills left, the `│` view-group separator, then
/// Preview (disabled until E8) and the Assistant toggle; the open
/// file name trails right with its dirty dot. Disabled pills render
/// dimmed with `░` markers — never color alone — and never fire.
pub(super) fn paint_toolbar(
    f: &mut Frame,
    row: Rect,
    session: &WriterSession,
    term_cols: u16,
    filename: Option<(&str, bool)>,
) {
    let narrow = toolbar_narrow(term_cols);
    let rects = toolbar_pill_rects(row, narrow, session);
    for (rect, button) in &rects {
        if rect.width == 0 {
            continue;
        }
        let label = toolbar_label(*button);
        let spans = if toolbar_enabled(session, *button) {
            // One shared marker source with the hit rects: a marked
            // pill never outpaints its rect.
            let mark = super::layout::toolbar_mark(session, *button);
            pill_spans(label, mark.is_some(), mark)
        } else {
            pill_spans_disabled(label)
        };
        f.render_widget(Paragraph::new(Line::from(spans)), *rect);
    }
    // The `│` sits in the two-cell gap the geometry leaves between
    // the Close and Preview pills.
    let close_end = rects
        .iter()
        .find(|(_, b)| *b == ToolbarButton::Close)
        .map(|(r, _)| r.x.saturating_add(r.width));
    let preview_x = rects
        .iter()
        .find(|(_, b)| *b == ToolbarButton::Preview)
        .map(|(r, _)| r.x);
    if let (Some(end), Some(px)) = (close_end, preview_x) {
        if px == end.saturating_add(2) && end.saturating_add(1) < row.x.saturating_add(row.width) {
            f.render_widget(
                Paragraph::new(Line::from(vec![Span::styled(
                    "│".to_string(),
                    style(Role::Muted),
                )])),
                Rect::new(end.saturating_add(1), row.y, 1, 1),
            );
        }
    }
    let Some((name, dirty)) = filename else {
        return;
    };
    let mut text = format!("{name}{}", if dirty { " ●" } else { "" });
    let pills_end = rects
        .iter()
        .filter(|(r, _)| r.width > 0)
        .map(|(r, _)| r.x.saturating_add(r.width))
        .max()
        .unwrap_or(row.x);
    let row_end = row.x.saturating_add(row.width);
    let mut x = row_end.saturating_sub(text.chars().count() as u16);
    if x < pills_end.saturating_add(1) {
        // Overlong names yield from the left, keeping the tail.
        let fit = row_end.saturating_sub(pills_end.saturating_add(1)) as usize;
        if fit < 4 {
            return;
        }
        let chars: Vec<char> = text.chars().collect();
        text = format!("…{}", chars[chars.len().saturating_sub(fit - 1)..].iter().collect::<String>());
        x = pills_end.saturating_add(1);
    }
    f.render_widget(
        Paragraph::new(Line::from(vec![Span::styled(text, style(Role::Text))])),
        Rect::new(x, row.y, row_end.saturating_sub(x), 1),
    );
}

/// Narrow-mode More menu: Save-as, Preview, Assistant rows under the
/// More pill. Nothing paints unless the menu is open; a second click
/// (or Esc, or a click anywhere else) closes it.
pub(super) fn paint_more_menu(
    f: &mut Frame,
    more: Rect,
    menu: Rect,
    session: &WriterSession,
) {
    if !session.more_open || more.width == 0 || menu.width == 0 {
        return;
    }
    for (rect, button) in more_menu_item_rects(menu, session) {
        if rect.width == 0 {
            continue;
        }
        let label = toolbar_label(button);
        let spans = if toolbar_enabled(session, button) {
            let mark = super::layout::toolbar_mark(session, button);
            pill_spans(label, mark.is_some(), mark)
        } else {
            pill_spans_disabled(label)
        };
        f.render_widget(Paragraph::new(Line::from(spans)), rect);
    }
}

/// Separator rule under the toolbar, across one column. Shared by
/// the empty and document states so both heads match.
pub(super) fn paint_separator(f: &mut Frame, col: Rect, y: u16) {
    if col.width == 0 {
        return;
    }
    f.render_widget(
        Paragraph::new(Line::from(vec![Span::styled(
            "─".repeat(col.width as usize),
            style(Role::Muted),
        )])),
        Rect::new(col.x, y, col.width, 1),
    );
}

/// Divider column between the editor and a visible panel, full body
/// height. The column is `panel.x - 1`; paint and mouse share the
/// layout, and the column is never a hit area.
pub(super) fn paint_divider(f: &mut Frame, panel_x: u16, body_y: u16, body_h: u16) {
    for y in body_y..body_y.saturating_add(body_h) {
        f.render_widget(
            Paragraph::new(Line::from(vec![Span::styled(
                "│".to_string(),
                style(Role::Muted),
            )])),
            Rect::new(panel_x.saturating_sub(1), y, 1, 1),
        );
    }
}

/// Actionable error-slot row: the message plus one pill per action,
/// the first default-marked. Replaces the plain error text while a
/// confirm is pending; paint and mouse share the rects.
pub(super) fn paint_confirm_slot(
    f: &mut Frame,
    slot: Rect,
    confirm: &PendingConfirm,
) {
    // Pills that overflow the slot never paint; the mouse shares the
    // same rects, so an unpainted pill can never fire either.
    let rects = confirm_pill_rects(slot, &confirm.message, &confirm.actions);
    let mut spans = vec![Span::styled(confirm.message.clone(), style(Role::Warning))];
    for (index, action) in confirm.actions.iter().enumerate() {
        if rects.get(index).is_none_or(|r| r.width == 0) {
            continue;
        }
        spans.push(Span::styled("  ".to_string(), ratatui::style::Style::default()));
        spans.extend(pill_spans(
            confirm_label(action),
            index == 0,
            (index == 0).then_some('*'),
        ));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), slot);
}
