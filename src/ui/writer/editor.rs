//! Writer document paint: empty state, the L2 document view with
//! the EdTUI editor plus the proposal gutter, and the wrap math the
//! gutter mapping shares with the adapter's vertical moves.

use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};
use ratatui::Frame;

use super::layout::{empty_pill_rects, panel_collapsed, pill_spans, writer_layout};
use super::thread::{
    paint_actions, paint_chat, paint_error_slot, paint_notice, paint_panel, paint_status,
};
use crate::app::writer::{WriterFocus, WriterSession};
use crate::ui::theme::{style, Role};

pub fn paint_empty(
    f: &mut Frame,
    area: Rect,
    session: &WriterSession,
) -> Option<ratatui::layout::Position> {
    let layout = writer_layout(area);
    let frame = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(style(Role::BorderFocused));
    f.render_widget(frame, area);
    f.render_widget(Paragraph::new(Line::from(vec![Span::styled(
        "Writer",
        style(Role::Text),
    )])), layout.title);
    // Pills center in the content width like dialog primary actions.
    let (new_rect, open_rect) = empty_pill_rects(layout.body);
    let new_pill = pill_spans("New document", session.open_prompt.is_none(), true);
    let open_pill = pill_spans(
        "Open…",
        session
            .open_prompt
            .as_ref()
            .is_some_and(|prompt| !prompt.create),
        false,
    );
    if new_rect.height > 0 {
        f.render_widget(Paragraph::new(Line::from(new_pill)), new_rect);
        f.render_widget(Paragraph::new(Line::from(open_pill)), open_rect);
    }
    let mut row = layout.body.y.saturating_add(3);
    let mut cursor = None;
    if let Some(prompt) = session.open_prompt.as_ref() {
        let head = if prompt.create {
            "New document — path:"
        } else {
            "Open document — path:"
        };
        if row < layout.body.y.saturating_add(layout.body.height) {
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(head.to_string(), style(Role::Muted)),
                    Span::styled(" ".to_string(), Style::default()),
                    Span::styled(prompt.buffer.clone(), style(Role::Text)),
                    Span::styled("▌".to_string(), style(Role::Focus)),
                ])),
                Rect::new(layout.body.x, row, layout.body.width, 1),
            );
            use ratatui::text::Line as TextLine;
            let dx = TextLine::from(format!("{head} {}", prompt.buffer)).width() as u16;
            cursor = Some(ratatui::layout::Position::new(
                layout.body.x.saturating_add(dx.min(layout.body.width.saturating_sub(1))),
                row,
            ));
        }
        row = row.saturating_add(1);
    }
    if row < layout.body.y.saturating_add(layout.body.height) {
        // Hints pick their long/short form from the measured width.
        let hint = if layout.body.width >= 90 {
            "n new · o open · Enter submits · Esc cancels · paths relative to the session dir"
        } else {
            "n new · o open · Enter submits · Esc cancels"
        };
        f.render_widget(
            Paragraph::new(Line::from(vec![Span::styled(hint, style(Role::Muted))])),
            Rect::new(layout.body.x, row, layout.body.width, 1),
        );
    }
    paint_error_slot(f, layout.error, session.error.as_deref());
    cursor
}

/// Dispatch empty versus document paint. Returns the terminal cursor
/// cell when a focused input owns it (editor, chat box, or prompt).
pub fn paint(
    f: &mut Frame,
    area: Rect,
    session: &mut WriterSession,
    activity: crate::session::Activity,
    term_cols: u16,
) -> Option<ratatui::layout::Position> {
    if session.doc.is_none() {
        return paint_empty(f, area, session);
    }
    paint_doc(f, area, session, activity, term_cols)
}

