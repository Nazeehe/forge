//! Writer mouse dispatch: toolbar pills, the More menu, confirm
//! pills, Start pills, prompt suggestions and buttons, recent rows,
//! thread-row clicks, and editor cursor/drag/wheel through the
//! adapter. Every hit area comes from the same layout the render
//! paints, so clicks can never desync; an open modal still swallows
//! everything before this runs.

use crossterm::event;

use crate::app::AppState;
use crate::ui::writer::ToolbarButton;

/// Multi-click window: presses on one cell this close chain into
/// word (second) and line (third) selection.
const DOUBLE_CLICK_MS: u128 = 500;

/// One main-area mouse event while the Writer overlay owns input.
/// Pill clicks fire, menu rows fire, recent rows open, the editor
/// takes cursor, drag, and wheel through the adapter (EdTUI ignores
/// events outside its painted area), and everything else dies here so
/// clicks never reach the agent pane behind the overlay.
pub(crate) fn handle_writer_mouse(state: &mut AppState, mev: event::MouseEvent) {
    use event::{MouseButton, MouseEventKind};
    use ratatui::layout::Rect;

    let Some(id) = state.writer_overlay_active() else {
        return;
    };
    let (rows, cols) = state.term_size;
    let content = crate::walkthrough::walk_area(Rect::new(0, 0, cols, rows));
    if mev.column < content.x
        || mev.column >= content.x.saturating_add(content.width)
        || mev.row < content.y
        || mev.row >= content.y.saturating_add(content.height)
    {
        return;
    }
    let panel_visible = state
        .writers
        .get(&id)
        .is_some_and(|session| session.panel_visible);
    let layout = crate::ui::writer::writer_layout(content, panel_visible);
    let narrow = crate::ui::writer::toolbar_narrow(cols);
    let has_doc = state
        .writers
        .get(&id)
        .is_some_and(|session| session.doc.is_some());
    match mev.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            // A new press ends any editor gesture first: only a
            // press ON the editor below re-arms it, so a drag that
            // started on chrome can never grow a selection.
            if let Some(session) = state.writers.get_mut(&id) {
                session.editor_gesture = false;
            }
            // The open More menu floats above everything: its rows
            // fire, the More pill toggles it shut, any other click
            // closes it and dies (menus swallow their dismissal).
            if narrow && state.writers.get(&id).is_some_and(|s| s.more_open) {
                if let Some(button) = menu_item_at(state, id, &layout, mev.column, mev.row) {
                    fire_menu_button(state, id, button);
                    return;
                }
                if toolbar_at(state, id, &layout, narrow, mev.column, mev.row)
                    == Some(ToolbarButton::More)
                {
                    state.writer_toggle_more(id);
                    return;
                }
                state.writer_toggle_more(id);
                return;
            }
            // Toolbar row 1 in both states; disabled pills ignore.
            if let Some(button) = toolbar_at(state, id, &layout, narrow, mev.column, mev.row) {
                fire_toolbar_button(state, id, button);
                return;
            }
            // Actionable error-slot row.
            if let Some(index) = confirm_at(state, id, &layout, mev.column, mev.row) {
                state.writer_fire_confirm(id, index);
                return;
            }
            // Find bar pills and fields in the fixed error slot (a
            // pending confirm paints over it, handled above).
            if state
                .writers
                .get(&id)
                .is_some_and(|s| s.find.is_some() && s.pending_confirm.is_none())
                && find_click(state, id, &layout, mev.column, mev.row)
            {
                return;
            }
            if !has_doc {
                empty_click(state, id, &layout, mev.column, mev.row);
                return;
            }
            // An open prompt overlays the editor head: suggestions
            // fill, buttons submit/cancel. The origin shares
            // DOC_PROMPT_OFF with the paint, never body.y.
            if state
                .writers
                .get(&id)
                .is_some_and(|s| s.open_prompt.is_some())
                && prompt_click(
                    state,
                    id,
                    layout.editor.x,
                    layout.body.y.saturating_add(crate::ui::writer::DOC_PROMPT_OFF),
                    layout.editor.width,
                    mev.column,
                    mev.row,
                )
            {
                return;
            }
            let rephrase = crate::ui::writer::action_pill_rect(layout.action);
            if hits(rephrase, mev.column, mev.row) {
                state.writer_rephrase(id);
                return;
            }
            let chip = crate::ui::writer::chip_detach_rect(
                layout.chat,
                state.writers.get(&id).and_then(|s| s.selection.as_ref()),
            );
            if chip.is_some_and(|rect| hits(rect, mev.column, mev.row)) {
                state.writer_clear_selection(id);
                return;
            }
            // Click-to-place in the chat input row (past the "> "
            // prefix). The chat box only paints while the panel shows.
            if state.writers.get(&id).is_some_and(|s| s.panel_visible)
                && mev.row == layout.chat.y.saturating_add(1)
                && mev.column >= layout.chat.x.saturating_add(2)
            {
                let input = state
                    .writers
                    .get(&id)
                    .map(|s| s.chat_input.clone())
                    .unwrap_or_default();
                let at = cell_to_char(&input, mev.column.saturating_sub(layout.chat.x.saturating_add(2)));
                state.writer_chat_place(id, at);
                return;
            }
            if let Some(click) = panel_click_at(state, id, &layout, mev.column, mev.row) {
                fire_panel_click(state, id, click);
                return;
            }
            // Preview is read-only: presses in the render move no
            // cursor and start no gesture (the Up arm drops without
            // one), but the event still dies here, never reaching
            // the agent pane behind the overlay.
            if hits(layout.editor, mev.column, mev.row)
                && !state.writers.get(&id).is_some_and(|s| s.preview)
            {
                // Multi-click chain on one cell: the second press
                // selects a word, the third a line, the fourth
                // starts over at a single click.
                let count = if let Some(session) = state.writers.get_mut(&id) {
                    let now = std::time::Instant::now();
                    let chained = session.last_press.is_some_and(|(x, y, at)| {
                        x == mev.column
                            && y == mev.row
                            && now.duration_since(at).as_millis() <= DOUBLE_CLICK_MS
                    });
                    let next = if chained { session.press_count + 1 } else { 1 };
                    let next = if next > 3 { 1 } else { next };
                    session.last_press = Some((mev.column, mev.row, now));
                    session.press_count = next;
                    session.editor_gesture = true;
                    next
                } else {
                    1
                };
                state.writer_feed_mouse(id, mev);
                match count {
                    2 => state.writer_select_word(id),
                    3 => {
                        state.writer_select_line(id);
                        if let Some(session) = state.writers.get_mut(&id) {
                            session.press_count = 0;
                        }
                    }
                    _ => {}
                }
            }
        }
        // Button-less motion is never a gesture: it dies here even
        // when the terminal reports it as Drag(Left).
        MouseEventKind::Moved => {}
        MouseEventKind::Drag(_) => {
            let gesture = state
                .writers
                .get(&id)
                .is_some_and(|s| s.editor_gesture);
            if gesture {
                // Autoscroll: past the top/bottom edge the viewport
                // moves a row and the edge-clamped drag extends the
                // selection. Still gated on the H1 press gesture, so
                // hover motion never scrolls or selects.
                let ed = layout.editor;
                if ed.width > 0 && ed.height > 0 {
                    let mut fixed = mev;
                    if mev.row < ed.y {
                        state.writer_scroll_editor(id, -1);
                        fixed.row = ed.y;
                    } else if mev.row >= ed.y.saturating_add(ed.height) {
                        state.writer_scroll_editor(id, 1);
                        fixed.row = ed.y.saturating_add(ed.height).saturating_sub(1);
                    }
                    fixed.column = fixed.column.clamp(
                        ed.x,
                        ed.x.saturating_add(ed.width).saturating_sub(1),
                    );
                    if hits(ed, fixed.column, fixed.row) {
                        state.writer_feed_mouse(id, fixed);
                    }
                }
            }
        }
        MouseEventKind::Up(_) => {
            // Up ends the gesture wherever it lands, so a release
            // off-editor cannot leave hover selecting afterwards.
            let gesture = state
                .writers
                .get_mut(&id)
                .map(|s| std::mem::replace(&mut s.editor_gesture, false))
                .unwrap_or(false);
            if gesture && hits(layout.editor, mev.column, mev.row) {
                state.writer_feed_mouse(id, mev);
            }
        }
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            if hits(layout.editor, mev.column, mev.row) {
                // Three rows a notch, mirroring the editor wheel: in
                // preview the render scrolls instead of the editor.
                if state.writers.get(&id).is_some_and(|s| s.preview) {
                    state.writer_preview_scroll(
                        id,
                        match mev.kind {
                            MouseEventKind::ScrollUp => -3,
                            _ => 3,
                        },
                    );
                } else {
                    state.writer_feed_mouse(id, mev);
                }
            }
        }
        _ => {}
    }
}

