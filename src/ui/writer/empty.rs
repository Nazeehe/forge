//! Writer empty-state paint: the Variant A body (header, Start,
//! Recent, Tip), the typed-path prompt block replacing Start while
//! open, and the pinned hint row. The toolbar (row 1) and the fixed
//! error slot live in `toolbar`.

use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use super::layout::{
    ellipsize_middle, pill_spans, prompt_button_rects, prompt_sugg_rect,
    prompt_suggestions, prompt_submit_label, prompt_submit_verb, recent_window, start_new_rect, PROMPT_ORIGIN_OFF,
    start_open_rect, writer_layout, EMPTY_INDENT,
};
use super::thread::{paint_error_slot, paint_panel};
use super::toolbar::{paint_confirm_slot, paint_more_menu, paint_toolbar};
use crate::app::writer::{PromptKind, WriterOpenPrompt, WriterSession};
use crate::ui::theme::{style, Role};
use crate::ui::writer::layout::more_menu_rect;

/// Empty state: toolbar, separator, Variant A body, hints in the
/// status row, confirm-or-error in the slot. With the panel visible
/// the body takes the editor column and the thread paints beside it.
/// Returns the prompt input cursor cell while the prompt is open.
pub fn paint_empty(
    f: &mut Frame,
    area: Rect,
    session: &WriterSession,
    term_cols: u16,
) -> Option<ratatui::layout::Position> {
    use crate::ui::writer::layout::toolbar_pill_rects;
    use crate::ui::writer::layout::toolbar_narrow;
    use crate::ui::writer::layout::ToolbarButton;

    let layout = writer_layout(area, session.panel_visible);
    super::editor::paint_frame(f, area);
    paint_toolbar(f, layout.title, session, term_cols, None);
    // Narrow-mode menu floats over the separator and body head.
    let narrow = toolbar_narrow(term_cols);
    let more = toolbar_pill_rects(layout.title, narrow, session)
        .into_iter()
        .find(|(_, b)| *b == ToolbarButton::More)
        .map(|(r, _)| r)
        .unwrap_or_default();
    paint_more_menu(f, more, more_menu_rect(more), session);
    // Body column: the editor column while the panel shows, else the
    // full body. Everything below indents to one column.
    let col = if session.panel_visible {
        layout.editor
    } else {
        layout.body
    };
    let x0 = col.x.saturating_add(EMPTY_INDENT);
    let width = col.width.saturating_sub(EMPTY_INDENT);
    if width == 0 {
        return None;
    }
    paint_separator(f, col, layout.body.y);
    paint_header(f, x0, width, layout.body.y.saturating_add(2), session);
    let mut cursor = None;
    let mut next = layout.body.y.saturating_add(PROMPT_ORIGIN_OFF);
    if let Some(prompt) = session.open_prompt.as_ref() {
        let (after, pos) = paint_prompt_block(f, x0, width, next, session, prompt);
        cursor = pos;
        next = after;
    } else {
        next = paint_start(f, x0, width, next);
        // Rows 0..9 above plus the Tip below: what remains fits rows.
        let room = layout.body.height.saturating_sub(11) as usize;
        next = paint_recent(f, x0, width, next, session, room);
    }
    paint_tip(f, x0, width, next);
    paint_empty_hints(f, layout.status);
    if session.panel_visible {
        paint_panel(f, layout.panel, session, "");
    }
    match session.pending_confirm.as_ref() {
        Some(confirm) => paint_confirm_slot(f, layout.error, confirm),
        None => paint_error_slot(f, layout.error, session.error.as_deref()),
    }
    cursor
}