/// Full L2 document view: title, editor+gutter | assistant panel,
/// chat box, status, actions, error slot.
fn paint_doc(
    f: &mut Frame,
    area: Rect,
    session: &mut WriterSession,
    activity: crate::session::Activity,
    term_cols: u16,
) -> Option<ratatui::layout::Position> {
    use edtui::{EditorTheme, EditorView, Highlight};

    let layout = writer_layout(area);
    let frame = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(style(Role::BorderFocused));
    f.render_widget(frame, area);
    let doc = session.doc.as_ref().expect("checked above");
    let title = if doc.dirty {
        format!("{} ●", doc.path_rel)
    } else {
        doc.path_rel.clone()
    };
    f.render_widget(
        Paragraph::new(Line::from(vec![Span::styled(title, style(Role::Text))])),
        layout.title,
    );
    let buffer = doc.text.clone();
    let rev = doc.revision;
    let dirty = doc.dirty;
    let editor = session.editor.as_mut().expect("editor built on open");
    // Fresh highlights every frame: stale ranges must never linger.
    editor.clear_highlights();
    let mut mark_rows: Vec<usize> = Vec::new();
    let mut mark_range: std::ops::Range<usize> = 0..0;
    if let Some(selected) = session.selected_proposal {
        if let Some(proposal) = session.proposals.get(selected) {
            if proposal.range.end > proposal.range.start {
                let start = row_of(&buffer, proposal.range.start);
                let end = row_of(&buffer, proposal.range.end.saturating_sub(1));
                mark_rows = (start..=end).collect();
                mark_range = proposal.range.clone();
                editor.add_highlight(Highlight::new(
                    crate::app::writer::adapter::offset_to_index2(&buffer, proposal.range.start),
                    crate::app::writer::adapter::offset_to_index2(
                        &buffer,
                        proposal.range.end.saturating_sub(1),
                    ),
                    // Accent fill, the same role the pills emphasize
                    // with: the target reads as selected, never
                    // color-alone beside the ▌ gutter marks.
                    style(Role::TabActive),
                ));
            }
        }
    }
    let theme = EditorTheme::default()
        .base(style(Role::Text))
        .selection_style(style(Role::Focus))
        .hide_status_line();
    f.render_widget(
        EditorView::new(editor).wrap(true).theme(theme),
        layout.editor,
    );
    // Page keys move by the last painted editor height; the adapter
    // cannot see the viewport, so the paint layer reports it here.
    session.editor_rows = layout.editor.height;
    // Wrap-exact gutter mapping, anchored on the cursor: screen rows
    // of every doc row are counted from the cursor with the same
    // greedy wrap EdTUI's LineWrapper uses, so wrapped rows can never
    // desync the marks. Every highlighted screen row gets a mark.
    let mut cursor = None;
    if let Some(pos) = session
        .editor
        .as_ref()
        .expect("rendered above")
        .cursor_screen_position()
    {
        let width = layout.editor.width.max(1) as usize;
        let rel = pos.y.saturating_sub(layout.editor.y) as isize;
        let cursor_row = row_of(
            &buffer,
            crate::app::writer::adapter::editor_cursor_offset(
                session.editor.as_ref().expect("rendered above"),
            ),
        );
        let doc_rows: Vec<&str> = buffer.split('\n').collect();
        // Screen-row prefix of doc row r: wrapped rows above it.
        let prefix = |r: usize| {
            doc_rows
                .iter()
                .take(r)
                .map(|line| wrapped_height(line, width) as isize)
                .sum::<isize>()
        };
        let base = prefix(cursor_row);
        // The range's first/last (row, col): partial rows only mark
        // the wrapped chunks the range touches.
        let (first_row, first_col) = range_mark_start(&buffer, &mark_range);
        let (last_row, last_col) = range_mark_end(&buffer, &mark_range);
        for row in mark_rows {
            let height = wrapped_height(doc_rows[row], width);
            let last_chunk = height.saturating_sub(1);
            let (from, to) = if row == first_row && row == last_row {
                (
                    chunk_of(doc_rows[row], first_col, width),
                    chunk_of(doc_rows[row], last_col, width),
                )
            } else if row == first_row {
                (chunk_of(doc_rows[row], first_col, width), last_chunk)
            } else if row == last_row {
                (0, chunk_of(doc_rows[row], last_col, width))
            } else {
                (0, last_chunk)
            };
            for k in from..=to.min(last_chunk) {
                let y =
                    layout.editor.y as isize + rel + (prefix(row) as isize + k as isize - base);
                if y >= layout.editor.y as isize
                    && y < (layout.editor.y + layout.editor.height) as isize
                    && layout.gutter.width > 0
                {
                    f.render_widget(
                        Paragraph::new(Line::from(vec![Span::styled(
                            "▌",
                            style(Role::Brand),
                        )])),
                        Rect::new(layout.gutter.x, y as u16, 1, 1),
                    );
                }
            }
        }
        if session.focus == WriterFocus::Editor {
            cursor = Some(pos);
        }
    }
    if panel_collapsed(term_cols) {
        paint_notice(f, layout.panel);
    } else {
        paint_panel(f, layout.panel, session, &buffer);
    }
    if session.focus == WriterFocus::Chat {
        cursor = paint_chat(f, layout.chat, session).or(cursor);
    } else {
        paint_chat(f, layout.chat, session);
    }
    paint_status(
        f,
        layout.status,
        session,
        &buffer,
        rev,
        dirty,
        activity,
    );
    paint_actions(f, layout.action);
    paint_error_slot(f, layout.error, session.error.as_deref());
    cursor
}

