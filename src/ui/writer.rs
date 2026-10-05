//! Writer overlay paint: L2 layout, empty state, editor, panels.
//!
//! Geometry lives in [`writer_layout`] so the TUI mouse dispatch can
//! hit-test the exact rects the render paints. Every conditional row
//! (prompt, error) owns a fixed slot, so state changes never shove
//! surrounding content around.

use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};
use ratatui::Frame;

use crate::app::writer::{WriterFocus, WriterSession};
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
/// width in, since the sidebar eats content columns first).
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
/// `default_mark` draws the `*` the wireframe pins on `(*New…)`.
fn pill_spans(label: &str, emphasized: bool, default_mark: bool) -> Vec<Span<'static>> {
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

/// Paint the empty state: title plus the `(*New document)` /
/// `(Open…)` pills, the typed-path prompt when open, and the fixed
/// error slot. Returns the prompt cursor cell while typing.
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

/// Doc row (0-based) holding a char offset.
fn row_of(text: &str, offset: usize) -> usize {
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

/// Fixed one-line narrow notice in place of the panel.
fn paint_notice(f: &mut Frame, area: Rect) {
    f.render_widget(
        Paragraph::new(Line::from(vec![Span::styled(
            "widen to ≥100 columns",
            style(Role::Warning),
        )])),
        area,
    );
}

/// Click target on one panel row: pills fire, any other row of a
/// proposal entry selects it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelClick {
    Select(u64),
    Accept(u64),
    Reject(u64),
}

/// One assistant-panel row with its click target. Built once per
/// paint AND per mouse event from the same function, so hit areas
/// can never desync from what is on screen.
pub struct PanelRow<'a> {
    pub line: Line<'a>,
    pub click: Option<PanelClick>,
}

fn diff_rows(
    rows: &mut Vec<PanelRow<'_>>,
    proposal: &crate::writer::proposal::Proposal,
    width: usize,
    selected: bool,
) {
    let head = if selected { "> " } else { "  " };
    rows.push(PanelRow {
        line: Line::from(vec![Span::styled(
            format!(
                "{head}P{} chars {}-{}",
                proposal.id, proposal.range.start, proposal.range.end
            ),
            if selected {
                style(Role::Focus)
            } else {
                style(Role::Muted)
            },
        )]),
        click: Some(PanelClick::Select(proposal.id)),
    });
    for row in proposal.original.split('\n') {
        rows.push(PanelRow {
            line: Line::from(vec![
                Span::styled("- ".to_string(), style(Role::Danger)),
                Span::styled(truncate(row, width), style(Role::Text)),
            ]),
            click: Some(PanelClick::Select(proposal.id)),
        });
    }
    for row in proposal.text.split('\n') {
        rows.push(PanelRow {
            line: Line::from(vec![
                Span::styled("+ ".to_string(), style(Role::Success)),
                Span::styled(truncate(row, width), style(Role::Text)),
            ]),
            click: Some(PanelClick::Select(proposal.id)),
        });
    }
}

pub fn panel_rows(session: &WriterSession, width: usize) -> Vec<PanelRow<'_>> {
    let mut rows: Vec<PanelRow<'_>> = Vec::new();
    for proposal in session.proposals.pending() {
        let selected = session.selected_proposal == Some(proposal.id);
        diff_rows(&mut rows, proposal, width, selected);
        if let Some(note) = proposal.note.as_ref() {
            rows.push(PanelRow {
                line: Line::from(vec![Span::styled(
                    truncate(&format!("note: {note}"), width),
                    style(Role::Muted),
                )]),
                click: Some(PanelClick::Select(proposal.id)),
            });
        }
        rows.push(PanelRow {
            line: pill_line(&[("Accept", true, true), ("Reject", false, false)]),
            click: None,
        });
    }
    // A selected stale proposal stays visible with its diff, but its
    // pills are gone: Accept is disabled once the text moved.
    if let Some(selected) = session.selected_proposal {
        if let Some(proposal) = session.proposals.get(selected) {
            if proposal.state == crate::writer::proposal::ProposalState::Stale {
                diff_rows(&mut rows, proposal, width, true);
                rows.push(PanelRow {
                    line: Line::from(vec![Span::styled(
                        "stale: text changed",
                        style(Role::Warning),
                    )]),
                    click: Some(PanelClick::Select(proposal.id)),
                });
            }
        }
    }
    // Recent answers first-glance last: at most 8, Markdown-skinned.
    for entry in session.thread.iter().rev().take(8).rev() {
        rows.push(PanelRow {
            line: Line::from(vec![Span::styled(
                format!("A{}:", entry.request_id),
                style(Role::Brand),
            )]),
            click: None,
        });
        for text_line in crate::walkthrough::highlight::md_text(&entry.answer).lines {
            rows.push(PanelRow {
                line: truncate_line(text_line, width),
                click: None,
            });
        }
    }
    if rows.is_empty() {
        rows.push(PanelRow {
            line: Line::from(vec![Span::styled(
                "proposals and answers land here",
                style(Role::Muted),
            )]),
            click: None,
        });
    }
    rows
}

