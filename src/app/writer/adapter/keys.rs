//! Adapter input layer: editor ownership, key/mouse feed, buffer
//! sync, and the prompt/chat/focus/thread/paste session ops the TUI
//! input layer calls into.
//!
//! Movement is adapter-owned on char offsets (see `nav`); the editor
//! mode invariant lives here too: plain moves, clicks, clean mouse
//! releases and Esc all land back in Insert, because EdTUI's hidden
//! vim Normal strands the keyboard with nothing ever returning.

use super::{
    changed_range, editor_selection_to_range, index2_to_offset, offset_to_index2,
};
use super::nav::{nav_target, vertical_target, NavMove};
use crate::app::writer::{MAX_PATH_CHARS, WriterFocus, WriterOpenPrompt};
use crate::app::AppState;
use edtui::{
    actions::{DeleteSelection, InsertChar, SwitchMode},
    EditorEventHandler, EditorMode, EditorState, Lines,
};

impl AppState {
    /// (Re)create the editor from the open document: emacs map with
    /// Insert mode at open, Forge clipboard from the start.
    /// `pub(crate)`: test seam for the TUI input layer, which never
    /// opens editors itself (agents do, through `writer_open`).
    pub(crate) fn writer_open_editor(&mut self, id: crate::session::SessionId) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        let Some(doc) = session.doc.as_ref() else {
            session.editor = None;
            return;
        };
        let mut editor = EditorState::new(Lines::from(doc.text.as_str()));
        editor.mode = EditorMode::Insert;
        editor.set_clipboard(session.clip.clone());
        session.editor = Some(editor);
        session.selection = None;
        session.sel_anchor = None;
        session.nav_goal = None;
        self.dirty = true;
    }

    /// Feed one terminal key to the editor and sync back. Movement is
    /// adapter-owned on top of char offsets, because EdTUI's char
    /// motions stop at line ends and can never cross `\n`, its word
    /// jumps stop at word ends (vim-style, not CUA starts), its page
    /// keys move only the viewport, and several CUA keys have no
    /// emacs-map binding at all. Everything else is forwarded as-is.
    pub fn writer_feed_key(&mut self, id: crate::session::SessionId, key: crossterm::event::KeyEvent) {
        use crossterm::event::{KeyCode, KeyModifiers};
        // Esc always returns the editor to the known-good state: no
        // selection, Insert mode. EdTUI would park in vim Normal
        // (its emacs map, mouse Down while Visual, all strand there)
        // with nothing ever returning, so the adapter owns this.
        if key.code == KeyCode::Esc && key.modifiers == KeyModifiers::empty() {
            if let Some(session) = self.writers.get_mut(&id) {
                if let Some(editor) = session.editor.as_mut() {
                    editor.selection = None;
                    editor.mode = EditorMode::Insert;
                }
                session.sel_anchor = None;
                session.nav_goal = None;
            }
            self.writer_sync_editor(id);
            self.dirty = true;
            return;
        }
        let shift = KeyModifiers::SHIFT;
        let shift_ctrl = KeyModifiers::SHIFT | KeyModifiers::CONTROL;
        // Adapter-owned moves: every move key plain (collapse) or
        // with Shift (select), plus word jumps and doc bounds on
        // Ctrl. Nothing navigation-like reaches EdTUI motions.
        let nav = if key.modifiers == KeyModifiers::empty() || key.modifiers == shift {
            match key.code {
                KeyCode::Left => Some(NavMove::CharLeft),
                KeyCode::Right => Some(NavMove::CharRight),
                KeyCode::Up => Some(NavMove::Up),
                KeyCode::Down => Some(NavMove::Down),
                KeyCode::Home => Some(NavMove::LineStart),
                KeyCode::End => Some(NavMove::LineEnd),
                KeyCode::PageUp => Some(NavMove::PageUp),
                KeyCode::PageDown => Some(NavMove::PageDown),
                _ => None,
            }
        } else if key.modifiers == KeyModifiers::CONTROL
            || key.modifiers == shift_ctrl
        {
            match key.code {
                KeyCode::Left => Some(NavMove::WordLeft),
                KeyCode::Right => Some(NavMove::WordRight),
                KeyCode::Home => Some(NavMove::DocStart),
                KeyCode::End => Some(NavMove::DocEnd),
                _ => None,
            }
        } else {
            None
        };
        if nav.is_none() {
            let Some(session) = self.writers.get_mut(&id) else {
                return;
            };
            let Some(editor) = session.editor.as_mut() else {
                return;
            };
            // Any other key ends the keyboard-selection gesture. A
            // live selection with a text-producing key replaces it
            // first (CUA): EdTUI's emacs map has no Visual-char
            // behavior we can rely on, so the adapter deletes, then
            // forwards the key for the normal insert. (Two undo
            // steps for now; E6 groups them.)
            session.sel_anchor = None;
            session.nav_goal = None;
            if editor.selection.is_some()
                && matches!(
                    (key.code, key.modifiers),
                    (KeyCode::Char(_), KeyModifiers::NONE)
                        | (KeyCode::Char(_), KeyModifiers::SHIFT)
                        | (KeyCode::Backspace, KeyModifiers::NONE)
                        | (KeyCode::Delete, KeyModifiers::NONE)
                        | (KeyCode::Enter, KeyModifiers::NONE)
                )
            {
                editor.execute(DeleteSelection);
                editor.mode = EditorMode::Insert;
            }
            let mut handler = EditorEventHandler::emacs_mode();
            handler.on_event(crossterm::event::Event::Key(key), editor);
        } else {
            let Some(session) = self.writers.get_mut(&id) else {
                return;
            };
            let page = session.editor_rows.max(1) as usize;
            let width = session.editor_cols as usize;
            let goal = session.nav_goal;
            let Some(editor) = session.editor.as_mut() else {
                return;
            };
            // The gesture anchor: stored while one runs, the cursor
            // when one starts, or the end opposite the cursor when
            // extending a mouse-made selection.
            let cursor_off = index2_to_offset(&editor.lines, editor.cursor);
            let selecting = key.modifiers == shift || key.modifiers == shift_ctrl;
            let anchor = match session.sel_anchor {
                Some(anchor) => anchor,
                None => match editor.selection.as_ref() {
                    Some(sel) => {
                        let start = index2_to_offset(&editor.lines, sel.start());
                        let end = index2_to_offset(&editor.lines, sel.end()) + 1;
                        if cursor_off == start {
                            end
                        } else {
                            start
                        }
                    }
                    None => cursor_off,
                },
            };
            if selecting {
                session.sel_anchor = Some(anchor);
                if editor.mode != EditorMode::Visual {
                    editor.execute(SwitchMode(EditorMode::Visual));
                }
            }
            if let Some(motion) = nav {
                let buffer = editor.lines.to_string();
                let total = buffer.chars().count();
                let vertical = matches!(
                    motion,
                    NavMove::Up | NavMove::Down | NavMove::PageUp | NavMove::PageDown
                );
                if vertical {
                    let delta = match motion {
                        NavMove::Up => -1,
                        NavMove::Down => 1,
                        NavMove::PageUp => -(page as isize),
                        _ => page as isize,
                    };
                    let (target, kept) =
                        vertical_target(&buffer, total, cursor_off, width, delta, goal);
                    editor.cursor = offset_to_index2(&buffer, target);
                    session.nav_goal = Some(kept);
                } else {
                    let target = nav_target(&buffer, total, cursor_off, motion);
                    editor.cursor = offset_to_index2(&buffer, target);
                    // Any horizontal move drops the visual-column goal.
                    session.nav_goal = None;
                }
            }
            if !selecting {
                // Plain moves collapse, the way every text field
                // does, and land back in Insert: navigation must
                // never strand the editor in a modal state.
                editor.selection = None;
                editor.mode = EditorMode::Insert;
                session.sel_anchor = None;
            } else {
                // Text-field semantics: N presses select N chars. EdTUI
                // visual counts the anchor char too, so the far end moves
                // back one char; meeting the anchor collapses to a cursor.
                let buffer = editor.lines.to_string();
                let total = buffer.chars().count();
                let anchor = anchor.min(total);
                let cursor = index2_to_offset(&editor.lines, editor.cursor).min(total);
                if cursor == anchor {
                    // Back at the start: no selection, not a zero-width
                    // value (which now reads as one char). Leave Visual
                    // too, so the next Shift gesture re-enters it and
                    // gets a fresh Selection instead of moving a None.
                    editor.selection = None;
                    editor.mode = EditorMode::Insert;
                } else if let Some(sel) = editor.selection.as_mut() {
                    let (low, high) = if cursor > anchor {
                        (anchor, cursor - 1)
                    } else {
                        (cursor, anchor - 1)
                    };
                    sel.start = offset_to_index2(&buffer, low);
                    sel.end = offset_to_index2(&buffer, high);
                }
            }
        }
        self.writer_sync_editor(id);
        self.dirty = true;
    }

    /// Feed one terminal mouse event to the editor. Drags select and
    /// the wheel scrolls through EdTUI; EdTUI ignores events that land
    /// outside its painted area or arrive before the first paint.
    pub fn writer_feed_mouse(&mut self, id: crate::session::SessionId, mouse: crossterm::event::MouseEvent) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        let Some(editor) = session.editor.as_mut() else {
            return;
        };
        let handler = EditorEventHandler::emacs_mode();
        handler.on_mouse_event(mouse, editor);
        // Clicks and clean releases land back in Insert: EdTUI parks
        // Down in vim Normal while a Visual selection is live, and
        // nothing would ever return after that. Drags keep EdTUI's
        // Visual and its growing selection untouched.
        match mouse.kind {
            crossterm::event::MouseEventKind::Down(_) => {
                editor.mode = EditorMode::Insert;
            }
            crossterm::event::MouseEventKind::Up(_) => {
                if editor.selection.is_none() {
                    editor.mode = EditorMode::Insert;
                }
            }
            _ => {}
        }
        session.sel_anchor = None;
        session.nav_goal = None;
        self.writer_sync_editor(id);
        self.dirty = true;
    }

    /// Diff the buffer against the document and apply the change.
    /// The document stays the single source of truth; proposals hear
    /// about every edit exactly once, whether typed or accepted.
    fn writer_sync_editor(&mut self, id: crate::session::SessionId) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        let Some(editor) = session.editor.as_ref() else {
            return;
        };
        let editor_text = editor.lines.to_string();
        session.selection = editor_selection_to_range(editor);
        let Some(doc) = session.doc.as_mut() else {
            return;
        };
        if editor_text == doc.text {
            return;
        }
        let Some((range, replacement)) = changed_range(&doc.text, &editor_text) else {
            return;
        };
        if doc.apply_edit(range.clone(), &replacement).is_err() {
            return;
        }
        session.proposals.on_edit(&range);
    }
    /// Open the typed-path prompt: `create` from `(*New document)`,
    /// otherwise from `(Open…)`. Typing is bounded; Enter submits.
    pub fn writer_prompt_open(&mut self, id: crate::session::SessionId, create: bool) {
        // Entry API: the empty state has no writer entry yet, and the
        // prompt is exactly how one comes to exist.
        let session = self.writers.entry(id).or_default();
        session.open_prompt = Some(WriterOpenPrompt {
            buffer: String::new(),
            create,
        });
        session.error = None;
        self.dirty = true;
    }

    /// Type one char into the prompt, bounded at [`MAX_PATH_CHARS`].
    /// Control chars never enter a path.
    pub fn writer_prompt_char(&mut self, id: crate::session::SessionId, c: char) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        let Some(prompt) = session.open_prompt.as_mut() else {
            return;
        };
        if c.is_control() || prompt.buffer.chars().count() >= MAX_PATH_CHARS {
            return;
        }
        prompt.buffer.push(c);
        self.dirty = true;
    }

    /// Backspace one char out of the prompt; an empty prompt stays open
    /// (Esc cancels it).
    pub fn writer_prompt_backspace(&mut self, id: crate::session::SessionId) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        let Some(prompt) = session.open_prompt.as_mut() else {
            return;
        };
        prompt.buffer.pop();
        self.dirty = true;
    }

    /// Cancel the prompt without opening anything.
    pub fn writer_prompt_cancel(&mut self, id: crate::session::SessionId) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        session.open_prompt = None;
        self.dirty = true;
    }

    /// Submit the prompt: same scope rules as the agent tool. Success
    /// closes the prompt and builds the editor; failure keeps the
    /// prompt open with the reason in the fixed error slot.
    pub fn writer_submit_open(&mut self, id: crate::session::SessionId) {
        let path = match self.writers.get(&id).and_then(|s| s.open_prompt.as_ref()) {
            Some(prompt) => prompt.buffer.clone(),
            None => return,
        };
        if path.trim().is_empty() {
            self.writer_fail(id, "type a path first");
            return;
        }
        let cwd = self
            .manager
            .get(id)
            .map(|rec| rec.cwd.clone())
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        let opened = self
            .writers
            .get_mut(&id)
            .expect("checked above")
            .open_document_path(&cwd, path.trim());
        match opened {
            Ok(_) => {
                let Some(session) = self.writers.get_mut(&id) else {
                    return;
                };
                session.open_prompt = None;
                session.error = None;
                self.dirty = true;
                self.writer_open_editor(id);
            }
            Err(message) => self.writer_fail(id, &message),
        }
    }

    /// Cycle keyboard focus Editor → Chat → Thread → Editor.
    pub fn writer_cycle_focus(&mut self, id: crate::session::SessionId) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        session.focus = match session.focus {
            WriterFocus::Editor => WriterFocus::Chat,
            WriterFocus::Chat => WriterFocus::Thread,
            WriterFocus::Thread => WriterFocus::Editor,
        };
        self.dirty = true;
    }

    /// Type one char into the chat box, bounded at
    /// [`crate::writer::MAX_INPUT_CHARS`]. Control chars never enter.
    pub fn writer_chat_char(&mut self, id: crate::session::SessionId, c: char) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        if c.is_control() {
            return;
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

    /// Backspace one char before the chat cursor.
    pub fn writer_chat_backspace(&mut self, id: crate::session::SessionId) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
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

    /// Move the chat cursor one char, clamped to the input.
    pub fn writer_chat_move(&mut self, id: crate::session::SessionId, dir: i32) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        let len = session.chat_input.chars().count();
        session.chat_cursor = (session.chat_cursor as i32 + dir).clamp(0, len as i32) as usize;
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