pub(super) fn row_of(text: &str, offset: usize) -> usize {
    text.chars().take(offset.min(text.chars().count())).filter(|&c| c == '\n').count()
}

/// Column (in chars) of an offset within its doc row.
fn col_of(text: &str, offset: usize) -> usize {
    let offset = offset.min(text.chars().count());
    let mut start = 0usize;
    for (i, c) in text.chars().enumerate() {
        if i >= offset {
            break;
        }
        if c == '\n' {
            start = i + 1;
        }
    }
    offset - start
}

/// First (row, col) touched by a proposal range: the range start.
fn range_mark_start(text: &str, range: &std::ops::Range<usize>) -> (usize, usize) {
    (row_of(text, range.start), col_of(text, range.start))
}

/// Last (row, col) touched: the final char of a non-empty range.
fn range_mark_end(text: &str, range: &std::ops::Range<usize>) -> (usize, usize) {
    let end = range.end.saturating_sub(1);
    (row_of(text, end), col_of(text, end))
}

/// EdTUI's tab stop: its view state default, which we never change.
const WRAP_TAB_WIDTH: usize = 2;

/// Cell width of one char under EdTUI's wrap, mirroring its
/// `LineWrapper` greedy fill (`helper::char_width`).
fn wrap_cell_width(ch: char) -> usize {
    use unicode_width::UnicodeWidthChar;
    if ch == '\t' {
        WRAP_TAB_WIDTH
    } else {
        ch.width().unwrap_or(0)
    }
}

/// Screen rows one doc line occupies at `width` cells: the chunk
/// count of EdTUI's `wrap_line` (empty lines still take one row).
fn wrapped_height(line: &str, width: usize) -> usize {
    let width = width.max(1);
    let mut rows = 0usize;
    let mut used = 0usize;
    let mut chars = 0usize;
    for ch in line.chars() {
        chars += 1;
        let cw = wrap_cell_width(ch);
        if used + cw > width {
            rows += 1;
            used = 0;
        }
        used += cw;
    }
    if chars > 0 {
        rows += 1;
    }
    rows.max(1)
}

/// Which wrapped chunk holds the char at column `col`: the split
/// count EdTUI's `wrap_line` would have emitted before it.
fn chunk_of(line: &str, col: usize, width: usize) -> usize {
    let width = width.max(1);
    let mut chunk = 0usize;
    let mut used = 0usize;
    for (i, ch) in line.chars().enumerate() {
        if i >= col {
            break;
        }
        let cw = wrap_cell_width(ch);
        if used + cw > width {
            chunk += 1;
            used = 0;
        }
        used += cw;
    }
    chunk
}
