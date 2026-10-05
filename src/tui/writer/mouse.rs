//! Writer mouse dispatch: pill clicks, thread-row clicks, editor
//! cursor/drag/wheel through the adapter. Hit areas come from the
//! same layout the render paints, so clicks can never desync; an
//! open modal still swallows everything before this runs.

use crossterm::event;

use crate::app::AppState;

/// One main-area mouse event while the Writer overlay owns input.
/// Pill clicks fire, thread rows select, the editor takes cursor,
/// drag, and wheel through the adapter (EdTUI ignores events outside
/// its painted area), and everything else dies here so clicks never
/// reach the agent pane behind the overlay. Hit areas come from the
/// same layout the render paints.
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
    let has_doc = state
        .writers
        .get(&id)
        .is_some_and(|session| session.doc.is_some());
    if !has_doc {
        if matches!(mev.kind, MouseEventKind::Down(MouseButton::Left)) {
            let (new_rect, open_rect) = crate::ui::writer::empty_pill_rects(layout.body);
            if hits(new_rect, mev.column, mev.row) {
                state.writer_prompt_open(id, true);
            } else if hits(open_rect, mev.column, mev.row) {
                state.writer_prompt_open(id, false);
            }
        }
        return;
    }
    match mev.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            let (rephrase, save) = crate::ui::writer::action_pill_rects(layout.action);
            if hits(rephrase, mev.column, mev.row) {
                state.writer_rephrase(id);
                return;
            }
            if hits(save, mev.column, mev.row) {
                state.writer_save(id);
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
            if let Some(click) = panel_click_at(state, id, &layout, mev.column, mev.row) {
                fire_panel_click(state, id, click);
                return;
            }
            if hits(layout.editor, mev.column, mev.row) {
                state.writer_feed_mouse(id, mev);
            }
        }
        MouseEventKind::Drag(_) | MouseEventKind::Up(_) => {
            if hits(layout.editor, mev.column, mev.row) {
                state.writer_feed_mouse(id, mev);
            }
        }
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            if hits(layout.editor, mev.column, mev.row) {
                state.writer_feed_mouse(id, mev);
            }
        }
        _ => {}
    }
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