/// Rows dropped off the top when the panel overflows: bottom-anchored.
pub fn panel_skip(rows: usize, height: usize) -> usize {
    rows.saturating_sub(height)
}

/// Assistant panel: pending proposals with diff rows, note, and pills,
/// then recent thread answers as Markdown. Bottom-anchored; oldest
/// rows drop when the panel overflows.
fn paint_panel(f: &mut Frame, area: Rect, session: &WriterSession, buffer: &str) {
    let _ = buffer;
    let rows = panel_rows(session, area.width as usize);
    let skip = panel_skip(rows.len(), area.height as usize);
    f.render_widget(
        Paragraph::new(rows.into_iter().skip(skip).map(|row| row.line).collect::<Vec<_>>()),
        area,
    );
}

/// Chat box: selection chip row, `> ` input row, hint row. Returns the
/// input cursor cell.
fn paint_chat(
    f: &mut Frame,
    area: Rect,
    session: &WriterSession,
) -> Option<ratatui::layout::Position> {
    let chip = match session.selection.as_ref() {
        Some(range) => format!("[{}–{}] (✕)", range.start, range.end),
        None => "no selection".to_string(),
    };
    f.render_widget(
        Paragraph::new(Line::from(vec![Span::styled(chip, style(Role::Info))])),
        Rect::new(area.x, area.y, area.width, 1),
    );
    let input: String = session
        .chat_input
        .chars()
        .take((area.width as usize).saturating_sub(2))
        .collect();
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("> ".to_string(), style(Role::Brand)),
            Span::styled(input.clone(), style(Role::Text)),
        ])),
        Rect::new(area.x, area.y.saturating_add(1), area.width, 1),
    );
    let hint = if area.width >= 60 {
        "Enter sends · Rephrase uses the selection or paragraph"
    } else {
        "Enter sends"
    };
    f.render_widget(
        Paragraph::new(Line::from(vec![Span::styled(hint, style(Role::Muted))])),
        Rect::new(area.x, area.y.saturating_add(2), area.width, 1),
    );
    if session.focus != WriterFocus::Chat {
        return None;
    }
    let before: String = session
        .chat_input
        .chars()
        .take(session.chat_cursor)
        .collect();
    use ratatui::text::Line as TextLine;
    let dx = TextLine::from(format!("> {before}")).width() as u16;
    Some(ratatui::layout::Position::new(
        area.x.saturating_add(dx.min(area.width.saturating_sub(1))),
        area.y.saturating_add(1),
    ))
}

/// Status row: revision, 1-based line:col, agent activity word, and
/// the word `unsaved` while dirty.
fn paint_status(
    f: &mut Frame,
    area: Rect,
    session: &WriterSession,
    buffer: &str,
    rev: u64,
    dirty: bool,
    activity: crate::session::Activity,
) {
    let (line, col) = session
        .editor
        .as_ref()
        .map(|editor| {
            let off = crate::app::writer::adapter::editor_cursor_offset(editor);
            (row_of(buffer, off) + 1, off - line_start(buffer, off) + 1)
        })
        .unwrap_or((1, 1));
    let mut text = format!(
        "rev {rev} · {line}:{col} · {}",
        crate::comms::Broker::activity_label(activity)
    );
    if dirty {
        text.push_str(" · unsaved");
    }
    f.render_widget(
        Paragraph::new(Line::from(vec![Span::styled(text, style(Role::Muted))])),
        area,
    );
}