/// Toolbar pill under one cell, if any (enabled or not; firing
/// checks [`toolbar_enabled`](crate::ui::writer::toolbar_enabled)).
fn toolbar_at(
    state: &AppState,
    id: crate::session::SessionId,
    layout: &crate::ui::writer::WriterLayout,
    narrow: bool,
    x: u16,
    y: u16,
) -> Option<ToolbarButton> {
    let session = state.writers.get(&id)?;
    crate::ui::writer::toolbar_pill_rects(layout.title, narrow, session)
        .into_iter()
        .find(|(rect, _)| hits(*rect, x, y))
        .map(|(_, button)| button)
}

/// Fire one toolbar pill; disabled pills die silently.
fn fire_toolbar_button(state: &mut AppState, id: crate::session::SessionId, button: ToolbarButton) {
    let enabled = state
        .writers
        .get(&id)
        .is_some_and(|s| crate::ui::writer::toolbar_enabled(s, button));
    if !enabled {
        return;
    }
    match button {
        ToolbarButton::New => state.writer_toolbar_new(id),
        ToolbarButton::Open => state.writer_toolbar_open(id),
        ToolbarButton::Save => state.writer_toolbar_save(id),
        ToolbarButton::SaveAs => state.writer_toolbar_save_as(id),
        ToolbarButton::Close => state.writer_toolbar_close(id),
        ToolbarButton::Preview => state.writer_toolbar_preview(id),
        ToolbarButton::Assistant => state.writer_toolbar_assistant(id),
        ToolbarButton::LineNumbers => state.writer_toggle_line_numbers(id),
        ToolbarButton::More => state.writer_toggle_more(id),
    }
}

