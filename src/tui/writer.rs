//! TUI Writer input: key routing for the Writer overlay tab.
//!
//! While the Writer overlay is focused, keys belong to the document,
//! not the agent pane — except the `Ctrl-b` prefix chord, which always
//! escapes to the router (the routing guard guarantees that before
//! this runs). The empty state, chat box, and thread keys arrive in
//! later slices; this slice routes editor keys to the adapter.

use crossterm::event;

use crate::app::AppState;

/// One key while the Writer overlay owns input. The typed-path prompt
/// takes typing/Enter/Esc; without a document `n`/`o` open the prompt;
/// otherwise Tab cycles Editor → Chat → Thread, `Ctrl+S` saves and
/// `Ctrl+R` rephrases from any focus, and every other key routes by
/// focus: editor keys to the adapter, chat typing to the chat box,
/// thread keys to the proposal selection.
pub(super) fn handle_writer_key(state: &mut AppState, key: event::KeyEvent) {
    use event::{KeyCode, KeyModifiers};
    let Some(id) = state.writer_overlay_active() else {
        return;
    };
    let prompt_open = state
        .writers
        .get(&id)
        .is_some_and(|session| session.open_prompt.is_some());
    if prompt_open {
        match (key.code, key.modifiers) {
            (KeyCode::Enter, _) => state.writer_submit_open(id),
            (KeyCode::Esc, _) => state.writer_prompt_cancel(id),
            (KeyCode::Backspace, _) => state.writer_prompt_backspace(id),
            (KeyCode::Char(c), KeyModifiers::NONE) => state.writer_prompt_char(id, c),
            _ => {}
        }
        return;
    }
    let focus = state.writers.get(&id).map(|session| session.focus);
    let has_editor = state
        .writers
        .get(&id)
        .is_some_and(|session| session.editor.is_some());
    if !has_editor {
        match (key.code, key.modifiers) {
            (KeyCode::Char('n'), KeyModifiers::NONE) => state.writer_prompt_open(id, true),
            (KeyCode::Char('o'), KeyModifiers::NONE) => state.writer_prompt_open(id, false),
            _ => {}
        }
        return;
    }
    // Save and Rephrase ride above every focus so the pills always
    // have a keyboard twin, even mid-sentence in the editor. (Emacs
    // reverse-search and isearch give way inside the Writer tab.)
    if key.modifiers == KeyModifiers::CONTROL {
        match key.code {
            KeyCode::Char('s') => {
                state.writer_save(id);
                return;
            }
            KeyCode::Char('r') => {
                state.writer_rephrase(id);
                return;
            }
            _ => {}
        }
    }
    if key.code == KeyCode::Tab {
        state.writer_cycle_focus(id);
        return;
    }
    match focus {
        Some(crate::app::writer::WriterFocus::Chat) => handle_chat_key(state, id, key),
        Some(crate::app::writer::WriterFocus::Thread) => handle_thread_key(state, id, key),
        _ => state.writer_feed_key(id, key),
    }
}

/// One key with the chat box focused: typing edits the input bounded
/// at `MAX_INPUT_CHARS`, Enter sends, Esc returns to the editor.
fn handle_chat_key(state: &mut AppState, id: crate::session::SessionId, key: event::KeyEvent) {
    use event::{KeyCode, KeyModifiers};
    match (key.code, key.modifiers) {
        (KeyCode::Enter, _) => state.writer_chat_send(id),
        (KeyCode::Esc, _) => {
            if let Some(session) = state.writers.get_mut(&id) {
                session.focus = crate::app::writer::WriterFocus::Editor;
                state.dirty = true;
            }
        }
        (KeyCode::Backspace, _) => state.writer_chat_backspace(id),
        (KeyCode::Left, _) => state.writer_chat_move(id, -1),
        (KeyCode::Right, _) => state.writer_chat_move(id, 1),
        (KeyCode::Char(c), KeyModifiers::NONE) => state.writer_chat_char(id, c),
        _ => {}
    }
}

