//! Writer document paint: the L2 document view with the EdTUI
//! editor plus the proposal gutter, the toolbar, and the Save-as
//! overlay. The empty state lives in `empty`; shared slot painters in
//! `toolbar` and `thread`.

use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};
use ratatui::Frame;

use super::empty::{paint_empty, paint_prompt_block};
use super::layout::{
    more_menu_rect, panel_collapsed, pill_spans, prompt_suggestions, toolbar_narrow,
    toolbar_pill_rects, writer_layout, ToolbarButton,
};
use super::thread::{
    paint_chat, paint_error_slot, paint_notice, paint_panel, paint_status,
};
use super::toolbar::{paint_confirm_slot, paint_more_menu, paint_toolbar};
use crate::app::writer::{WriterFocus, WriterSession};
use crate::ui::theme::{style, Role};

/// Rounded chrome frame shared by the empty and document states.
pub(super) fn paint_frame(f: &mut Frame, area: Rect) {
    let frame = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(style(Role::BorderFocused));
    f.render_widget(frame, area);
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
        return paint_empty(f, area, session, term_cols);
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

    let layout = writer_layout(area, session.panel_visible);
    paint_frame(f, area);
    let buffer = session.doc.as_ref().expect("checked above").text.clone();
    let rev = session.doc.as_ref().expect("checked above").revision;
    let dirty = session.doc.as_ref().expect("checked above").dirty;
    let path_rel = session
        .doc
        .as_ref()
        .expect("checked above")
        .path_rel
        .clone();
    // Toolbar row 1 with the file name trailing, like the empty state.
    paint_toolbar(f, layout.title, session, term_cols, Some((&path_rel, dirty)));
    // The document head matches the empty state: a rule under the
    // toolbar plus one row of top padding, then the editor.
    super::toolbar::paint_separator(f, layout.editor, layout.body.y);
    // Narrow-mode menu floats over the separator and body head.
    let narrow = toolbar_narrow(term_cols);
    let more = toolbar_pill_rects(layout.title, narrow, session)
        .into_iter()
        .find(|(_, b)| *b == ToolbarButton::More)
        .map(|(r, _)| r)
        .unwrap_or_default();
    paint_more_menu(f, more, more_menu_rect(more), session);
    // The prompt overlays the editor head below the rule (any kind:
    // New/Open prompted from the toolbar land here too). The editor
    // shrinks below it and reports the shrunk rows/cols, so paging
    // and wrapping count what is actually on screen.
    let head_y = layout.body.y.saturating_add(super::layout::DOC_PROMPT_OFF);
    let mut prompt_cursor = None;
    let mut edit_rect = Rect::new(
        layout.editor.x,
        head_y,
        layout.editor.width,
        layout.editor.height.saturating_sub(2),
    );
    let mut gut_rect = Rect::new(
        layout.gutter.x,
        head_y,
        layout.gutter.width,
        layout.gutter.height.saturating_sub(2),
    );
    if let Some(prompt) = session.open_prompt.clone() {
        let sugg = prompt_suggestions(session, &prompt.buffer);
        let shift = (2 + sugg.len() as u16 + 2).min(edit_rect.height);
        let (_, pos) = paint_prompt_block(
            f,
            layout.editor.x,
            layout.editor.width,
            head_y,
            session,
            &prompt,
        );
        prompt_cursor = pos;
        edit_rect = Rect::new(
            layout.editor.x,
            layout.editor.y.saturating_add(shift),
            layout.editor.width,
            layout.editor.height.saturating_sub(shift),
        );
        gut_rect = Rect::new(
            layout.gutter.x,
            layout.gutter.y.saturating_add(shift),
            layout.gutter.width,
            layout.gutter.height.saturating_sub(shift),
        );
    }
    // Find matches refresh from the doc revision before any
    // highlight reads them; the borrow ends before the editor's.
    crate::app::writer::adapter::keys::find::refresh_find_matches(session);
    // Marker cache follows the revision too: one O(n) parse per
    // edit, never per frame on a static buffer.
    crate::app::writer::markers::refresh_markers(session, &buffer, rev);
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
    // Find matches highlight under the proposal marks: every hit
    // in Warning, the current one reversed too, so it reads without
    // color. The editor selection (the current match) still wins:
    // EdTUI gives the selection priority over every highlight.
    if let Some(find) = session.find.as_ref() {
        for (index, range) in find.matches.iter().enumerate() {
            let mut mark = style(Role::Warning);
            if index == find.current {
                mark = mark.add_modifier(ratatui::style::Modifier::REVERSED);
            }
            editor.add_highlight(Highlight::new(
                crate::app::writer::adapter::offset_to_index2(&buffer, range.start),
                crate::app::writer::adapter::offset_to_index2(
                    &buffer,
                    range.end.saturating_sub(1).max(range.start),
                ),
                mark,
            ));
        }
    }
    // Marker part styles sit between find and markdown: proposal
    // and find win ties, markers sit on top of the E4 scanner (the
    // first-added highlight wins each cell).
    for mark in crate::app::writer::markers::marker_highlights(&buffer, &session.markers) {
        editor.add_highlight(mark);
    }
    // Markdown highlights for the visible window plus one row of
    // lookahead: fence parity comes from the session cache (valid
    // above the first edited line, extended forward on demand), so
    // a paint costs only the visible lines — see the typing-at-end
    // perf test in markdown.rs. Proposal marks were added above;
    // EdTUI gives the selection priority over every highlight.
    let first_visible = editor.viewport_offset().1;
    let window_rows = edit_rect.height as usize + 1;
    let md_marks = {
        let cache = &mut session.fence_cache;
        let dirty = session.fence_dirty_from.take();
        let through = first_visible.saturating_add(window_rows);
        crate::app::writer::markdown::fence_cover(&buffer, cache, rev, dirty, through);
        let fence_at_first = cache.starts.get(first_visible).copied().flatten();
        crate::app::writer::markdown::highlight_window(&buffer, first_visible, window_rows, fence_at_first)
    };
    for mark in md_marks {
        editor.add_highlight(mark);
    }
    // Every EdTUI style maps to a semantic role: the defaults hide
    // hard-coded RGB (white-on-black cursor, yellow selection, gray
    // line numbers) that no theme remap could reach.
    let theme = EditorTheme::default()
        .base(style(Role::Text))
        .cursor_style(
            style(Role::Text).add_modifier(ratatui::style::Modifier::REVERSED),
        )
        .selection_style(style(Role::Focus))
        .line_numbers_style(style(Role::Muted))
        .hide_status_line();
    // Absolute numbers take the same gutter EdTUI reserves: digits
    // of the row count plus one. The text width below subtracts it,
    // so wrap math, paging and clicks stay exact while numbered.
    let number_width = if session.line_numbers {
        (editor.lines.len().max(1).to_string().len() + 1) as u16
    } else {
        0
    };
    let text_cols = edit_rect.width.saturating_sub(number_width);
    if session.preview {
        // Read-only render: the editor keeps its cursor and scroll
        // underneath, and proposal gutter marks stay off (their
        // ranges are source rows, which preview reflows).
        let rendered = super::preview_text(&buffer);
        f.render_widget(
            Paragraph::new(rendered)
                .wrap(ratatui::widgets::Wrap { trim: false })
                .scroll((session.preview_scroll, 0)),
            edit_rect,
        );
    } else {
        f.render_widget(
            EditorView::new(editor)
                .wrap(true)
                .theme(theme)
                .line_numbers(if session.line_numbers {
                    edtui::LineNumbers::Absolute
                } else {
                    edtui::LineNumbers::None
                }),
            edit_rect,
        );
    }
    // Page keys move by the last painted editor height, vertical
    // moves wrap by its width; the adapter cannot see the viewport,
    // so the paint layer reports both here (shrunk while the prompt
    // overlays the editor head, narrowed by the number gutter).
    session.editor_rows = edit_rect.height;
    session.editor_cols = text_cols;
    // Wrap-exact gutter mapping, anchored on the cursor: screen rows
    // of every doc row are counted from the cursor with the same
    // greedy wrap EdTUI's LineWrapper uses, so wrapped rows can never
    // desync the marks. Every highlighted screen row gets a mark.
    let mut cursor = None;
    // Preview shows no editor cursor and no gutter marks: the
    // ranges are source rows, which the render reflows.
    if !session.preview {
        if let Some(pos) = session
            .editor
            .as_ref()
            .expect("rendered above")
            .cursor_screen_position()
        {
        let width = text_cols.max(1) as usize;
        let rel = pos.y.saturating_sub(edit_rect.y) as isize;
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
                    edit_rect.y as isize + rel + (prefix(row) as isize + k as isize - base);
                if y >= edit_rect.y as isize
                    && y < (edit_rect.y + edit_rect.height) as isize
                    && gut_rect.width > 0
                {
                    f.render_widget(
                        Paragraph::new(Line::from(vec![Span::styled(
                            "▌",
                            style(Role::Brand),
                        )])),
                        Rect::new(gut_rect.x, y as u16, 1, 1),
                    );
                }
            }
        }
        // Marker errors mark every wrapped chunk of their rows with
        // `✕`: painted after the `▌` marks, so an error row reads as
        // an error even under a proposal selection. The glyph (never
        // color alone) carries the meaning, like `▌` does.
        for row in crate::app::writer::markers::error_rows(&buffer, &session.markers) {
            let height = wrapped_height(doc_rows[row], width);
            for k in 0..=height.saturating_sub(1) {
                let y =
                    edit_rect.y as isize + rel + (prefix(row) as isize + k as isize - base);
                if y >= edit_rect.y as isize
                    && y < (edit_rect.y + edit_rect.height) as isize
                    && gut_rect.width > 0
                {
                    f.render_widget(
                        Paragraph::new(Line::from(vec![Span::styled(
                            "✕",
                            style(Role::Danger),
                        )])),
                        Rect::new(gut_rect.x, y as u16, 1, 1),
                    );
                }
            }
        }
        // The run's started markers spin every wrapped chunk of
        // their rows with `⟳`, after the error marks: a started
        // marker parsed cleanly, so the two never share a row.
        for row in crate::app::writer::markers::process_spin_rows(&buffer, session) {
            let height = wrapped_height(doc_rows[row], width);
            for k in 0..=height.saturating_sub(1) {
                let y =
                    edit_rect.y as isize + rel + (prefix(row) as isize + k as isize - base);
                if y >= edit_rect.y as isize
                    && y < (edit_rect.y + edit_rect.height) as isize
                    && gut_rect.width > 0
                {
                    f.render_widget(
                        Paragraph::new(Line::from(vec![Span::styled(
                            "⟳",
                            style(Role::Info),
                        )])),
                        Rect::new(gut_rect.x, y as u16, 1, 1),
                    );
                }
            }
        }
        if session.focus == WriterFocus::Editor {
            cursor = Some(pos);
        }
        }
    }
    // The panel, chat box, and action row exist only while the
    // assistant is visible; the status row and error slot stay.
    if session.panel_visible {
        super::toolbar::paint_divider(f, layout.panel.x, layout.body.y, layout.body.height);
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
        paint_rephrase(f, layout.action);
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
    // The prompt owns the cursor while it overlays the editor.
    if prompt_cursor.is_some() {
        cursor = prompt_cursor;
    }
    // The marker error under the cursor names itself in the
    // fixed slot: actionable confirms, the find bar, and real
    // errors all outrank it.
    let marker_reason = session.editor.as_ref().and_then(|editor| {
        let off = crate::app::writer::adapter::editor_cursor_offset(editor);
        crate::app::writer::markers::error_at(&session.markers, off)
            .map(|e| crate::app::writer::markers::error_reason(e.kind).to_string())
    });
    // Fixed-slot priority, defined: an actionable confirm wins,
    // then the find bar, then errors, then marker reasons, then
    // watch notices.
    match session.pending_confirm.as_ref() {
        Some(confirm) => paint_confirm_slot(f, layout.error, confirm),
        None if session.find.is_some() => {
            // The find bar owns the fixed error slot while open, so
            // the layout never shifts; its cursor wins like the
            // prompt cursor above.
            if let Some(pos) = super::paint_find_bar(f, layout.error, session) {
                cursor = Some(pos);
            }
        }
        None => match session.error.as_deref() {
            Some(_) => paint_error_slot(f, layout.error, session.error.as_deref()),
            None => match marker_reason.as_deref() {
                Some(_) => paint_error_slot(f, layout.error, marker_reason.as_deref()),
                None => {
                    super::thread::paint_banner_slot(f, layout.error, session.banner.as_deref())
                }
            },
        },
    }
    cursor
}

/// Action row, panel-visible only: `(Rephrase)` alone. Save moved to
/// the toolbar (E2); the row vanishes with the panel, giving the
/// editor its row back.
fn paint_rephrase(f: &mut Frame, area: Rect) {
    if area.height == 0 {
        return;
    }
    let rephrase = super::layout::action_pill_rect(area);
    f.render_widget(
        Paragraph::new(Line::from(pill_spans("Rephrase", true, None))),
        rephrase,
    );
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

/// Wrap math lives in the adapter (`nav`): the key layer's vertical
/// moves count the same screen rows the gutter marks, so the two can
/// never desync.
use crate::app::writer::adapter::nav::{chunk_of, wrapped_height};