/// Separator rule under the toolbar, across the body column.
fn paint_separator(f: &mut Frame, col: Rect, y: u16) {
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

/// Header: `📝 Writer`, one-line subhead with the session cwd
/// (middle-ellipsis when too long).
fn paint_header(f: &mut Frame, x: u16, width: u16, y: u16, session: &WriterSession) {
    f.render_widget(
        Paragraph::new(Line::from(vec![Span::styled("📝  Writer".to_string(), style(Role::Text))])),
        Rect::new(x, y, width, 1),
    );
    let mut sub = "Markdown editor for this session's folder".to_string();
    if let Some(cwd) = session.recent_cwd.as_ref().map(|p| p.to_string_lossy().into_owned()) {
        if !cwd.is_empty() {
            sub = format!("Markdown editor · {}", ellipsize_middle(&cwd, width.saturating_sub(19) as usize));
        }
    }
    f.render_widget(
        Paragraph::new(Line::from(vec![Span::styled(sub, style(Role::Muted))])),
        Rect::new(x, y.saturating_add(1), width, 1),
    );
}

/// Start block: section head plus the two entry pills with their key
/// twins on one grid (twins at +18, descriptions at +28). Returns the
/// next free row, leaving one blank row before Recent.
fn paint_start(f: &mut Frame, x: u16, width: u16, y: u16) -> u16 {
    f.render_widget(
        Paragraph::new(Line::from(vec![Span::styled("Start".to_string(), style(Role::Muted))])),
        Rect::new(x, y, width, 1),
    );
    let rows = [
        ("New document", true, "Ctrl+N", "create a .md file here"),
        ("Open…", false, "Ctrl+O", "open an existing file"),
    ];
    for (i, (label, mark, key, what)) in rows.iter().enumerate() {
        let row_y = y.saturating_add(1 + i as u16);
        // The pill paints through the shared rect mouse hit-tests;
        // twins align at +18, descriptions at +28: one grid.
        let pill = if i == 0 {
            start_new_rect(x, row_y)
        } else {
            start_open_rect(x, row_y)
        };
        f.render_widget(
            Paragraph::new(Line::from(pill_spans(label, *mark, *mark))),
            pill,
        );
        let rest = vec![
            Span::styled(key.to_string(), style(Role::Muted)),
            Span::styled("    ".to_string(), Style::default()),
            Span::styled(what.to_string(), style(Role::Muted)),
        ];
        let rest_x = x.saturating_add(18);
        f.render_widget(
            Paragraph::new(Line::from(rest)),
            Rect::new(rest_x, row_y, width.saturating_sub(18), 1),
        );
    }
    y.saturating_add(4)
}

/// Recent block: section head plus the visible window of cached rows,
/// the keyboard selection carrying `▸`. Empty cache names the slot.
/// Returns the next free row.
fn paint_recent(
    f: &mut Frame,
    x: u16,
    width: u16,
    y: u16,
    session: &WriterSession,
    room: usize,
) -> u16 {
    f.render_widget(
        Paragraph::new(Line::from(vec![Span::styled("Recent".to_string(), style(Role::Muted))])),
        Rect::new(x, y, width, 1),
    );
    if session.recent_cache.is_empty() {
        f.render_widget(
            Paragraph::new(Line::from(vec![Span::styled(
                "No Markdown files found".to_string(),
                style(Role::Muted),
            )])),
            Rect::new(x, y.saturating_add(1), width, 1),
        );
        return y.saturating_add(2);
    }
    // The window keeps the keyboard selection visible when the cache
    // outgrows the body.
    let (start, count) = recent_window(session.recent_cache.len(), session.recent_sel, room.max(1));
    let now = std::time::SystemTime::now();
    for (i, entry) in session.recent_cache.iter().skip(start).take(count).enumerate() {
        let selected = start + i == session.recent_sel;
        let mark = if selected { "▸ " } else { "  " };
        let age = crate::writer::recent::relative_age(entry.mtime, now);
        let line = format!("{mark}{}  edited {age}", ellipsize_middle(&entry.rel, 40));
        f.render_widget(
            Paragraph::new(Line::from(vec![Span::styled(
                line,
                if selected { style(Role::Focus) } else { style(Role::Text) },
            )])),
            Rect::new(x, y.saturating_add(1 + i as u16), width, 1),
        );
    }
    y.saturating_add(1 + count as u16)
}

/// Tip line: the agent can open documents too.
fn paint_tip(f: &mut Frame, x: u16, width: u16, y: u16) {
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("Tip  ".to_string(), style(Role::Brand)),
            Span::styled(
                "The agent can open a document for you, e.g. \"draft notes/intro.md\".".to_string(),
                style(Role::Muted),
            ),
        ])),
        Rect::new(x, y, width, 1),
    );
}