/// Start offset of the line holding `offset`.
fn line_start(text: &str, offset: usize) -> usize {
    let mut start = 0;
    for (index, c) in text.chars().enumerate() {
        if index >= offset {
            break;
        }
        if c == '\n' {
            start = index + 1;
        }
    }
    start
}

/// Action row: `(Rephrase)` carries the emphasis, `(Save)` rests.
fn paint_actions(f: &mut Frame, area: Rect) {
    let (rephrase, save) = action_pill_rects(area);
    f.render_widget(
        Paragraph::new(Line::from(pill_spans("Rephrase", true, false))),
        rephrase,
    );
    if save.x < area.x.saturating_add(area.width) {
        f.render_widget(
            Paragraph::new(Line::from(pill_spans("Save", false, false))),
            save,
        );
    }
}

/// One row of pills separated by two spaces.
fn pill_line(buttons: &[(&str, bool, bool)]) -> Line<'static> {
    let mut spans = Vec::new();
    for (index, (label, emphasized, default_mark)) in buttons.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled("  ".to_string(), Style::default()));
        }
        spans.extend(pill_spans(label, *emphasized, *default_mark));
    }
    Line::from(spans)
}

/// Truncate a row to the measured width (char count approximates the
/// prose cells the panel holds).
fn truncate(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    text.chars().take(width.saturating_sub(1)).collect::<String>() + "…"
}

