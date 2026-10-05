//! Writer key routing: overlay prompt/empty-state keys, focus dispatch
//! to editor/chat/thread, plus the chat-box and thread key handlers.

use crossterm::event;

use crate::app::AppState;

/// One key while the Writer overlay owns input. The typed-path prompt
/// takes typing/Enter/Esc; without a document `n`/`o` open the prompt;
/// otherwise Tab cycles Editor → Chat → Thread, `Ctrl+S` saves and
/// `Alt+R` rephrases from any focus, and every other key routes by
/// focus: editor keys to the adapter, chat typing to the chat box,
/// thread keys to the proposal selection.
pub(crate) fn handle_writer_key(state: &mut AppState, key: event::KeyEvent) {
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
    // Save rides above every focus so the pill always has a keyboard
    // twin, even mid-sentence in the editor (Emacs isearch gives way
    // inside the Writer tab). Rephrase rides on Alt+R, which is free
    // in EdTUI's emacs map and outside the Forge prefix: Ctrl+R stays
    // the editor's redo.
    if key.modifiers == KeyModifiers::CONTROL && key.code == KeyCode::Char('s') {
        state.writer_save(id);
        return;
    }
    if key.modifiers == KeyModifiers::ALT && key.code == KeyCode::Char('r') {
        state.writer_rephrase(id);
        return;
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
