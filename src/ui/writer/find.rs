//! Find bar paint: one fixed row in the error slot, so opening it
//! never shifts the layout. Segments: the query field, the match
//! counter (or the transient note), the case pill, and — with
//! replace open — the replace field plus the (Replace) and
//! (Replace all) pills. Every pill and field rect comes from
//! [`find_bar_rects`](super::layout::find_bar_rects), which the mouse
//! dispatch hit-tests too.

use ratatui::layout::{Position, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use super::layout::{find_bar_rects, find_case_label, find_counter, pill_spans};
use crate::app::writer::{FindFocus, WriterFocus, WriterSession};
use crate::ui::theme::{style, Role};

/// Paint the bar; returns the focused field's cursor cell for the
/// terminal when the editor owns focus (typing lands in the bar).
/// The cursor renders as a `▌` caret, prompt parity; a select-all
/// renders the whole field reversed for type-to-replace.
pub fn paint_find_bar(
    f: &mut Frame,
    slot: Rect,
    session: &WriterSession,
) -> Option<Position> {
    let find = session.find.as_ref()?;
    if slot.height == 0 {
        return None;
    }
    let rects = find_bar_rects(slot, find);
    let mut spans = vec![Span::styled("Find ".to_string(), style(Role::Muted))];
    spans.extend(field_spans(
        &find.query,
        find.cursor,
        find.select_all && find.focus == FindFocus::Query,
        rects.query.width,
    ));
    let counter = find_counter(find);
    if !counter.is_empty() {
        spans.push(Span::raw(" ".to_string()));
        let role = if find.note.is_some() {
            Role::Info
        } else {
            Role::Muted
        };
        spans.push(Span::styled(counter, style(role)));
    }
    spans.push(Span::raw(" ".to_string()));
    let case_focused = find.focus == FindFocus::CaseBtn;
    spans.extend(pill_spans(
        find_case_label(find),
        case_focused,
        case_focused.then_some('>'),
    ));
    // The replace toggle doubles as the field's label: `Replace ▸`
    // opens, `Replace ▾` collapses. Closed, the Alt+H twin rides
    // along as muted hint text so replace is discoverable without
    // the shortcut. The glyph is the state cue (never color alone);
    // Tab focus shows as the emphasized fill, with no `>` mark, so
    // the pill never changes width between states.
    spans.push(Span::raw(" ".to_string()));
    let toggle_focused = find.focus == FindFocus::ToggleBtn;
    spans.extend(pill_spans(
        if find.replace_open {
            "Replace ▾"
        } else {
            "Replace ▸"
        },
        toggle_focused,
        None,
    ));
    if find.replace_open {
        spans.push(Span::raw(" ".to_string()));
        spans.extend(field_spans(
            &find.replace,
            find.replace_cursor,
            find.select_all && find.focus == FindFocus::Replace,
            rects.replace.width,
        ));
        spans.push(Span::raw(" ".to_string()));
        let replace_focused = find.focus == FindFocus::ReplaceNextBtn;
        spans.extend(pill_spans(
            "Replace next",
            replace_focused,
            replace_focused.then_some('>'),
        ));
        spans.push(Span::raw(" ".to_string()));
        let all_focused = find.focus == FindFocus::ReplaceAllBtn;
        spans.extend(pill_spans(
            "Replace all",
            all_focused,
            all_focused.then_some('>'),
        ));
    } else {
        spans.push(Span::styled(
            " Alt+H replace".to_string(),
            style(Role::Muted),
        ));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), slot);
    if session.focus != WriterFocus::Editor {
        return None;
    }
    let (rect, text, caret) = match find.focus {
        FindFocus::Replace if find.replace_open => {
            (rects.replace, find.replace.as_str(), find.replace_cursor)
        }
        FindFocus::Query => (rects.query, find.query.as_str(), find.cursor),
        _ => return None,
    };
    field_cursor(rect, text, caret)
}

/// One `[text]` field with a `▌` caret (or a reversed whole when the
/// field is marked for type-to-replace), scrolled so the caret stays
/// visible. `width` is the full rect; the brackets take two cells and
/// trailing blanks pad to the rect, so the segments after the field
/// sit exactly on their hit rects.
fn field_spans(text: &str, caret: usize, select_all: bool, width: u16) -> Vec<Span<'static>> {
    let inner = width.saturating_sub(2) as usize;
    let mut out = vec![Span::styled("[".to_string(), style(Role::Muted))];
    // Cells the field content occupies (brackets excluded).
    let used: usize;
    if select_all {
        let shown: String = text.chars().take(inner).collect();
        used = shown.chars().count();
        out.push(Span::styled(
            shown,
            style(Role::Text).add_modifier(ratatui::style::Modifier::REVERSED),
        ));
    } else {
        let len = text.chars().count();
        let at = caret.min(len);
        let start = at.saturating_sub(inner.saturating_sub(1)).min(at);
        let before: String = text.chars().skip(start).take(at - start).collect();
        let after: String = text
            .chars()
            .skip(at)
            .take(inner.saturating_sub(at - start).saturating_sub(1))
            .collect();
        used = before.chars().count() + 1 + after.chars().count();
        out.push(Span::styled(before, style(Role::Text)));
        out.push(Span::styled("▌".to_string(), style(Role::Focus)));
        out.push(Span::styled(after, style(Role::Text)));
    }
    out.push(Span::styled("]".to_string(), style(Role::Muted)));
    let pad = inner.saturating_sub(used);
    if pad > 0 {
        out.push(Span::raw(" ".repeat(pad)));
    }
    out
}

/// Terminal cursor cell for a field caret, mirroring the scroll in
/// [`field_spans`]; the brackets offset by one.
fn field_cursor(rect: Rect, text: &str, caret: usize) -> Option<Position> {
    let inner = rect.width.saturating_sub(2) as usize;
    if inner == 0 {
        return None;
    }
    let len = text.chars().count();
    let at = caret.min(len);
    let start = at.saturating_sub(inner.saturating_sub(1)).min(at);
    Some(Position::new(
        rect.x.saturating_add(1).saturating_add((at - start) as u16),
        rect.y,
    ))
}