/// More-menu row under one cell, if any.
fn menu_item_at(
    state: &AppState,
    id: crate::session::SessionId,
    layout: &crate::ui::writer::WriterLayout,
    x: u16,
    y: u16,
) -> Option<ToolbarButton> {
    let session = state.writers.get(&id)?;
    let more = crate::ui::writer::toolbar_pill_rects(layout.title, true, session)
        .into_iter()
        .find(|(_, b)| *b == ToolbarButton::More)
        .map(|(r, _)| r)?;
    let menu = crate::ui::writer::more_menu_rect(more);
    crate::ui::writer::more_menu_item_rects(menu, session)
        .into_iter()
        .find(|(rect, _)| hits(*rect, x, y))
        .map(|(_, button)| button)
}

/// Fire one menu row and close the menu. Disabled rows close without
/// firing, like their toolbar twins.
fn fire_menu_button(state: &mut AppState, id: crate::session::SessionId, button: ToolbarButton) {
    state.writer_toggle_more(id);
    let enabled = state
        .writers
        .get(&id)
        .is_some_and(|s| crate::ui::writer::toolbar_enabled(s, button));
    if !enabled {
        return;
    }
    match button {
        ToolbarButton::SaveAs => state.writer_toolbar_save_as(id),
        ToolbarButton::Preview => state.writer_toolbar_preview(id),
        ToolbarButton::Assistant => state.writer_toolbar_assistant(id),
        ToolbarButton::LineNumbers => state.writer_toggle_line_numbers(id),
        _ => {}
    }
}

/// Confirm pill index under one cell, if a confirm is pending.
fn confirm_at(
    state: &AppState,
    id: crate::session::SessionId,
    layout: &crate::ui::writer::WriterLayout,
    x: u16,
    y: u16,
) -> Option<usize> {
    let session = state.writers.get(&id)?;
    let confirm = session.pending_confirm.as_ref()?;
    crate::ui::writer::confirm_pill_rects(layout.error, &confirm.message, &confirm.actions)
        .into_iter()
        .enumerate()
        .find(|(_, rect)| hits(*rect, x, y))
        .map(|(index, _)| index)
}

