//! Writer key routing: overlay prompt/empty-state keys, focus dispatch
//! to editor/chat/thread, plus the chat-box and thread key handlers.

use crossterm::event;

use crate::app::AppState;

/// One key while the Writer overlay owns input. The typed-path prompt
/// takes typing/Tab/Enter/Esc; Esc dismisses the More menu or a
/// pending confirm anywhere; without a document `n`/`o` (and their
/// Ctrl twins) open the prompt and ↑↓/Enter drive the recent list;
/// with a document Ctrl+N/O/S ride above every focus so the toolbar
/// always has a keyboard twin (Ctrl+N yields its Emacs next-line to
/// the table twin), `Alt+R` rephrases, and every other key routes by
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
            (KeyCode::Delete, _) => state.writer_prompt_delete(id),
            (KeyCode::Tab, KeyModifiers::NONE) => state.writer_prompt_complete(id),
            (KeyCode::Left, KeyModifiers::NONE) => state.writer_prompt_move(id, -1),
            (KeyCode::Right, KeyModifiers::NONE) => state.writer_prompt_move(id, 1),
            (KeyCode::Home, _) => state.writer_prompt_home(id),
            (KeyCode::End, _) => state.writer_prompt_end(id),
            (KeyCode::Left, KeyModifiers::CONTROL) => state.writer_prompt_word(id, -1),
            (KeyCode::Right, KeyModifiers::CONTROL) => state.writer_prompt_word(id, 1),
            (KeyCode::Char('a'), KeyModifiers::CONTROL) => state.writer_prompt_select_all(id),
            (KeyCode::Char(c), KeyModifiers::NONE) => state.writer_prompt_char(id, c),
            _ => {}
        }
        return;
    }
    // Esc dismisses the topmost transient first; otherwise it falls
    // through to its normal target (prompt cancel above, editor
    // selection clearing below).
    if key.code == KeyCode::Esc && key.modifiers == KeyModifiers::NONE {
        if state.writer_dismiss_top(id) {
            return;
        }
    }
    let focus = state.writers.get(&id).map(|session| session.focus);
    let has_editor = state
        .writers
        .get(&id)
        .is_some_and(|session| session.editor.is_some());
    if !has_editor {
        match (key.code, key.modifiers) {
            (KeyCode::Char('n'), KeyModifiers::NONE)
            | (KeyCode::Char('n'), KeyModifiers::CONTROL) => state.writer_toolbar_new(id),
            (KeyCode::Char('o'), KeyModifiers::NONE)
            | (KeyCode::Char('o'), KeyModifiers::CONTROL) => state.writer_toolbar_open(id),
            (KeyCode::Up, _) => state.writer_recent_move(id, -1),
            (KeyCode::Down, _) => state.writer_recent_move(id, 1),
            (KeyCode::Enter, _) => state.writer_recent_open(id),
            _ => {}
        }
        return;
    }
    // Twins ride above every focus so the toolbar pills always have a
    // keyboard path, even mid-sentence in the editor (Emacs isearch
    // and next-line give way inside the Writer tab). Rephrase rides on
    // Alt+R, which is free in EdTUI's emacs map and outside the Forge
    // prefix: Ctrl+R stays the editor's redo.
    if key.modifiers == KeyModifiers::CONTROL {
        match key.code {
            KeyCode::Char('n') => {
                state.writer_toolbar_new(id);
                return;
            }
            KeyCode::Char('o') => {
                state.writer_toolbar_open(id);
                return;
            }
            KeyCode::Char('s') => {
                state.writer_toolbar_save(id);
                return;
            }
            KeyCode::Char('f') => {
                if state
                    .writers
                    .get(&id)
                    .is_some_and(|session| session.find.is_some())
                {
                    state.writer_find_close(id);
                } else {
                    state.writer_find_open(id, false);
                }
                return;
            }
            KeyCode::Char('h') => {
                state.writer_find_open(id, true);
                return;
            }
            _ => {}
        }
    }
    // Alt+H is the reachable replace twin: legacy terminals deliver
    // Ctrl+H as Backspace, which can never carry Control.
    if key.modifiers == KeyModifiers::ALT && key.code == KeyCode::Char('h') {
        state.writer_find_open(id, true);
        return;
    }
    if key.modifiers == KeyModifiers::ALT && key.code == KeyCode::Char('r') {
        state.writer_rephrase(id);
        return;
    }
    // The Assistant toggle rides beside Rephrase: Alt+A is free in
    // the CUA register and outside the Forge prefix.
    if key.modifiers == KeyModifiers::ALT && key.code == KeyCode::Char('a') {
        state.writer_toggle_assistant(id);
        return;
    }
    // Preview rides the same Alt family: Alt+P is free in the CUA
    // register, outside the Forge prefix, and unlike Ctrl+Shift+V
    // no terminal steals it for paste.
    if key.modifiers == KeyModifiers::ALT && key.code == KeyCode::Char('p') {
        state.writer_toggle_preview(id);
        return;
    }
    // F6 owns focus cycling (Editor → Chat → Thread) now that Tab
    // indents. The prompt keeps its own keys above.
    if key.code == KeyCode::F(6) {
        state.writer_cycle_focus(id);
        return;
    }
    // Tab indents in the editor (Shift+Tab outdents) and is ignored
    // everywhere else: Tab never focus-cycles. The prompt keeps its
    // own Tab (completion) above.
    if key.code == KeyCode::Tab {
        if focus == Some(crate::app::writer::WriterFocus::Editor) {
            if state
                .writers
                .get(&id)
                .is_some_and(|session| session.find.is_some())
            {
                state.writer_find_tab(id);
            } else {
                state.writer_feed_key(id, key);
            }
        }
        return;
    }
    // The find bar owns its keys while open with Editor focus: text
    // and navigation edit the focused field, Enter activates the
    // Tab-focused control, F3/Shift+F3 (and Up/Down) step through
    // matches. Undo/redo still reach the editor, so a replace-all
    // can be reverted with the bar open; anything else is ignored,
    // prompt parity.
    if focus == Some(crate::app::writer::WriterFocus::Editor)
        && state
            .writers
            .get(&id)
            .is_some_and(|session| session.find.is_some())
    {
        use event::KeyCode as KC;
        use event::KeyModifiers as KM;
        match (key.code, key.modifiers) {
            (KC::Char('z'), KM::CONTROL)
            | (KC::Char('y'), KM::CONTROL)
            | (KC::Char('Z'), KM::CONTROL | KM::SHIFT) => state.writer_feed_key(id, key),
            _ => handle_find_key(state, id, key),
        }
        return;
    }
    match focus {
        Some(crate::app::writer::WriterFocus::Chat) => handle_chat_key(state, id, key),
        Some(crate::app::writer::WriterFocus::Thread) => handle_thread_key(state, id, key),
        _ => state.writer_feed_key(id, key),
    }
}

