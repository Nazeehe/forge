//! Writer EdTUI adapter: editor ownership, buffer sync, selection,
//! accept/reject, requests, and the delivery queue flush.
//!
//! The domain `Document` is the single source of truth. After every
//! editor event the adapter diffs the buffer against it (common
//! prefix/suffix), applies the change through `doc.apply_edit`, and
//! notifies `proposals.on_edit`. Accept/Reject run through exactly one
//! method each so no caller can skip a step.

use super::*;
use edtui::{
    actions::{
        DeleteSelection, InsertChar, MoveDown, MoveUp, MoveWordBackward, MoveWordForward,
        SwitchMode,
    },
    clipboard::ClipboardTrait,
    EditorEventHandler, EditorMode, EditorState, Index2, Lines,
};

/// Most outbound requests awaiting the settle flush; never drops.
pub const MAX_WRITER_QUEUE: usize = 8;

/// Forge-owned clipboard backing the editor: Accept saves and restores
/// the user's copied text around the DeleteSelection+InsertChar replace,
/// which EdTUI would otherwise clobber with the drained range.
#[derive(Clone, Default)]
pub struct SharedClipboard(pub std::rc::Rc<std::cell::RefCell<String>>);

impl ClipboardTrait for SharedClipboard {
    fn set_text(&mut self, text: String) {
        *self.0.borrow_mut() = text;
    }