/// One empty-state click: prompt suggestions fill, prompt buttons
/// submit/cancel, Start pills open the prompt kinds, recent rows open.
fn empty_click(
    state: &mut AppState,
    id: crate::session::SessionId,
    layout: &crate::ui::writer::WriterLayout,
    x: u16,
    y: u16,
) {
    use crate::ui::writer::EMPTY_INDENT;
    let session = state.writers.get(&id);
    let panel_visible = session.is_some_and(|s| s.panel_visible);
    let col = if panel_visible { layout.editor } else { layout.body };
    let x0 = col.x.saturating_add(EMPTY_INDENT);
    let width = col.width.saturating_sub(EMPTY_INDENT);
    let prompt_open = state
        .writers
        .get(&id)
        .is_some_and(|s| s.open_prompt.is_some());
    if prompt_open {
        prompt_click(state, id, x0, prompt_origin(layout), width, x, y);
        return;
    }
    if hits(crate::ui::writer::start_new_rect(x0, layout.body.y.saturating_add(6)), x, y) {
        state.writer_toolbar_new(id);
        return;
    }
    if hits(crate::ui::writer::start_open_rect(x0, layout.body.y.saturating_add(7)), x, y) {
        state.writer_toolbar_open(id);
        return;
    }
    // Recent rows below the head; the window matches the paint.
    let (len, sel) = state
        .writers
        .get(&id)
        .map(|s| (s.recent_cache.len(), s.recent_sel))
        .unwrap_or((0, 0));
    if len == 0 {
        return;
    }
    let room = layout.body.height.saturating_sub(11).max(1) as usize;
    let (start, count) = crate::ui::writer::recent_window(len, sel, room);
    for j in 0..count {
        let row = layout.body.y.saturating_add(crate::ui::writer::RECENT_FIRST_ROW_OFF + j as u16);
        if hits(ratatui::layout::Rect::new(x0, row, width, 1), x, y) {
            state.writer_open_recent_at(id, start + j);
            return;
        }
    }
}

/// First prompt-block row in the empty state (Start sits at +5..+7
/// when closed; the prompt replaces it).
fn prompt_origin(layout: &crate::ui::writer::WriterLayout) -> u16 {
    layout.body.y.saturating_add(crate::ui::writer::PROMPT_ORIGIN_OFF)
}

/// One prompt-block click: a suggestion fills the buffer, the buttons
/// submit/cancel. True when a prompt row took the click.
fn prompt_click(
    state: &mut AppState,
    id: crate::session::SessionId,
    x: u16,
    origin_y: u16,
    width: u16,
    cx: u16,
    cy: u16,
) -> bool {
    let (buffer, kind) = match state.writers.get(&id).and_then(|s| s.open_prompt.as_ref()) {
        Some(prompt) => (prompt.buffer.clone(), prompt.kind),
        None => return false,
    };
    // Click-to-place in the input row (origin + 1, past the "> "
    // prefix): the cursor follows the clicked char.
    if cy == origin_y.saturating_add(1) && cx >= x.saturating_add(2) {
        let at = cell_to_char(&buffer, cx.saturating_sub(x.saturating_add(2)));
        state.writer_prompt_place(id, at);
        return true;
    }
    let suggestions = state
        .writers
        .get(&id)
        .map(|s| crate::ui::writer::prompt_suggestions(s, &buffer))
        .unwrap_or_default();
    for (index, rel) in suggestions.iter().enumerate() {
        if hits(crate::ui::writer::prompt_sugg_rect(x, origin_y, width, index), cx, cy) {
            state.writer_prompt_fill(id, rel.clone());
            return true;
        }
    }
    let buttons_y = origin_y.saturating_add(2 + suggestions.len() as u16);
    let (submit, cancel) = crate::ui::writer::prompt_button_rects(x, buttons_y, width, &kind);
    if hits(submit, cx, cy) {
        state.writer_submit_open(id);
        return true;
    }
    if hits(cancel, cx, cy) {
        state.writer_prompt_cancel(id);
        return true;
    }
    false
}

/// Char index under a click `dx` cells into a one-line input.
/// Wide glyphs count their cells, so CJK clicks land right.
fn cell_to_char(text: &str, dx: u16) -> usize {
    use unicode_width::UnicodeWidthChar;
    let mut cells = 0u16;
    for (index, c) in text.chars().enumerate() {
        cells += c.width().unwrap_or(0) as u16;
        if cells > dx {
            return index;
        }
    }
    text.chars().count()
}