fn truncate_line(line: Line<'_>, width: usize) -> Line<'_> {
    let mut kept = 0usize;
    let mut spans = Vec::new();
    for span in line.spans {
        let take = width.saturating_sub(kept);
        let text: String = span.content.chars().take(take).collect();
        kept += text.chars().count();
        spans.push(Span::styled(text, span.style));
        if kept >= width {
            break;
        }
    }
    Line::from(spans)
}

/// Fixed one-row error slot: the message or nothing, never shifting
/// the rows around it.
fn paint_error_slot(f: &mut Frame, area: Rect, error: Option<&str>) {
    let line = match error {
        Some(message) => Line::from(vec![Span::styled(message.to_string(), style(Role::Danger))]),
        None => Line::from(vec![Span::styled(String::new(), Style::default())]),
    };
    f.render_widget(Paragraph::new(line), area);
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

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    fn paint_empty_to(
        session: &WriterSession,
        w: u16,
        h: u16,
    ) -> ratatui::buffer::Buffer {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal
            .draw(|f| {
                paint_empty(f, f.area(), session);
            })
            .unwrap();
        terminal.backend().buffer().clone()
    }

    fn row_text(buf: &ratatui::buffer::Buffer, y: u16, x0: u16, x1: u16) -> String {
        (x0..x1.min(buf.area.width))
            .map(|x| buf[(x, y)].symbol().to_string())
            .collect()
    }

    fn doc_session(text: &str) -> WriterSession {
        use edtui::{EditorMode, EditorState, Lines};
        let mut session = WriterSession::default();
        session.doc = Some(crate::writer::document::Document {
            path_rel: "d.md".to_string(),
            abs_path: std::path::PathBuf::from("/tmp/d.md"),
            text: text.to_string(),
            revision: 0,
            disk_hash: 0,
            dirty: false,
        });
        let mut editor = EditorState::new(Lines::from(text));
        editor.mode = EditorMode::Insert;
        editor.set_clipboard(session.clip.clone());
        session.editor = Some(editor);
        session
    }

    fn paint_doc_to(session: &mut WriterSession, w: u16, h: u16) -> ratatui::buffer::Buffer {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal
            .draw(|f| {
                paint(f, f.area(), session, crate::session::Activity::Idle, w);
            })
            .unwrap();
        terminal.backend().buffer().clone()
    }

    fn buffer_text(buf: &ratatui::buffer::Buffer) -> String {
        buf.content.iter().map(|cell| cell.symbol()).collect()
    }

    #[test]
    fn doc_paints_title_editor_status_and_actions() {
        let mut session = doc_session("aaa bbb");
        let buf = paint_doc_to(&mut session, 120, 30);
        let text = buffer_text(&buf);
        assert!(text.contains("d.md"), "title row: {text}");
        assert!(text.contains("aaa bbb"), "editor text");
        assert!(text.contains("rev 0"), "status revision");
        assert!(text.contains("1:1"), "status line:col");
        assert!(text.contains("Idle"), "agent activity word");
        assert!(!text.contains("unsaved"), "clean doc hides it");
        assert!(text.contains("Rephrase"), "action row");
        assert!(text.contains("Save"), "action row");
        assert!(text.contains("no selection"), "chat chip");
        assert!(text.contains("proposals and answers land here"), "panel placeholder");
    }

    #[test]
    fn dirty_doc_marks_title_and_status() {
        let mut session = doc_session("aaa");
        session
            .doc
            .as_mut()
            .unwrap()
            .apply_edit(0..3, "bbb")
            .unwrap();
        let buf = paint_doc_to(&mut session, 120, 30);
        let text = buffer_text(&buf);
        assert!(text.contains("d.md ●"), "title dot");
        assert!(text.contains("unsaved"), "status word");
        assert!(text.contains("rev 1"), "revision bumped");
    }

    #[test]
    fn selected_proposal_paints_diff_gutter_and_highlight() {
        use ratatui::style::Color;
        let mut session = doc_session("aaa bbb ccc");
        let pid = session
            .proposals
            .propose(
                session.doc.as_ref().unwrap(),
                None,
                4..7,
                "BBB".to_string(),
                Some("louder".to_string()),
            )
            .unwrap();
        session.selected_proposal = Some(pid);
        let buf = paint_doc_to(&mut session, 120, 30);
        let text = buffer_text(&buf);
        assert!(text.contains("- bbb"), "diff row: {text}");
        assert!(text.contains("+ BBB"), "diff row");
        assert!(text.contains("note: louder"), "note row");
        assert!(text.contains("Accept"), "accept pill");
        assert!(text.contains("Reject"), "reject pill");
        assert!(text.contains("> P"), "selected marker");
        // Single-line doc: the gutter mark sits on the editor's first
        // row, and the highlight fills exactly the target cells.
        assert_eq!(buf[(3, 3)].symbol(), "▌", "gutter mark");
        for x in [8, 9, 10] {
            assert_eq!(
                buf[(x, 3)].style().bg,
                Some(Color::Yellow),
                "highlight cell {x}"
            );
        }
        assert_ne!(buf[(4, 3)].style().bg, Some(Color::Yellow), "outside range");
    }

    #[test]
    fn stale_entry_shows_note_without_pills() {
        let mut session = doc_session("aaa bbb");
        let pid = session
            .proposals
            .propose(session.doc.as_ref().unwrap(), None, 0..3, "AAA".to_string(), None)
            .unwrap();
        session.selected_proposal = Some(pid);
        session.doc.as_mut().unwrap().apply_edit(0..3, "zzz").unwrap();
        session.proposals.on_edit(&(0..3));
        let buf = paint_doc_to(&mut session, 120, 30);
        let text = buffer_text(&buf);
        assert!(text.contains("stale: text changed"), "stale note: {text}");
        assert!(!text.contains("Accept"), "no accept affordance");
        assert!(!text.contains("Reject"), "no reject affordance");
    }

    #[test]
    fn answer_renders_markdown_in_the_thread() {
        let mut session = doc_session("aaa");
        session.thread.push(crate::app::writer::WriterThreadEntry {
            request_id: 1,
            answer: "# Why\n\nbecause reasons".to_string(),
        });
        let buf = paint_doc_to(&mut session, 120, 30);
        let text = buffer_text(&buf);
        assert!(text.contains("Why"), "heading: {text}");
        assert!(text.contains("because reasons"), "body");
        assert!(text.contains("A1:"), "answer header");
    }

    #[test]
    fn narrow_panel_shows_the_widen_notice() {
        let mut session = doc_session("aaa bbb");
        let buf = paint_doc_to(&mut session, 80, 24);
        let layout = writer_layout(Rect::new(0, 0, 80, 24));
        assert!(panel_collapsed(80));
        let row = row_text(&buf, layout.panel.y, layout.panel.x, layout.panel.x + 22);
        assert!(row.contains("widen to ≥100 columns"), "notice: {row:?}");
        assert!(buffer_text(&buf).contains("aaa bbb"), "editor keeps painting");
    }

    #[test]
    fn chat_chip_shows_the_live_selection() {
        let mut session = doc_session("aaa bbb");
        session.selection = Some(2..5);
        let buf = paint_doc_to(&mut session, 120, 30);
        assert!(buffer_text(&buf).contains("[2–5] (✕)"), "chip with detach");
    }

    #[test]
    fn layout_keeps_the_65_35_split_and_fixed_slots() {
        let area = Rect::new(0, 0, 120, 30);
        let layout = writer_layout(area);
        // Border 1 + side pad 2 on the left.
        assert_eq!((layout.title.x, layout.title.y), (3, 2));
        assert_eq!(layout.title.width, 114);
        // Editor takes 65% of the 114-wide content.
        assert_eq!(layout.editor.width, 114 * 65 / 100 - 1);
        assert_eq!(layout.gutter.width, 1);
        assert_eq!(
            layout.panel.width,
            114 - 114 * 65 / 100,
            "panel takes the rest"
        );
        // Fixed slots stack at the bottom: chat 3, status/action/error 1.
        assert_eq!(layout.chat.height, 3);
        assert_eq!(layout.status.height, 1);
        assert_eq!(layout.action.height, 1);
        assert_eq!(layout.error.height, 1);
        assert_eq!(
            layout.error.y, 27,
            "error owns the last content row (inner 2..28)"
        );
        assert!(panel_collapsed(80));
        assert!(!panel_collapsed(100));
        assert!(!panel_collapsed(120));
    }

    #[test]
    fn empty_state_paints_both_pills_and_title() {
        let session = WriterSession::default();
        let buf = paint_empty_to(&session, 120, 30);
        let text: String = buf
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(text.contains("Writer"), "title row");
        assert!(text.contains("*New document"), "default-marked pill");
        assert!(text.contains("Open…"), "open pill");
        // Rounded border corners, never blank panels.
        assert_eq!(buf[(0, 0)].symbol(), "╭");
        assert_eq!(buf[(119, 0)].symbol(), "╮");
    }

    #[test]
    fn rest_pill_caps_use_the_semantic_rest_cap() {
        // No new inline colours: rest caps come from the shared theme
        // helper, mirroring the topbar/dialog pills.
        let buf = paint_empty_to(&WriterSession::default(), 120, 30);
        let layout = writer_layout(Rect::new(0, 0, 120, 30));
        let (_, open_rect) = empty_pill_rects(layout.body);
        assert!(open_rect.height > 0, "open pill paints");
        let left_cap = buf[(open_rect.x, open_rect.y)].fg;
        let right_cap =
            buf[(open_rect.x + open_rect.width - 1, open_rect.y)].fg;
        let rest = crate::ui::theme::pill_rest_cap();
        assert_eq!(left_cap, rest, "left cap");
        assert_eq!(right_cap, rest, "right cap");
    }

    #[test]
    fn prompt_paints_with_title_and_buffer() {
        let mut session = WriterSession::default();
        session.open_prompt = Some(crate::app::writer::WriterOpenPrompt {
            buffer: "zz9".to_string(),
            create: false,
        });
        let buf = paint_empty_to(&session, 120, 30);
        let text: String = buf
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(text.contains("Open document — path:"), "prompt title");
        assert!(text.contains("zz9"), "buffer text");
    }

    #[test]
    fn error_slot_holds_the_fixed_row() {
        let mut session = WriterSession::default();
        session.error = Some("no live agent tab".to_string());
        let buf = paint_empty_to(&session, 120, 30);
        let layout = writer_layout(Rect::new(0, 0, 120, 30));
        let row = row_text(&buf, layout.error.y, layout.error.x, layout.error.x + 20);
        assert!(row.contains("no live agent tab"), "slot row: {row:?}");
        // Same row empty without an error: geometry never moves.
        let clean = paint_empty_to(&WriterSession::default(), 120, 30);
        let same = row_text(&clean, layout.error.y, layout.error.x, layout.error.x + 20);
        assert_eq!(same.trim(), "", "slot row: {same:?}");
    }
}