    fn get_text(&mut self) -> String {
        self.0.borrow().clone()
    }
}

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
        self.dirty = true;
    }

    /// Feed one terminal key to the editor and sync back. Shift+arrows
    /// (and Shift+Ctrl+arrows by word) are adapter-owned selection and
    /// never reach EdTUI; everything else is forwarded as-is.
    pub fn writer_feed_key(&mut self, id: crate::session::SessionId, key: crossterm::event::KeyEvent) {
        use crossterm::event::{KeyCode, KeyModifiers};
        let shift = KeyModifiers::SHIFT;
        let shift_ctrl = KeyModifiers::SHIFT | KeyModifiers::CONTROL;
        let shifted = if key.modifiers == shift {
            match key.code {
                KeyCode::Left => Some((false, false)),
                KeyCode::Right => Some((true, false)),
                KeyCode::Up => None,
                KeyCode::Down => None,
                _ => None,
            }
        } else if key.modifiers == shift_ctrl {
            match key.code {
                KeyCode::Left => Some((false, true)),
                KeyCode::Right => Some((true, true)),
                _ => None,
            }
        } else {
            None
        };
        // Vertical Shift selection shares the same anchor logic.
        let vertical = key.modifiers == shift
            && matches!(key.code, KeyCode::Up | KeyCode::Down);
        if shifted.is_none() && !vertical {
            let Some(session) = self.writers.get_mut(&id) else {
                return;
            };
            let Some(editor) = session.editor.as_mut() else {
                return;
            };
            // Any other key ends the keyboard-selection gesture; a
            // plain arrow also collapses the selection first, the way
            // every text field does.
            session.sel_anchor = None;
            if key.modifiers == KeyModifiers::empty()
                && matches!(
                    key.code,
                    KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down
                )
            {
                editor.selection = None;
                if editor.mode == EditorMode::Visual {
                    editor.mode = EditorMode::Insert;
                }
            }
            let mut handler = EditorEventHandler::emacs_mode();
            handler.on_event(crossterm::event::Event::Key(key), editor);
        } else {
            let Some(session) = self.writers.get_mut(&id) else {
                return;
            };
            let Some(editor) = session.editor.as_mut() else {
                return;
            };
            // The gesture anchor: stored while one runs, the cursor
            // when one starts, or the end opposite the cursor when
            // extending a mouse-made selection.
            let cursor_off = index2_to_offset(&editor.lines, editor.cursor);
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
            session.sel_anchor = Some(anchor);
            if editor.mode != EditorMode::Visual {
                editor.execute(SwitchMode(EditorMode::Visual));
            }
            if vertical {
                if key.code == KeyCode::Up {
                    editor.execute(MoveUp(1));
                } else {
                    editor.execute(MoveDown(1));
                }
            } else if let Some((forward, word)) = shifted {
                match (forward, word) {
                    // Char motions move by char offset, not by EdTUI
                    // motion: MoveForward/MoveBackward stop at line
                    // ends and can never cross `\n`. Word and vertical
                    // motions already cross lines on their own.
                    (true, false) | (false, false) => {
                        let buffer = editor.lines.to_string();
                        let total = buffer.chars().count();
                        let off = index2_to_offset(&editor.lines, editor.cursor);
                        let next = if forward {
                            off.saturating_add(1).min(total)
                        } else {
                            off.saturating_sub(1)
                        };
                        editor.cursor = offset_to_index2(&buffer, next);
                    }
                    (true, true) => editor.execute(MoveWordForward(1)),
                    (false, true) => editor.execute(MoveWordBackward(1)),
                }
            }
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
        session.sel_anchor = None;
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

    /// Accept one pending proposal: domain replace, then the same range
    /// in the editor as a single undo step with the clipboard intact.
    /// When the buffer drifted since the proposal, the buffer is rebuilt
    /// from the document instead (correctness first; the one-undo path
    /// needs identical text).
    /// `pub(crate)`: the TUI input layer calls this directly.
    pub(crate) fn writer_accept(
        &mut self,
        id: crate::session::SessionId,
        proposal_id: u64,
    ) -> Result<(), String> {
        let session = self
            .writers
            .get_mut(&id)
            .ok_or_else(|| "no Writer document open for this session".to_string())?;
        if session.doc.is_none() {
            return Err("no Writer document open for this session".to_string());
        }
        let (range, text) = match session.proposals.get(proposal_id) {
            Some(proposal) => (proposal.range.clone(), proposal.text.clone()),
            None => return Err(format!("unknown proposal {proposal_id}")),
        };
        let pre_text = session.doc.as_ref().expect("checked above").text.clone();
        let doc = session.doc.as_mut().expect("checked above");
        session
            .proposals
            .accept(doc, proposal_id)
            .map_err(|e| e.to_string())?;
        if session.editor.is_some() {
            let buffer_matches = session
                .editor
                .as_ref()
                .expect("checked above")
                .lines
                .to_string()
                == pre_text;
            if buffer_matches {
                let editor = session.editor.as_mut().expect("checked above");
                let clip = session.clip.clone();
                editor_apply_accept(editor, &clip, range.clone(), &text);
            } else {
                let fresh = session.doc.as_ref().expect("checked above").text.clone();
                let editor = session.editor.as_mut().expect("checked above");
                editor_rebuild(editor, &fresh, range.start);
            }
            // Belt and braces: the buffer must equal the doc after any
            // replace. A diverged buffer is rebuilt from the doc, never
            // diffed back into it by the next sync. The cursor lands at
            // the end of the inserted text either way, so typing
            // continues where the accept left off.
            let authoritative = session.doc.as_ref().expect("checked above").text.clone();
            let matches = session
                .editor
                .as_ref()
                .expect("checked above")
                .lines
                .to_string()
                == authoritative;
            if !matches {
                let editor = session.editor.as_mut().expect("checked above");
                editor_rebuild(editor, &authoritative, range.start + text.chars().count());
            }
            session.selection = editor_selection_to_range(session.editor.as_ref().expect("checked above"));
        }
        session.sel_anchor = None;
        session.selected_proposal = None;
        session.evict_finished();
        self.dirty = true;
        Ok(())
    }

    /// Reject one pending or stale proposal; the editor is untouched.
    /// `pub(crate)`: the TUI input layer calls this directly.
    pub(crate) fn writer_reject(
        &mut self,
        id: crate::session::SessionId,
        proposal_id: u64,
    ) -> Result<(), String> {
        let session = self
            .writers
            .get_mut(&id)
            .ok_or_else(|| "no Writer document open for this session".to_string())?;
        session
            .proposals
            .reject(proposal_id)
            .map_err(|e| e.to_string())?;
        if session.selected_proposal == Some(proposal_id) {
            session.selected_proposal = None;
        }
        session.evict_finished();
        self.dirty = true;
        Ok(())
    }

    /// Rephrase the live selection, or the paragraph under the cursor.
    /// Sends no instruction and leaves the chat draft untouched: a
    /// half-typed message must never ride along and be lost.
    /// `pub(crate)`: the TUI input layer calls this directly.
    pub(crate) fn writer_rephrase(&mut self, id: crate::session::SessionId) {
        self.writer_send_request(id, WriterAction::Rephrase, String::new());
    }

    /// Chat over the live selection, or the whole document when there
    /// is none. The chat box is the instruction and clears on send.
    /// `pub(crate)`: the TUI input layer calls this directly.
    pub(crate) fn writer_chat_send(&mut self, id: crate::session::SessionId) {
        let instruction = self
            .writers
            .get(&id)
            .map(|session| session.chat_input.clone())
            .unwrap_or_default();
        if instruction.trim().is_empty() {
            self.writer_fail(id, "type a message first");
            return;
        }
        self.writer_send_request(id, WriterAction::Chat, instruction);
    }

    /// Build a request over the live editor state and queue its markup
    /// for the settle flush. Order matters: live-tab and queue-full
    /// checks first (no orphans), then the record, then the body.
    fn writer_send_request(
        &mut self,
        id: crate::session::SessionId,
        action: WriterAction,
        instruction: String,
    ) {
        if !self.writer_agent_live(id) {
            self.writer_fail(id, "no live agent tab");
            return;
        }
        let queue_full = self
            .writers
            .get(&id)
            .map(|session| session.queue.len() >= MAX_WRITER_QUEUE)
            .unwrap_or(false);
        if queue_full {
            self.writer_fail(id, "request queue full (8); wait for delivery");
            return;
        }
        let range = match self.writer_request_range(id, action) {
            Some(range) => range,
            None => {
                self.writer_fail(id, "place the cursor in a paragraph or select text");
                return;
            }
        };
        let markup = {
            let session = match self.writers.get_mut(&id) {
                Some(session) => session,
                None => return,
            };
            let doc = match session.doc.as_ref() {
                Some(doc) => doc,
                None => return,
            };
            let rev = doc.revision;
            let doc_text = doc.text.clone();
            let doc_name = Self::writer_doc_name(&doc.path_rel);
            let request_id = match session.new_request(action, &doc_text, range.clone(), rev) {
                Ok(request_id) => request_id,
                Err(message) => {
                    session.error = Some(message);
                    self.dirty = true;
                    return;
                }
            };
            match crate::writer::request::WriterRequest::over_range(
                request_id,
                action,
                &doc_name,
                &doc_text,
                range,
                rev,
                &instruction,
            ) {
                Ok(request) => request.markup(),
                Err(error) => {
                    // Withdraw the record: no body, no orphan.
                    session.cancel_request(request_id);
                    session.error = Some(error.to_string());
                    self.dirty = true;
                    return;
                }
            }
        };
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        session.queue.push_back(markup);
        if action == WriterAction::Chat {
            session.chat_input.clear();
            session.chat_cursor = 0;
        }
        session.error = None;
        self.dirty = true;
    }

    /// Resolve the range for a human request from the live editor:
    /// the selection, the paragraph under the cursor for actions, or
    /// the whole document for chat.
    fn writer_request_range(
        &self,
        id: crate::session::SessionId,
        action: WriterAction,
    ) -> Option<std::ops::Range<usize>> {
        let session = self.writers.get(&id)?;
        let editor = session.editor.as_ref()?;
        let doc = session.doc.as_ref()?;
        if let Some(selected) = editor_selection_to_range(editor) {
            return Some(selected);
        }
        let cursor = index2_to_offset(&editor.lines, editor.cursor);
        match action {
            WriterAction::Chat => Some(0..doc.text.chars().count()),
            _ => crate::writer::paragraph_at(&doc.text, cursor),
        }
    }

    /// File name for the request header: the jail-relative path's final
    /// component, so the markup never carries directories.
    fn writer_doc_name(path_rel: &str) -> String {
        std::path::Path::new(path_rel)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    /// Record a fixed-slot error without touching anything else.
    fn writer_fail(&mut self, id: crate::session::SessionId, message: &str) {
        if let Some(session) = self.writers.get_mut(&id) {
            session.error = Some(message.to_string());
        }
        self.dirty = true;
    }

    /// True while the session looks deliverable to: the record exists,
    /// the session hasn't exited, and it has an agent tab. A fast
    /// pre-check only — the flush path re-gates on the agent's live
    /// state and holds (never drops) when the pane write fails.
    fn writer_agent_live(&self, id: crate::session::SessionId) -> bool {
        let Some(rec) = self.manager.get(id) else {
            return false;
        };
        rec.state.is_live()
            && rec
                .tabs
                .iter()
                .any(|tab| tab.kind == crate::session::TabKind::Agent)
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

    /// Flush queued request bodies under the same gate as comms:
    /// agent Idle/Stopped, settled hands and hooks, no staged Enter.
    /// Inject failures keep the body queued for the next tick.
    /// `pub(crate)`: the settle path calls this every tick.
    pub(crate) fn settle_writer_queues(&mut self, now: std::time::Instant) {
        use crate::session::Activity;
        let ids: Vec<crate::session::SessionId> = self
            .writers
            .iter()
            .filter(|(_, session)| !session.queue.is_empty())
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            let Some(rec) = self.manager.get(id) else {
                continue;
            };
            if !matches!(rec.activity, Activity::Idle | Activity::Stopped) {
                continue;
            }
            if self.pending_enter.contains_key(&id) {
                continue;
            }
            if !self.turn_registered(id, now) || !self.injection_settled_for(id, now) {
                continue;
            }
            let body = match self.writers.get(&id).and_then(|s| s.queue.front().cloned()) {
                Some(body) => body,
                None => continue,
            };
            // Body only: the staged Enter goes out on a later tick
            // through settle_enters, never in the same burst as the
            // text — the same split the broker delivery uses.
            match self.manager.inject_write(id, body.as_bytes()) {
                Ok(()) => {
                    if let Some(session) = self.writers.get_mut(&id) {
                        session.queue.pop_front();
                    }
                    self.pending_enter.insert(id, (now, None));
                    self.dirty = true;
                }
                Err(_) => {
                    // Pane went away mid-tick: hold for the next one.
                    continue;
                }
            }
        }
    }

    /// Save through S2; conflicts land in the fixed error slot.
    /// `pub(crate)`: the TUI input layer calls this directly.
    pub(crate) fn writer_save(&mut self, id: crate::session::SessionId) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        let Some(doc) = session.doc.as_mut() else {
            session.error = Some("no document open".to_string());
            self.dirty = true;
            return;
        };
        match doc.save() {
            Ok(()) => {
                session.error = None;
            }
            Err(crate::writer::WriterError::ConflictOnSave) => {
                session.error = Some("save conflict: file changed on disk".to_string());
            }
            Err(other) => {
                session.error = Some(format!("save failed: {other}"));
            }
        }
        self.dirty = true;
    }
}

/// Rebuild the editor buffer from the document text: cursor near the
/// accept, no selection, Insert mode. Used for drift and as the
/// never-diverge backstop after a replace.
fn editor_rebuild(editor: &mut EditorState, text: &str, cursor_off: usize) {
    editor.lines = Lines::from(text);
    let total = text.chars().count();
    editor.cursor = offset_to_index2(text, cursor_off.min(total));
    editor.selection = None;
    editor.mode = EditorMode::Insert;
}

/// Replace an accepted range inside the editor buffer: DeleteSelection
/// plus one InsertChar per replacement char is exactly one undo step
/// (only DeleteSelection captures). The Forge-owned clipboard is saved
/// and restored so Accept never clobbers what the user copied. Empty
/// ranges capture through an Insert-mode re-entry instead, since there
/// is nothing to delete.
fn editor_apply_accept(
    editor: &mut EditorState,
    clip: &SharedClipboard,
    range: std::ops::Range<usize>,
    text: &str,
) {
    let saved = clip.0.borrow().clone();
    let buffer = editor.lines.to_string();
    editor.cursor = offset_to_index2(&buffer, range.start);
    editor.selection = None;
    if range.is_empty() {
        editor.mode = EditorMode::Normal;
        editor.execute(SwitchMode(EditorMode::Insert));
        for c in text.chars() {
            editor.execute(InsertChar(c));
        }
    } else {
        // The selection is set directly from the char offsets:
        // EdTUI motions stop at line ends and can never span `\n`.
        // (EdTUI's extract still eats a line-final break, so ranges
        // ending at a line end rebuild below; the doc stays exact.)
        editor.cursor = offset_to_index2(&buffer, range.start);
        editor.execute(SwitchMode(EditorMode::Visual));
        if let Some(sel) = editor.selection.as_mut() {
            sel.start = offset_to_index2(&buffer, range.start);
            sel.end = offset_to_index2(&buffer, range.end.saturating_sub(1).max(range.start));
        }
        editor.execute(DeleteSelection);
        for c in text.chars() {
            editor.execute(InsertChar(c));
        }
    }
    editor.mode = EditorMode::Insert;
    *clip.0.borrow_mut() = saved;
}

/// The inclusive→exclusive conversion, in one place: EdTUI selections
/// address both ends inclusively, while the domain counts the end
/// exclusive — so start == end still covers exactly one char. A true
/// cursor is `selection = None`, never a zero-width value.
pub fn editor_selection_to_range(editor: &EditorState) -> Option<std::ops::Range<usize>> {
    let selection = editor.selection.as_ref()?;
    let lines = &editor.lines;
    let start = index2_to_offset(lines, selection.start());
    let end = index2_to_offset(lines, selection.end());
    let total = lines.to_string().chars().count();
    if start > end || start >= total {
        return None;
    }
    Some(start..(end + 1).min(total))
}

/// Char offset of the live editor cursor: the paint layer's anchor
/// for gutter marks and the 1-based line:col status.
pub fn editor_cursor_offset(editor: &EditorState) -> usize {
    index2_to_offset(&editor.lines, editor.cursor)
}

/// Char offset of an EdTUI cursor over the buffer lines. Columns past
/// the line end clamp to it; rows past the buffer clamp to the end.
fn index2_to_offset(lines: &Lines, index: Index2) -> usize {
    use edtui::RowIndex;
    let mut offset = 0;
    for row in 0..index.row {
        let len = lines.get(RowIndex::new(row)).map(|line| line.len()).unwrap_or(0);
        offset += len + 1;
    }
    let current = lines
        .get(RowIndex::new(index.row))
        .map(|line| line.len())
        .unwrap_or(0);
    offset + index.col.min(current)
}

/// EdTUI cursor for a char offset over plain text. Offsets past the end
/// clamp to the last position.
pub fn offset_to_index2(text: &str, offset: usize) -> Index2 {
    let mut row = 0;
    let mut col = 0;
    for (index, c) in text.chars().enumerate() {
        if index == offset {
            break;
        }
        if c == '\n' {
            row += 1;
            col = 0;
        } else {
            col += 1;
        }
    }
    Index2::new(row, col)
}

/// Minimal changed span: common prefix/suffix in chars. The range
/// addresses the old text; the string is the replacement.
fn changed_range(old: &str, new: &str) -> Option<(std::ops::Range<usize>, String)> {
    if old == new {
        return None;
    }
    let old_chars: Vec<char> = old.chars().collect();
    let new_chars: Vec<char> = new.chars().collect();
    let mut prefix = 0;
    while prefix < old_chars.len()
        && prefix < new_chars.len()
        && old_chars[prefix] == new_chars[prefix]
    {
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < old_chars.len() - prefix
        && suffix < new_chars.len() - prefix
        && old_chars[old_chars.len() - 1 - suffix] == new_chars[new_chars.len() - 1 - suffix]
    {
        suffix += 1;
    }
    let range = prefix..(old_chars.len() - suffix);
    let replacement: String = new_chars[prefix..(new_chars.len() - suffix)].iter().collect();
    Some((range, replacement))
}