/// One click on the find bar row: pills fire, fields take focus
/// (and Editor focus, so typing lands in the bar). True when a
/// control was hit; misses fall through to the handlers below.
fn find_click(
    state: &mut AppState,
    id: crate::session::SessionId,
    layout: &crate::ui::writer::WriterLayout,
    x: u16,
    y: u16,
) -> bool {
    let Some(session) = state.writers.get(&id) else {
        return false;
    };
    let Some(find) = session.find.as_ref() else {
        return false;
    };
    let rects = crate::ui::writer::find_bar_rects(layout.error, find);
    if hits(rects.case, x, y) {
        state.writer_find_toggle_case(id);
        return true;
    }
    if hits(rects.toggle, x, y) {
        state.writer_find_toggle_replace(id);
        return true;
    }
    if find.replace_open {
        if hits(rects.replace_next_btn, x, y) {
            state.writer_find_replace_current(id);
            return true;
        }
        if hits(rects.replace_all_btn, x, y) {
            state.writer_find_replace_all(id);
            return true;
        }
        if hits(rects.replace, x, y) {
            if let Some(session) = state.writers.get_mut(&id) {
                session.focus = crate::app::writer::WriterFocus::Editor;
            }
            state.writer_find_focus_replace(id);
            return true;
        }
    }
    if hits(rects.query, x, y) {
        if let Some(session) = state.writers.get_mut(&id) {
            session.focus = crate::app::writer::WriterFocus::Editor;
        }
        state.writer_find_focus_query(id);
        return true;
    }
    false
}

fn hits(area: ratatui::layout::Rect, x: u16, y: u16) -> bool {
    area.width > 0
        && area.height > 0
        && x >= area.x
        && x < area.x.saturating_add(area.width)
        && y >= area.y
        && y < area.y.saturating_add(area.height)
}

/// Panel click target under one cell, if any: pills resolve to
/// Accept/Reject by column, other entry rows to Select.
fn panel_click_at(
    state: &AppState,
    id: crate::session::SessionId,
    layout: &crate::ui::writer::WriterLayout,
    x: u16,
    y: u16,
) -> Option<crate::ui::writer::PanelClick> {
    use crate::ui::writer::{panel_collapsed, pill_width, PanelClick};
    let (_, cols) = state.term_size;
    if !hits(layout.panel, x, y) || panel_collapsed(cols) {
        return None;
    }
    let session = state.writers.get(&id)?;
    let rows = crate::ui::writer::panel_rows(session, layout.panel.width as usize);
    let skip = crate::ui::writer::panel_skip(rows.len(), layout.panel.height as usize);
    let row = rows.get(y.saturating_sub(layout.panel.y) as usize + skip)?;
    match row.click {
        Some(click) => Some(click),
        None => {
            // Pills row: Accept then Reject from the panel edge.
            let accept = pill_width("Accept", true);
            let reject = pill_width("Reject", false);
            let rel = x.saturating_sub(layout.panel.x);
            // Find the proposal this pills row belongs to: the
            // nearest Select above it.
            let index = y.saturating_sub(layout.panel.y) as usize + skip;
            let owner = rows[..index.min(rows.len())]
                .iter()
                .rev()
                .filter_map(|r| match r.click {
                    Some(PanelClick::Select(pid)) => Some(pid),
                    _ => None,
                })
                .next()?;
            if rel < accept {
                Some(PanelClick::Accept(owner))
            } else if rel >= accept + 2 && rel < accept + 2 + reject {
                Some(PanelClick::Reject(owner))
            } else {
                None
            }
        }
    }
}

/// Fire one panel click through the single accept/reject methods, so
/// a click can never skip the doc+editor+queue sync. Failures land in
/// the fixed error slot.
fn fire_panel_click(
    state: &mut AppState,
    id: crate::session::SessionId,
    click: crate::ui::writer::PanelClick,
) {
    use crate::ui::writer::PanelClick;
    match click {
        PanelClick::Select(pid) => state.writer_select_proposal(id, pid),
        PanelClick::Accept(pid) => {
            if let Err(message) = state.writer_accept(id, pid) {
                if let Some(session) = state.writers.get_mut(&id) {
                    session.error = Some(message);
                    state.dirty = true;
                }
            }
        }
        PanelClick::Reject(pid) => {
            if let Err(message) = state.writer_reject(id, pid) {
                if let Some(session) = state.writers.get_mut(&id) {
                    session.error = Some(message);
                    state.dirty = true;
                }
            }
        }
    }
}