/// One main-area mouse event while the Writer overlay owns input.
/// Pill clicks fire, thread rows select, the editor takes cursor,
/// drag, and wheel through the adapter (EdTUI ignores events outside
/// its painted area), and everything else dies here so clicks never
/// reach the agent pane behind the overlay. Hit areas come from the
/// same layout the render paints.
pub(super) fn handle_writer_mouse(state: &mut AppState, mev: event::MouseEvent) {
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
    let layout = crate::ui::writer::writer_layout(content);
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

/// One key with the thread focused: `j`/`k` move the proposal
/// selection, Enter/`a` accepts, `x` rejects, `r` rephrases the live
/// range, Esc returns to the editor. Accept/Reject run through the
/// single `writer_accept` / `writer_reject` methods, so a key can
/// never skip the doc+editor+queue sync.
fn handle_thread_key(state: &mut AppState, id: crate::session::SessionId, key: event::KeyEvent) {
    use event::{KeyCode, KeyModifiers};
    match (key.code, key.modifiers) {
        (KeyCode::Char('j'), KeyModifiers::NONE) | (KeyCode::Down, _) => {
            state.writer_thread_move(id, 1);
        }
        (KeyCode::Char('k'), KeyModifiers::NONE) | (KeyCode::Up, _) => {
            state.writer_thread_move(id, -1);
        }
        (KeyCode::Enter, _) | (KeyCode::Char('a'), KeyModifiers::NONE) => {
            if let Some(pid) = state
                .writers
                .get(&id)
                .and_then(|session| session.selected_proposal)
            {
                if let Err(message) = state.writer_accept(id, pid) {
                    if let Some(session) = state.writers.get_mut(&id) {
                        session.error = Some(message);
                        state.dirty = true;
                    }
                }
            }
        }
        (KeyCode::Char('x'), KeyModifiers::NONE) => {
            if let Some(pid) = state
                .writers
                .get(&id)
                .and_then(|session| session.selected_proposal)
            {
                if let Err(message) = state.writer_reject(id, pid) {
                    if let Some(session) = state.writers.get_mut(&id) {
                        session.error = Some(message);
                        state.dirty = true;
                    }
                }
            }
        }
        (KeyCode::Char('r'), KeyModifiers::NONE) => state.writer_rephrase(id),
        (KeyCode::Esc, _) => {
            if let Some(session) = state.writers.get_mut(&id) {
                session.focus = crate::app::writer::WriterFocus::Editor;
                state.dirty = true;
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::AppState;
    use crate::tui::input::{prefix_key, InputRouter};
    use crate::tui::keys::handle_key_at;

    fn key(code: event::KeyCode) -> event::KeyEvent {
        event::KeyEvent::new(code, event::KeyModifiers::NONE)
    }

    fn ctrl(code: event::KeyCode) -> event::KeyEvent {
        event::KeyEvent::new(code, event::KeyModifiers::CONTROL)
    }

    fn tab() -> event::KeyEvent {
        key(event::KeyCode::Tab)
    }

    fn shift_select(state: &mut AppState, id: crate::session::SessionId, count: usize) {
        let shift = event::KeyModifiers::SHIFT;
        for _ in 0..count {
            state.writer_feed_key(
                id,
                event::KeyEvent::new(event::KeyCode::Right, shift),
            );
        }
    }

    fn writer_agent() -> (AppState, crate::session::SessionId, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "forge-tui-writer-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let mut state = AppState::new();
        let id = state
            .manager
            .spawn_agent(
                "agent",
                &dir,
                "exec cat",
                crate::infra::ids::RunId::generate(),
                "codex",
            )
            .unwrap();
        (state, id, dir)
    }

    fn open_doc(state: &mut AppState, id: crate::session::SessionId, name: &str) {
        // Default text only when the test did not pre-write the file.
        let path = state.manager.get(id).unwrap().cwd.join(name);
        if !path.exists() {
            std::fs::write(&path, "aaa bbb").unwrap();
        }
        let doc =
            crate::writer::document::Document::open(&state.manager.get(id).unwrap().cwd, name)
                .unwrap();
        let session = state.writers.entry(id).or_default();
        session.doc = Some(doc);
        state.writer_open_editor(id);
    }

    #[test]
    fn prefix_d_opens_the_writer_tab() {
        let (mut state, id, dir) = writer_agent();
        let mut router = InputRouter::new();
        let now = std::time::Instant::now();
        handle_key_at(&mut state, &mut router, prefix_key(), now);
        handle_key_at(&mut state, &mut router, key(event::KeyCode::Char('d')), now);
        assert_eq!(state.writer_overlay_active(), Some(id));
        assert!(state.overlay_view.is_some(), "overlay tab opens");
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn typing_reaches_the_editor_not_the_pane() {
        let (mut state, id, dir) = writer_agent();
        open_doc(&mut state, id, "d.md");
        state.open_writer_overlay();
        let mut router = InputRouter::new();
        let now = std::time::Instant::now();
        handle_key_at(&mut state, &mut router, key(event::KeyCode::Char('!')), now);
        assert_eq!(
            state.writers.get(&id).unwrap().doc.as_ref().unwrap().text,
            "!aaa bbb"
        );
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn empty_state_keys_drive_the_prompt() {
        let (mut state, id, dir) = writer_agent();
        std::fs::write(dir.join("d.md"), "aaa").unwrap();
        state.open_writer_overlay();
        let mut router = InputRouter::new();
        let now = std::time::Instant::now();
        // Esc cancels a fresh prompt before anything opens.
        handle_key_at(&mut state, &mut router, key(event::KeyCode::Char('n')), now);
        assert!(state.writers.get(&id).unwrap().open_prompt.is_some());
        handle_key_at(&mut state, &mut router, key(event::KeyCode::Esc), now);
        assert!(state.writers.get(&id).unwrap().open_prompt.is_none());
        // `o` opens the prompt, typing fills it, Enter submits.
        handle_key_at(&mut state, &mut router, key(event::KeyCode::Char('o')), now);
        assert!(state.writers.get(&id).unwrap().open_prompt.is_some());
        for code in [
            event::KeyCode::Char('d'),
            event::KeyCode::Char('.'),
            event::KeyCode::Char('m'),
            event::KeyCode::Char('d'),
        ] {
            handle_key_at(&mut state, &mut router, key(code), now);
        }
        handle_key_at(&mut state, &mut router, key(event::KeyCode::Enter), now);
        let session = state.writers.get(&id).unwrap();
        assert!(session.open_prompt.is_none(), "prompt closes on open");
        assert!(session.editor.is_some(), "editor builds");
        assert_eq!(session.doc.as_ref().unwrap().text, "aaa");
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn tab_cycles_editor_chat_thread() {
        use crate::app::writer::WriterFocus;
        let (mut state, id, dir) = writer_agent();
        open_doc(&mut state, id, "d.md");
        state.open_writer_overlay();
        let mut router = InputRouter::new();
        let now = std::time::Instant::now();
        assert_eq!(state.writers.get(&id).unwrap().focus, WriterFocus::Editor);
        handle_key_at(&mut state, &mut router, tab(), now);
        assert_eq!(state.writers.get(&id).unwrap().focus, WriterFocus::Chat);
        handle_key_at(&mut state, &mut router, tab(), now);
        assert_eq!(state.writers.get(&id).unwrap().focus, WriterFocus::Thread);
        handle_key_at(&mut state, &mut router, tab(), now);
        assert_eq!(state.writers.get(&id).unwrap().focus, WriterFocus::Editor);
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn chat_box_edits_and_enter_sends() {
        let (mut state, id, dir) = writer_agent();
        open_doc(&mut state, id, "d.md");
        state.open_writer_overlay();
        let mut router = InputRouter::new();
        let now = std::time::Instant::now();
        handle_key_at(&mut state, &mut router, tab(), now);
        for c in ['h', 'i', '?'] {
            handle_key_at(&mut state, &mut router, key(event::KeyCode::Char(c)), now);
        }
        assert_eq!(state.writers.get(&id).unwrap().chat_input, "hi?");
        // The editor never saw the typing.
        assert_eq!(
            state.writers.get(&id).unwrap().doc.as_ref().unwrap().text,
            "aaa bbb"
        );
        handle_key_at(&mut state, &mut router, key(event::KeyCode::Enter), now);
        let session = state.writers.get(&id).unwrap();
        assert!(session.chat_input.is_empty(), "chat clears on send");
        assert!(session.queue.back().expect("queued").contains("action=chat"));
        // Esc returns to the editor.
        handle_key_at(&mut state, &mut router, key(event::KeyCode::Esc), now);
        assert_eq!(
            state.writers.get(&id).unwrap().focus,
            crate::app::writer::WriterFocus::Editor
        );
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn thread_keys_select_accept_and_reject() {
        let (mut state, id, dir) = writer_agent();
        open_doc(&mut state, id, "d.md");
        let doc = state.writers.get(&id).unwrap().doc.clone().unwrap();
        let session = state.writers.get_mut(&id).unwrap();
        session
            .proposals
            .propose(&doc, None, 0..3, "AAA".to_string(), None)
            .unwrap();
        session
            .proposals
            .propose(&doc, None, 4..7, "BBB".to_string(), None)
            .unwrap();
        state.open_writer_overlay();
        let mut router = InputRouter::new();
        let now = std::time::Instant::now();
        handle_key_at(&mut state, &mut router, tab(), now);
        handle_key_at(&mut state, &mut router, tab(), now);
        // `j` selects the first pending proposal, Enter accepts it.
        handle_key_at(&mut state, &mut router, key(event::KeyCode::Char('j')), now);
        let first = state.writers.get(&id).unwrap().selected_proposal;
        assert!(first.is_some(), "j selects");
        handle_key_at(&mut state, &mut router, key(event::KeyCode::Enter), now);
        assert_eq!(
            state.writers.get(&id).unwrap().doc.as_ref().unwrap().text,
            "AAA bbb"
        );
        // `j` then `x` rejects the second.
        handle_key_at(&mut state, &mut router, key(event::KeyCode::Char('j')), now);
        handle_key_at(&mut state, &mut router, key(event::KeyCode::Char('x')), now);
        let session = state.writers.get(&id).unwrap();
        assert_eq!(session.proposals.pending_count(), 0, "both settled");
        assert_eq!(session.doc.as_ref().unwrap().text, "AAA bbb");
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn ctrl_s_saves_and_ctrl_r_rephrases() {
        let (mut state, id, dir) = writer_agent();
        open_doc(&mut state, id, "d.md");
        state.open_writer_overlay();
        let mut router = InputRouter::new();
        let now = std::time::Instant::now();
        handle_key_at(&mut state, &mut router, key(event::KeyCode::Char('!')), now);
        handle_key_at(&mut state, &mut router, ctrl(event::KeyCode::Char('s')), now);
        assert_eq!(std::fs::read_to_string(dir.join("d.md")).unwrap(), "!aaa bbb");
        assert_eq!(state.writers.get(&id).unwrap().error, None);
        // Ctrl+R rephrases the paragraph under the cursor.
        handle_key_at(&mut state, &mut router, ctrl(event::KeyCode::Char('r')), now);
        let session = state.writers.get(&id).unwrap();
        assert!(session.queue.back().expect("queued").contains("action=rephrase"));
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    fn click_at(x: u16, y: u16) -> event::MouseEvent {
        event::MouseEvent {
            kind: event::MouseEventKind::Down(event::MouseButton::Left),
            column: x,
            row: y,
            modifiers: event::KeyModifiers::NONE,
        }
    }

    fn find_text(buf: &ratatui::buffer::Buffer, needle: &str) -> (u16, u16) {
        find_all_text(buf, needle)
            .into_iter()
            .next()
            .unwrap_or_else(|| panic!("needle {needle:?} not painted"))
    }

    fn find_all_text(buf: &ratatui::buffer::Buffer, needle: &str) -> Vec<(u16, u16)> {
        let mut hits = Vec::new();
        for y in 0..buf.area.height {
            let row: String = (0..buf.area.width)
                .map(|x| buf[(x, y)].symbol())
                .collect();
            let mut rest = row.as_str();
            let mut offset = 0usize;
            while let Some(byte) = rest.find(needle) {
                let x = (offset + row[offset..offset + byte].chars().count()) as u16;
                hits.push((x, y));
                let step = byte + needle.len();
                offset += step;
                rest = &rest[step..];
            }
        }
        hits
    }

    /// Paint the overlay exactly like the draw closure, so clicks read
    /// off the buffer land where the handler looks.
    fn paint_full(state: &mut AppState, id: crate::session::SessionId) -> ratatui::buffer::Buffer {
        use ratatui::{backend::TestBackend, Terminal};
        let (rows, cols) = state.term_size;
        let area = crate::walkthrough::walk_area(ratatui::layout::Rect::new(0, 0, cols, rows));
        let activity = state.manager.get(id).map(|rec| rec.activity).unwrap();
        let mut terminal = Terminal::new(TestBackend::new(cols, rows)).unwrap();
        terminal
            .draw(|f| {
                crate::ui::writer::paint(f, area, state.writers.get_mut(&id).unwrap(), activity, cols);
            })
            .unwrap();
        terminal.backend().buffer().clone()
    }

    #[test]
    fn empty_pills_fire_on_click() {
        let (mut state, id, dir) = writer_agent();
        state.term_size = (30, 120);
        state.open_writer_overlay();
        let buf = paint_full(&mut state, id);
        let (x, y) = find_text(&buf, "*New document");
        super::handle_writer_mouse(&mut state, click_at(x, y));
        let session = state.writers.get(&id).unwrap();
        assert!(session.open_prompt.as_ref().is_some_and(|p| p.create));
        let buf = paint_full(&mut state, id);
        let (x, y) = find_text(&buf, "Open…");
        super::handle_writer_mouse(&mut state, click_at(x, y));
        assert!(state.writers.get(&id).unwrap().open_prompt.as_ref().is_some_and(|p| !p.create));
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn modal_swallows_writer_clicks() {
        use crate::tui::mouse::forward_mouse;
        let (mut state, id, dir) = writer_agent();
        state.term_size = (30, 120);
        state.open_writer_overlay();
        state.open_quit_confirm();
        let buf = paint_full(&mut state, id);
        let (x, y) = find_text(&buf, "*New document");
        forward_mouse(&mut state, click_at(x, y));
        assert!(state.writers.get(&id).unwrap().open_prompt.is_none(), "click died at the modal");
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn action_pills_fire_on_click() {
        let (mut state, id, dir) = writer_agent();
        open_doc(&mut state, id, "d.md");
        state.term_size = (30, 120);
        state.open_writer_overlay();
        // Save pill writes the typed text.
        let mut router = InputRouter::new();
        let now = std::time::Instant::now();
        handle_key_at(&mut state, &mut router, key(event::KeyCode::Char('!')), now);
        let buf = paint_full(&mut state, id);
        let (x, y) = find_text(&buf, "Save");
        super::handle_writer_mouse(&mut state, click_at(x, y));
        assert_eq!(std::fs::read_to_string(dir.join("d.md")).unwrap(), "!aaa bbb");
        // Rephrase pill queues over the whole paragraph (the last
        // "Rephrase" on screen: the chat hint above names it too).
        let buf = paint_full(&mut state, id);
        let (x, y) = *find_all_text(&buf, "Rephrase")
            .last()
            .expect("rephrase pill painted");
        super::handle_writer_mouse(&mut state, click_at(x, y));
        let session = state.writers.get(&id).unwrap();
        assert!(session.queue.back().expect("queued").contains("action=rephrase"));
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn thread_entry_click_selects_and_accept_pill_fires() {
        let (mut state, id, dir) = writer_agent();
        open_doc(&mut state, id, "d.md");
        let doc = state.writers.get(&id).unwrap().doc.clone().unwrap();
        state
            .writers
            .get_mut(&id)
            .unwrap()
            .proposals
            .propose(&doc, None, 0..3, "AAA".to_string(), None)
            .unwrap();
        state.term_size = (30, 120);
        state.open_writer_overlay();
        // Click the diff row: selects the proposal.
        let buf = paint_full(&mut state, id);
        let (x, y) = find_text(&buf, "- aaa");
        super::handle_writer_mouse(&mut state, click_at(x, y));
        assert!(state.writers.get(&id).unwrap().selected_proposal.is_some());
        // Click Accept: the range is replaced with one undo step.
        let buf = paint_full(&mut state, id);
        let (x, y) = find_text(&buf, "Accept");
        super::handle_writer_mouse(&mut state, click_at(x, y));
        let session = state.writers.get(&id).unwrap();
        assert_eq!(session.doc.as_ref().unwrap().text, "AAA bbb");
        assert_eq!(session.proposals.pending_count(), 0);
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn editor_click_moves_cursor_and_drag_selects() {
        let (mut state, id, dir) = writer_agent();
        open_doc(&mut state, id, "d.md");
        state.term_size = (30, 120);
        state.open_writer_overlay();
        // Paint once so EdTUI learns its screen area.
        let buf = paint_full(&mut state, id);
        let (mut x, y) = find_text(&buf, "bbb");
        // Click the third char: cursor lands on it.
        x += 2;
        super::handle_writer_mouse(&mut state, click_at(x, y));
        assert_eq!(
            state.writers.get(&id).unwrap().editor.as_ref().unwrap().cursor,
            edtui::Index2::new(0, 6)
        );
        // Drag back to the first char: selects exactly "bbb".
        super::handle_writer_mouse(
            &mut state,
            event::MouseEvent {
                kind: event::MouseEventKind::Drag(event::MouseButton::Left),
                column: x.saturating_sub(2),
                row: y,
                modifiers: event::KeyModifiers::NONE,
            },
        );
        assert_eq!(state.writers.get(&id).unwrap().selection, Some(4..7));
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn wheel_scrolls_the_editor() {
        let (mut state, id, dir) = writer_agent();
        let text: String = (0..60).map(|n| format!("line {n}")).collect::<Vec<_>>().join("\n");
        std::fs::write(dir.join("long.md"), &text).unwrap();
        open_doc(&mut state, id, "long.md");
        state.term_size = (30, 120);
        state.open_writer_overlay();
        let buf = paint_full(&mut state, id);
        let (x, y) = find_text(&buf, "line 0");
        for _ in 0..5 {
            super::handle_writer_mouse(
                &mut state,
                event::MouseEvent {
                    kind: event::MouseEventKind::ScrollDown,
                    column: x,
                    row: y,
                    modifiers: event::KeyModifiers::NONE,
                },
            );
        }
        let after = paint_full(&mut state, id);
        let area = crate::walkthrough::walk_area(ratatui::layout::Rect::new(0, 0, 120, 30));
        let editor = crate::ui::writer::writer_layout(area).editor;
        let first: String = (editor.x..editor.x + 8)
            .map(|cx| after[(cx, editor.y)].symbol())
            .collect();
        assert!(!first.starts_with("line 0"), "scrolled: {first:?}");
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn detach_chip_click_clears_the_selection() {
        let (mut state, id, dir) = writer_agent();
        open_doc(&mut state, id, "d.md");
        state.term_size = (30, 120);
        state.open_writer_overlay();
        shift_select(&mut state, id, 3);
        assert!(state.writers.get(&id).unwrap().selection.is_some());
        let buf = paint_full(&mut state, id);
        let (x, y) = find_text(&buf, "(✕)");
        super::handle_writer_mouse(&mut state, click_at(x, y));
        assert_eq!(state.writers.get(&id).unwrap().selection, None);
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn prefix_escapes_while_editing() {
        let (mut state, id, dir) = writer_agent();
        open_doc(&mut state, id, "d.md");
        state.open_writer_overlay();
        let mut router = InputRouter::new();
        let now = std::time::Instant::now();
        // The prefix still reaches the router: Ctrl-b t can leave.
        handle_key_at(&mut state, &mut router, prefix_key(), now);
        assert!(router.is_pending(), "prefix escapes the editor");
        // ... and the editor never saw the chord.
        assert_eq!(
            state.writers.get(&id).unwrap().doc.as_ref().unwrap().text,
            "aaa bbb"
        );
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }
}