/// Hint row pinned in the status slot; long/short by measured width.
fn paint_empty_hints(f: &mut Frame, status: Rect) {
    let hint = if status.width >= 90 {
        "↑↓ choose recent · Enter open · Ctrl+N new · Ctrl+O open · F6 focus"
    } else {
        "↑↓ recent · Enter open · n/o · F6"
    };
    f.render_widget(
        Paragraph::new(Line::from(vec![Span::styled(hint, style(Role::Muted))])),
        status,
    );
}

/// Path prompt block replacing Start: head, `> ` input, up to 4
/// clickable suggestions, submit + Cancel, hint row. Shared with the
/// document-state overlay. Returns the next free row plus the input
/// cursor cell.
pub(super) fn paint_prompt_block(
    f: &mut Frame,
    x: u16,
    width: u16,
    y: u16,
    session: &WriterSession,
    prompt: &WriterOpenPrompt,
) -> (u16, Option<ratatui::layout::Position>) {
    let head = match prompt.kind {
        PromptKind::New => "New document — path:",
        PromptKind::Open => "Open document — path:",
        PromptKind::SaveAs => "Save as — path:",
    };
    f.render_widget(
        Paragraph::new(Line::from(vec![Span::styled(head.to_string(), style(Role::Muted))])),
        Rect::new(x, y, width, 1),
    );
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("> ".to_string(), style(Role::Brand)),
            Span::styled(prompt.buffer.clone(), style(Role::Text)),
            Span::styled("▌".to_string(), style(Role::Focus)),
        ])),
        Rect::new(x, y.saturating_add(1), width, 1),
    );
    use ratatui::text::Line as TextLine;
    let dx = TextLine::from(format!("> {}", prompt.buffer)).width() as u16;
    let cursor = Some(ratatui::layout::Position::new(
        x.saturating_add(dx.min(width.saturating_sub(1))),
        y.saturating_add(1),
    ));
    let suggestions = prompt_suggestions(session, &prompt.buffer);
    for (index, rel) in suggestions.iter().enumerate() {
        let rect = prompt_sugg_rect(x, y, width, index);
        let mut line = format!("  {rel}");
        if index == 0 {
            line.push_str("        (Tab completes)");
        }
        f.render_widget(
            Paragraph::new(Line::from(vec![Span::styled(line, style(Role::Text))])),
            rect,
        );
    }
    let buttons_y = y.saturating_add(2 + suggestions.len() as u16);
    let (submit_rect, cancel_rect) = prompt_button_rects(x, buttons_y, width, &prompt.kind);
    f.render_widget(
        Paragraph::new(Line::from(pill_spans(prompt_submit_label(&prompt.kind), true, true))),
        submit_rect,
    );
    if cancel_rect.x < x.saturating_add(width) {
        f.render_widget(
            Paragraph::new(Line::from(pill_spans("Cancel", false, false))),
            cancel_rect,
        );
    }
    let verb = prompt_submit_verb(&prompt.kind);
    let hint = format!("Enter {verb} · Tab complete · Esc cancel");
    f.render_widget(
        Paragraph::new(Line::from(vec![Span::styled(hint, style(Role::Muted))])),
        Rect::new(x, buttons_y.saturating_add(1), width, 1),
    );
    (buttons_y.saturating_add(2), cursor)
}
