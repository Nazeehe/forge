//! Chat box, thread cursor and bracketed paste: the assistant
//! panel's inputs plus the router that pastes into whatever owns
//! the input.

use crate::app::writer::WriterFocus;
use crate::app::AppState;
use edtui::{
    actions::{DeleteSelection, InsertChar},
    EditorMode,
};

impl AppState {
    /// Type one char into the chat box, bounded at
    /// [`crate::writer::MAX_INPUT_CHARS`]. Control chars never enter.
    pub fn writer_chat_char(&mut self, id: crate::session::SessionId, c: char) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        if c.is_control() {
            return;
        }
        if session.chat_select_all {
            session.chat_input.clear();
            session.chat_cursor = 0;
            session.chat_select_all = false;
        }
        let len = session.chat_input.chars().count();
        if len < crate::writer::MAX_INPUT_CHARS {
            let at = session.chat_cursor.min(len);
            let mut text = session.chat_input.clone();
            let byte = text
                .char_indices()
                .nth(at)
                .map(|(index, _)| index)
                .unwrap_or(text.len());
            text.insert(byte, c);
            session.chat_input = text;
            session.chat_cursor = at + 1;
            self.dirty = true;
        }
    }

    /// Backspace one char before the chat cursor. A select-all clears.
    pub fn writer_chat_backspace(&mut self, id: crate::session::SessionId) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        if session.chat_select_all {
            session.chat_input.clear();
            session.chat_cursor = 0;
            session.chat_select_all = false;
            self.dirty = true;
            return;
        }
        if session.chat_cursor == 0 {
            return;
        }
        let mut chars: Vec<char> = session.chat_input.chars().collect();
        if session.chat_cursor <= chars.len() {
            chars.remove(session.chat_cursor - 1);
            session.chat_input = chars.into_iter().collect();
            session.chat_cursor -= 1;
            self.dirty = true;
        }
    }

    /// Move the chat cursor one char, clamped to the input. Drops a
    /// select-all.
    pub fn writer_chat_move(&mut self, id: crate::session::SessionId, dir: i32) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        let len = session.chat_input.chars().count();
        session.chat_cursor = (session.chat_cursor as i32 + dir).clamp(0, len as i32) as usize;
        session.chat_select_all = false;
        self.dirty = true;
    }

    /// Chat cursor to the head of the input. Drops a select-all.
    pub fn writer_chat_home(&mut self, id: crate::session::SessionId) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        session.chat_cursor = 0;
        session.chat_select_all = false;
        self.dirty = true;
    }

    /// Chat cursor to the end of the input. Drops a select-all.
    pub fn writer_chat_end(&mut self, id: crate::session::SessionId) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        session.chat_cursor = session.chat_input.chars().count();
        session.chat_select_all = false;
        self.dirty = true;
    }

    /// Chat cursor one word in `dir` (-1/1). Drops a select-all.
    pub fn writer_chat_word(&mut self, id: crate::session::SessionId, dir: i32) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        session.chat_cursor = word_edge(&session.chat_input.clone(), session.chat_cursor, dir);
        session.chat_select_all = false;
        self.dirty = true;
    }

    /// Delete one char under the chat cursor. A select-all clears.
    pub fn writer_chat_delete(&mut self, id: crate::session::SessionId) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        if session.chat_select_all {
            session.chat_input.clear();
            session.chat_cursor = 0;
            session.chat_select_all = false;
            self.dirty = true;
            return;
        }
        let mut chars: Vec<char> = session.chat_input.chars().collect();
        if session.chat_cursor < chars.len() {
            chars.remove(session.chat_cursor);
            session.chat_input = chars.into_iter().collect();
            self.dirty = true;
        }
    }

    /// Mark the whole chat input for type-to-replace.
    pub fn writer_chat_select_all(&mut self, id: crate::session::SessionId) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        session.chat_select_all = !session.chat_input.is_empty();
        self.dirty = true;
    }

    /// Place the chat cursor at a char index (mouse click-to-place).
    /// Clamped; drops a select-all.
    pub fn writer_chat_place(&mut self, id: crate::session::SessionId, at: usize) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        session.chat_cursor = at.min(session.chat_input.chars().count());
        session.chat_select_all = false;
        self.dirty = true;
    }

    /// Move the thread selection one step through pending proposals,
    /// clamped at the ends. A missing selection starts at the first.
    pub fn writer_thread_move(&mut self, id: crate::session::SessionId, dir: i32) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        let ids: Vec<u64> = session.proposals.pending().map(|p| p.id).collect();
        if ids.is_empty() {
            return;
        }
        let next = match session
            .selected_proposal
            .and_then(|current| ids.iter().position(|id| *id == current))
        {
            Some(index) => (index as i32 + dir).clamp(0, ids.len() as i32 - 1) as usize,
            None => 0,
        };
        session.selected_proposal = Some(ids[next]);
        self.dirty = true;
    }

    /// Select a proposal by clicking its thread entry. Unknown ids
    /// never select: the click must name a real proposal.
    pub fn writer_select_proposal(&mut self, id: crate::session::SessionId, proposal_id: u64) {
        let valid = self
            .writers
            .get(&id)
            .and_then(|session| session.proposals.get(proposal_id))
            .is_some();
        if !valid {
            return;
        }
        if let Some(session) = self.writers.get_mut(&id) {
            session.selected_proposal = Some(proposal_id);
            self.dirty = true;
        }
    }

    /// Paste into whatever owns the input: the typed-path prompt, the
    /// chat box (both bounded), or the editor buffer through the
    /// adapter sync. Pastes never reach the agent pane behind the
    /// overlay.
    pub fn writer_paste(&mut self, id: crate::session::SessionId, text: &str) {
        let prompt_open = self
            .writers
            .get(&id)
            .is_some_and(|session| session.open_prompt.is_some());
        if prompt_open {
            for c in text.chars() {
                self.writer_prompt_char(id, c);
            }
            return;
        }
        let chat_focused = self
            .writers
            .get(&id)
            .is_some_and(|session| session.focus == WriterFocus::Chat);
        if chat_focused {
            for c in text.chars() {
                self.writer_chat_char(id, c);
            }
            return;
        }
        // A bracketed paste types literally at the cursor (one undo
        // step per char, like typing), never vim-`p` after it. A live
        // selection is replaced first, still as one undo step.
        {
            let Some(session) = self.writers.get_mut(&id) else {
                return;
            };
            let Some(editor) = session.editor.as_mut() else {
                return;
            };
            editor.mode = EditorMode::Insert;
            if editor.selection.is_some() {
                editor.execute(DeleteSelection);
            }
            for c in text.chars() {
                if c == '\r' {
                    continue;
                }
                editor.execute(InsertChar(c));
            }
            session.nav_goal = None;
        }
        self.writer_sync_editor(id);
        self.dirty = true;
    }

    /// Clear the editor selection (the chat chip ✕): collapse to the
    /// cursor so later sends fall back to paragraph or document.
    pub fn writer_clear_selection(&mut self, id: crate::session::SessionId) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        let Some(editor) = session.editor.as_mut() else {
            return;
        };
        editor.selection = None;
        editor.mode = EditorMode::Insert;
        session.selection = None;
        session.sel_anchor = None;
        self.dirty = true;
    }
}

/// Word edge in `dir` (-1 left, +1 right) from a char `cursor`.
/// Word chars are alphanumeric plus underscore; separators bound
/// words. Clamped to the text.
pub(super) fn word_edge(text: &str, cursor: usize, dir: i32) -> usize {
    fn word_char(c: char) -> bool {
        c.is_alphanumeric() || c == '_'
    }
    let chars: Vec<char> = text.chars().collect();
    let mut at = cursor.min(chars.len());
    if dir < 0 {
        while at > 0 && !word_char(chars[at - 1]) {
            at -= 1;
        }
        while at > 0 && word_char(chars[at - 1]) {
            at -= 1;
        }
    } else {
        while at < chars.len() && !word_char(chars[at]) {
            at += 1;
        }
        while at < chars.len() && word_char(chars[at]) {
            at += 1;
        }
    }
    at
}