/// One key with the find bar open and Editor focus: text edits the
/// Tab-focused field (same editing as the path prompt), Enter
/// activates the Tab-focused control, F3/Shift+F3 and Up/Down step
/// through matches with wrap-around. Esc never arrives here: it
/// closes the bar through `writer_dismiss_top` above.
fn handle_find_key(state: &mut AppState, id: crate::session::SessionId, key: event::KeyEvent) {
    use event::{KeyCode, KeyModifiers};
    match (key.code, key.modifiers) {
        (KeyCode::Enter, KeyModifiers::NONE) => state.writer_find_activate(id),
        (KeyCode::Enter, KeyModifiers::SHIFT) => state.writer_find_next(id, -1),
        (KeyCode::F(3), KeyModifiers::NONE) => state.writer_find_next(id, 1),
        (KeyCode::F(3), KeyModifiers::SHIFT) => state.writer_find_next(id, -1),
        (KeyCode::Up, KeyModifiers::NONE) => state.writer_find_next(id, -1),
        (KeyCode::Down, KeyModifiers::NONE) => state.writer_find_next(id, 1),
        (KeyCode::Backspace, _) => state.writer_find_backspace(id),
        (KeyCode::Delete, _) => state.writer_find_delete(id),
        (KeyCode::Left, KeyModifiers::NONE) => state.writer_find_move(id, -1),
        (KeyCode::Right, KeyModifiers::NONE) => state.writer_find_move(id, 1),
        (KeyCode::Home, _) => state.writer_find_home(id),
        (KeyCode::End, _) => state.writer_find_end(id),
        (KeyCode::Left, KeyModifiers::CONTROL) => state.writer_find_word(id, -1),
        (KeyCode::Right, KeyModifiers::CONTROL) => state.writer_find_word(id, 1),
        (KeyCode::Char('a'), KeyModifiers::CONTROL) => state.writer_find_select_all(id),
        (KeyCode::Char(c), KeyModifiers::NONE) => state.writer_find_char(id, c),
        (KeyCode::Char(c), KeyModifiers::SHIFT) => state.writer_find_char(id, c),
        _ => {}
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
        (KeyCode::Delete, _) => state.writer_chat_delete(id),
        (KeyCode::Left, KeyModifiers::NONE) => state.writer_chat_move(id, -1),
        (KeyCode::Right, KeyModifiers::NONE) => state.writer_chat_move(id, 1),
        (KeyCode::Home, _) => state.writer_chat_home(id),
        (KeyCode::End, _) => state.writer_chat_end(id),
        (KeyCode::Left, KeyModifiers::CONTROL) => state.writer_chat_word(id, -1),
        (KeyCode::Right, KeyModifiers::CONTROL) => state.writer_chat_word(id, 1),
        (KeyCode::Char('a'), KeyModifiers::CONTROL) => state.writer_chat_select_all(id),
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
