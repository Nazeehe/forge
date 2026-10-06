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
    EditorMode, EditorState, Lines,
};

impl AppState {
    /// (Re)create the editor from the open document: the CUA
    /// register with Insert mode at open, Forge clipboard from the start.
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
    /// keys move only the viewport, and the CUA register binds text
    /// entry only. Everything else is forwarded as-is.
    pub fn writer_feed_key(&mut self, id: crate::session::SessionId, key: crossterm::event::KeyEvent) {
        use crossterm::event::{KeyCode, KeyModifiers};
        // Any key ends the multi-click chain: a later press starts
        // over at a single click.
        if let Some(session) = self.writers.get_mut(&id) {
            session.press_count = 0;
            session.last_press = None;
        }
        // Esc always returns the editor to the known-good state: no
        // selection, Insert mode. EdTUI would park in vim Normal
        // (mouse Down while Visual strands there) with nothing ever
        // returning, so the adapter owns this.
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
        // CUA text ops, above navigation: Tab indents, Ctrl+A selects,
        // Ctrl+Backspace/Delete kill words. They need adapter
        // selection/offset state, so they never reach the register.
        match (key.code, key.modifiers) {
            (KeyCode::Tab, KeyModifiers::NONE) => {
                self.writer_indent(id);
                return;
            }
            (KeyCode::Tab, KeyModifiers::SHIFT) => {
                self.writer_outdent(id);
                return;
            }
            (KeyCode::Char('a'), KeyModifiers::CONTROL) => {
                self.writer_select_all(id);
                return;
            }
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                self.writer_clip_copy(id);
                return;
            }
            (KeyCode::Char('x'), KeyModifiers::CONTROL) => {
                self.writer_clip_cut(id);
                return;
            }
            (KeyCode::Char('v'), KeyModifiers::CONTROL) => {
                self.writer_clip_paste(id);
                return;
            }
            (KeyCode::Backspace, KeyModifiers::CONTROL) => {
                self.writer_delete_word(id, true);
                return;
            }
            (KeyCode::Delete, KeyModifiers::CONTROL) => {
                self.writer_delete_word(id, false);
                return;
            }
            _ => {}
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
            // first (CUA): the register has no Visual-char behavior,
            // so the adapter deletes, then forwards the key for the
            // normal insert. (Two undo steps for now; E6 groups them.)
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
            // Only convertible codes reach the register: EdTUI's
            // crossterm conversion panics on the rest (F-keys, media,
            // …), so anything else dies here after ending the gesture.
            if !matches!(
                key.code,
                KeyCode::Char(_)
                    | KeyCode::Enter
                    | KeyCode::Backspace
                    | KeyCode::Delete
            ) {
                self.writer_sync_editor(id);
                self.dirty = true;
                return;
            }
            let mut handler = super::cua::cua_handler();
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
        let handler = super::cua::cua_handler();
        // One wheel notch scrolls three lines: EdTUI moves a single
        // line per event, matching the PTY panes and the tour.
        let scrolls = match mouse.kind {
            crossterm::event::MouseEventKind::ScrollUp
            | crossterm::event::MouseEventKind::ScrollDown => 3,
            _ => 1,
        };
        for _ in 0..scrolls {
            handler.on_mouse_event(mouse, editor);
        }
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
    pub(super) fn writer_sync_editor(&mut self, id: crate::session::SessionId) {
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
        let edit_line = crate::app::writer::WriterSession::line_of_offset(&doc.text, range.start);
        if doc.apply_edit(range.clone(), &replacement).is_err() {
            return;
        }
        session.note_fence_edit(edit_line);
        session.proposals.on_edit(&range);
    }
    /// Open the typed-path prompt for New / Open / Save-as (E2b:
    /// one prompt component, three kinds; submit routes on the kind).
    /// Entry API: the empty state has no writer entry yet, and the
    /// prompt is exactly how one comes to exist. Opening a prompt
    /// clears any pending confirm: one actionable thing at a time.
    pub fn writer_prompt_open(
        &mut self,
        id: crate::session::SessionId,
        kind: crate::app::writer::PromptKind,
    ) {
        let session = self.writers.entry(id).or_default();
        session.open_prompt = Some(WriterOpenPrompt {
            buffer: String::new(),
            kind,
            cursor: 0,
            select_all: false,
        });
        session.pending_confirm = None;
        session.error = None;
        self.dirty = true;
    }

    /// Type one char into the prompt at the cursor, bounded at
    /// [`MAX_PATH_CHARS`]. Control chars never enter a path. A
    /// select-all replaces the whole buffer (type-to-replace).
    pub fn writer_prompt_char(&mut self, id: crate::session::SessionId, c: char) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        let Some(prompt) = session.open_prompt.as_mut() else {
            return;
        };
        if c.is_control() {
            return;
        }
        if prompt.select_all {
            prompt.buffer.clear();
            prompt.cursor = 0;
            prompt.select_all = false;
        }
        if prompt.buffer.chars().count() >= MAX_PATH_CHARS {
            return;
        }
        let at = prompt.cursor.min(prompt.buffer.chars().count());
        let byte = prompt
            .buffer
            .char_indices()
            .nth(at)
            .map(|(index, _)| index)
            .unwrap_or(prompt.buffer.len());
        prompt.buffer.insert(byte, c);
        prompt.cursor = at + 1;
        self.dirty = true;
    }

    /// Backspace one char before the prompt cursor; an empty prompt
    /// stays open (Esc cancels it). A select-all clears the buffer.
    pub fn writer_prompt_backspace(&mut self, id: crate::session::SessionId) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        let Some(prompt) = session.open_prompt.as_mut() else {
            return;
        };
        if prompt.select_all {
            prompt.buffer.clear();
            prompt.cursor = 0;
            prompt.select_all = false;
            self.dirty = true;
            return;
        }
        if prompt.cursor == 0 {
            return;
        }
        let mut chars: Vec<char> = prompt.buffer.chars().collect();
        if prompt.cursor <= chars.len() {
            chars.remove(prompt.cursor - 1);
            prompt.buffer = chars.into_iter().collect();
            prompt.cursor -= 1;
            self.dirty = true;
        }
    }

    /// Delete one char under the prompt cursor. A select-all clears.
    pub fn writer_prompt_delete(&mut self, id: crate::session::SessionId) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        let Some(prompt) = session.open_prompt.as_mut() else {
            return;
        };
        if prompt.select_all {
            prompt.buffer.clear();
            prompt.cursor = 0;
            prompt.select_all = false;
            self.dirty = true;
            return;
        }
        let mut chars: Vec<char> = prompt.buffer.chars().collect();
        if prompt.cursor < chars.len() {
            chars.remove(prompt.cursor);
            prompt.buffer = chars.into_iter().collect();
            self.dirty = true;
        }
    }

    /// Move the prompt cursor one char, clamped. Drops a select-all.
    pub fn writer_prompt_move(&mut self, id: crate::session::SessionId, dir: i32) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        let Some(prompt) = session.open_prompt.as_mut() else {
            return;
        };
        let len = prompt.buffer.chars().count();
        prompt.cursor = (prompt.cursor as i32 + dir).clamp(0, len as i32) as usize;
        prompt.select_all = false;
        self.dirty = true;
    }

    /// Prompt cursor to the head of the buffer. Drops a select-all.
    pub fn writer_prompt_home(&mut self, id: crate::session::SessionId) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        let Some(prompt) = session.open_prompt.as_mut() else {
            return;
        };
        prompt.cursor = 0;
        prompt.select_all = false;
        self.dirty = true;
    }

    /// Prompt cursor to the end of the buffer. Drops a select-all.
    pub fn writer_prompt_end(&mut self, id: crate::session::SessionId) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        let Some(prompt) = session.open_prompt.as_mut() else {
            return;
        };
        prompt.cursor = prompt.buffer.chars().count();
        prompt.select_all = false;
        self.dirty = true;
    }

    /// Prompt cursor one word in `dir` (-1/1). Word chars are
    /// alphanumeric plus underscore; anything else is a separator.
    pub fn writer_prompt_word(&mut self, id: crate::session::SessionId, dir: i32) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        let Some(prompt) = session.open_prompt.as_mut() else {
            return;
        };
        prompt.cursor = word_edge(&prompt.buffer, prompt.cursor, dir);
        prompt.select_all = false;
        self.dirty = true;
    }

    /// Mark the whole prompt buffer for type-to-replace.
    pub fn writer_prompt_select_all(&mut self, id: crate::session::SessionId) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        let Some(prompt) = session.open_prompt.as_mut() else {
            return;
        };
        prompt.select_all = !prompt.buffer.is_empty();
        self.dirty = true;
    }

    /// Place the prompt cursor at a char index (mouse click-to-place).
    /// Clamped; drops a select-all.
    pub fn writer_prompt_place(&mut self, id: crate::session::SessionId, at: usize) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        let Some(prompt) = session.open_prompt.as_mut() else {
            return;
        };
        prompt.cursor = at.min(prompt.buffer.chars().count());
        prompt.select_all = false;
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

    /// Submit the prompt: routes on the prompt kind. Success closes
    /// the prompt and builds the editor; failure (and actionable
    /// confirms) keep the prompt open with the reason in the fixed
    /// error slot.
    pub fn writer_submit_open(&mut self, id: crate::session::SessionId) {
        let (buffer, kind) = match self.writers.get(&id).and_then(|s| s.open_prompt.as_ref()) {
            Some(prompt) => (prompt.buffer.clone(), prompt.kind),
            None => return,
        };
        match kind {
            crate::app::writer::PromptKind::New => self.writer_submit_new(id, buffer.trim()),
            crate::app::writer::PromptKind::Open => {
                self.writer_submit_open_existing(id, buffer.trim());
            }
            crate::app::writer::PromptKind::SaveAs => {
                self.writer_submit_save_as(id, buffer.trim());
            }
        }
    }

    /// New-document submit: an existing path offers to open it instead
    /// of silently reusing the name; a missing path creates empty.
    fn writer_submit_new(&mut self, id: crate::session::SessionId, path: &str) {
        if path.is_empty() {
            self.writer_fail(id, "type a path first");
            return;
        }
        let cwd = super::requests::writer_cwd(self, id);
        let abs = match crate::infra::paths::confine(&cwd, std::path::Path::new(path)) {
            Ok(abs) => abs,
            Err(e) => {
                self.writer_fail(id, &e.to_string());
                return;
            }
        };
        if abs.is_file() {
            self.writer_raise_confirm(
                id,
                format!("{path} exists:"),
                vec![
                    crate::app::writer::ConfirmAction::OpenInstead(abs),
                    crate::app::writer::ConfirmAction::Cancel,
                ],
            );
            return;
        }
        // A missing path creates empty through the same confined open
        // the agent tool uses (Document::open maps missing → empty).
        self.writer_open_rel(id, path);
    }

    /// Open-document submit: a missing path offers to create it.
    fn writer_submit_open_existing(&mut self, id: crate::session::SessionId, path: &str) {
        if path.is_empty() {
            self.writer_fail(id, "type a path first");
            return;
        }
        let cwd = super::requests::writer_cwd(self, id);
        let abs = match crate::infra::paths::confine(&cwd, std::path::Path::new(path)) {
            Ok(abs) => abs,
            Err(e) => {
                self.writer_fail(id, &e.to_string());
                return;
            }
        };
        if !abs.is_file() {
            self.writer_raise_confirm(
                id,
                format!("no file {path}:"),
                vec![
                    crate::app::writer::ConfirmAction::CreateInstead(abs),
                    crate::app::writer::ConfirmAction::Cancel,
                ],
            );
            return;
        }
        self.writer_open_rel(id, path);
    }

    /// Save-as submit: needs an open doc, a Markdown extension (the
    /// same rule the opener enforces), and a non-identity target. An
    /// existing target offers Overwrite; a missing one saves at once.
    fn writer_submit_save_as(&mut self, id: crate::session::SessionId, path: &str) {
        let current = self
            .writers
            .get(&id)
            .and_then(|s| s.doc.as_ref())
            .map(|d| d.abs_path.clone());
        let Some(current) = current else {
            self.writer_fail(id, "no document open");
            return;
        };
        if path.is_empty() {
            self.writer_fail(id, "type a path first");
            return;
        }
        if !crate::writer::document::is_markdown(path) {
            self.writer_fail(id, &format!("{path}: only .md, .markdown or .txt"));
            return;
        }
        let cwd = super::requests::writer_cwd(self, id);
        let abs = match crate::infra::paths::confine(&cwd, std::path::Path::new(path)) {
            Ok(abs) => abs,
            Err(e) => {
                self.writer_fail(id, &e.to_string());
                return;
            }
        };
        if abs == current {
            self.writer_fail(id, &format!("already saved as {path}"));
            return;
        }
        if abs.is_file() {
            self.writer_raise_confirm(
                id,
                format!("{path} exists:"),
                vec![
                    crate::app::writer::ConfirmAction::Overwrite(abs),
                    crate::app::writer::ConfirmAction::Cancel,
                ],
            );
            return;
        }
        self.writer_save_as_to(id, path, abs);
    }

    /// Move the recent-list keyboard selection, clamped to the cache.
    /// `pub(crate)`: the TUI input layer calls this directly.
    pub(crate) fn writer_recent_move(&mut self, id: crate::session::SessionId, delta: i32) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        if session.recent_cache.is_empty() {
            session.recent_sel = 0;
        } else {
            let len = session.recent_cache.len();
            let next = (session.recent_sel as i32 + delta).clamp(0, len as i32 - 1);
            session.recent_sel = next as usize;
        }
        self.dirty = true;
    }

    /// Open the keyboard-selected recent row, if any.
    /// `pub(crate)`: the TUI input layer calls this directly.
    pub(crate) fn writer_recent_open(&mut self, id: crate::session::SessionId) {
        let sel = self.writers.get(&id).map(|s| s.recent_sel).unwrap_or(0);
        self.writer_open_recent_at(id, sel);
    }

    /// Open one cached recent row by index; out-of-range opens nothing.
    /// `pub(crate)`: mouse clicks land on rows, not the selection.
    pub(crate) fn writer_open_recent_at(&mut self, id: crate::session::SessionId, index: usize) {
        let rel = self
            .writers
            .get(&id)
            .and_then(|s| s.recent_cache.get(index))
            .map(|e| e.rel.clone());
        let Some(rel) = rel else {
            return;
        };
        if let Some(session) = self.writers.get_mut(&id) {
            session.recent_sel = index;
        }
        self.writer_open_rel(id, &rel);
    }

    /// Dismiss the topmost transient: the More menu first, then the
    /// pending confirm (a Cancel without firing). True when something
    /// dismissed, so keys fall through to their normal target
    /// otherwise. `pub(crate)`: the TUI input layer calls this directly.
    pub(crate) fn writer_dismiss_top(&mut self, id: crate::session::SessionId) -> bool {
        let Some(session) = self.writers.get_mut(&id) else {
            return false;
        };
        if session.more_open {
            session.more_open = false;
            self.dirty = true;
            return true;
        }
        if session.pending_confirm.is_some() {
            session.pending_confirm = None;
            self.dirty = true;
            return true;
        }
        false
    }

    /// Fill the prompt buffer with one suggestion (clicking a row).
    /// Bounded like typing; unknown sessions stay silent.
    /// `pub(crate)`: the TUI input layer calls this directly.
    pub(crate) fn writer_prompt_fill(&mut self, id: crate::session::SessionId, value: String) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        let Some(prompt) = session.open_prompt.as_mut() else {
            return;
        };
        prompt.buffer = value.chars().take(super::super::MAX_PATH_CHARS).collect();
        prompt.cursor = prompt.buffer.chars().count();
        prompt.select_all = false;
        self.dirty = true;
    }

    /// Open `rel` (confined, Markdown-gated by Document::open): record
    /// it as opened this run, rebuild the editor, close the prompt.
    /// Same-path re-opens and dirty-switch refusals surface as errors.
    /// `pub(crate)`: recent rows and confirms share it.
    pub(crate) fn writer_open_rel(&mut self, id: crate::session::SessionId, rel: &str) {
        let cwd = super::requests::writer_cwd(self, id);
        let abs = match crate::infra::paths::confine(&cwd, std::path::Path::new(rel)) {
            Ok(abs) => abs,
            Err(e) => {
                self.writer_fail(id, &e.to_string());
                return;
            }
        };
        let opened = match self.writers.get_mut(&id) {
            Some(session) => session.open_document_path(&cwd, rel),
            None => return,
        };
        match opened {
            Ok(_) => {
                if let Some(session) = self.writers.get_mut(&id) {
                    session.open_prompt = None;
                    session.pending_confirm = None;
                    session.error = None;
                }
                self.writer_note_opened(id, abs);
                self.writer_open_editor(id);
                self.dirty = true;
            }
            Err(message) => self.writer_fail(id, &message),
        }
    }

    /// Raise an actionable confirm into the fixed error slot. The
    /// prompt stays open underneath; Cancel returns to it.
    fn writer_raise_confirm(
        &mut self,
        id: crate::session::SessionId,
        message: String,
        actions: Vec<crate::app::writer::ConfirmAction>,
    ) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        session.pending_confirm = Some(crate::app::writer::PendingConfirm { message, actions });
        self.dirty = true;
    }

    /// Fire one confirm action by index; out-of-range clicks are
    /// ignored. The confirm clears first so every branch below lands
    /// with errors (if any) in the fixed slot, never stacked behind it.
    pub fn writer_fire_confirm(&mut self, id: crate::session::SessionId, index: usize) {
        // Out-of-range picks (and firing with no confirm) are ignored
        // with the row left up for a valid pick.
        let action = match self.writers.get_mut(&id) {
            Some(session) => match session.pending_confirm.take() {
                Some(mut confirm) if index < confirm.actions.len() => {
                    Some(confirm.actions.swap_remove(index))
                }
                kept => {
                    session.pending_confirm = kept;
                    None
                }
            },
            None => None,
        };
        self.dirty = true;
        let Some(action) = action else { return };
        match action {
            crate::app::writer::ConfirmAction::Cancel => {}
            crate::app::writer::ConfirmAction::OpenInstead(abs)
            | crate::app::writer::ConfirmAction::CreateInstead(abs) => {
                let rel = self.writer_rel_for(id, &abs);
                self.writer_open_rel(id, &rel);
            }
            crate::app::writer::ConfirmAction::Overwrite(abs) => {
                let rel = self.writer_rel_for(id, &abs);
                self.writer_save_as_to(id, &rel, abs);
            }
            crate::app::writer::ConfirmAction::SaveAndClose => {
                self.writer_save(id);
                let dirty = self
                    .writers
                    .get(&id)
                    .and_then(|s| s.doc.as_ref())
                    .is_some_and(|d| d.dirty);
                if !dirty {
                    self.writer_do_close(id);
                }
            }
            crate::app::writer::ConfirmAction::DiscardClose => self.writer_do_close(id),
        }
    }

    /// Session-relative display path for an absolute path; falls back
    /// to the full path when it escapes the session folder.
    fn writer_rel_for(&self, id: crate::session::SessionId, abs: &std::path::Path) -> String {
        let cwd = super::requests::writer_cwd(self, id);
        abs.strip_prefix(&cwd)
            .map(|rel| rel.to_string_lossy().into_owned())
            .unwrap_or_else(|_| abs.to_string_lossy().into_owned())
    }

    /// Tab completion over the prompt: first opened-this-run or recent
    /// path extending the typed prefix. A second Tab is stable (the
    /// filled value no longer extends), so completion never cycles.
    pub fn writer_prompt_complete(&mut self, id: crate::session::SessionId) {
        let prefix = match self.writers.get(&id).and_then(|s| s.open_prompt.as_ref()) {
            Some(prompt) => prompt.buffer.clone(),
            None => return,
        };
        let cwd = super::requests::writer_cwd(self, id);
        let mut seen = std::collections::HashSet::new();
        let mut hit: Option<String> = None;
        let opened: Vec<String> = self
            .writers
            .get(&id)
            .map(|s| {
                s.opened
                    .iter()
                    .filter_map(|abs| {
                        abs.strip_prefix(&cwd)
                            .ok()
                            .map(|rel| rel.to_string_lossy().into_owned())
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let recent: Vec<String> = self
            .writers
            .get(&id)
            .map(|s| s.recent_cache.iter().map(|e| e.rel.clone()).collect())
            .unwrap_or_default();
        // Live filesystem last: files the cache has not seen yet (open
        // races it), through the jail. The suggestion row previews only
        // the cache hits above; Tab additionally consults the disk.
        let live = crate::writer::recent::complete(&cwd, &prefix);
        for candidate in opened.into_iter().chain(recent).chain(live) {
            if !seen.insert(candidate.clone()) {
                continue;
            }
            if candidate.starts_with(prefix.as_str()) && candidate.len() > prefix.len() {
                hit = Some(candidate);
                break;
            }
        }
        if let (Some(hit), Some(session)) = (hit, self.writers.get_mut(&id)) {
            if let Some(prompt) = session.open_prompt.as_mut() {
                prompt.buffer = hit;
                prompt.cursor = prompt.buffer.chars().count();
                prompt.select_all = false;
                self.dirty = true;
            }
        }
    }

    /// Toolbar dispatch (E2b): every toolbar pill funnels through
    /// these, so mouse clicks and shortcut keys share one path.
    /// Toolbar `(*New document)`: the New-kind prompt.
    pub fn writer_toolbar_new(&mut self, id: crate::session::SessionId) {
        self.writer_prompt_open(id, crate::app::writer::PromptKind::New);
    }

    /// Toolbar `(Open…)`: the Open-kind prompt, with completion and
    /// the create-instead confirm.
    pub fn writer_toolbar_open(&mut self, id: crate::session::SessionId) {
        self.writer_prompt_open(id, crate::app::writer::PromptKind::Open);
    }

    /// Toolbar `(Save)`: plain S2 save; no doc is a fixed-slot error.
    pub fn writer_toolbar_save(&mut self, id: crate::session::SessionId) {
        self.writer_save(id);
    }

    /// Toolbar `(Save as)`: needs an open doc, otherwise the prompt
    /// would have nothing to write. The entry is created first so the
    /// refusal lands in the fixed error slot instead of vanishing.
    pub fn writer_toolbar_save_as(&mut self, id: crate::session::SessionId) {
        let has_doc = self.writers.entry(id).or_default().doc.is_some();
        if !has_doc {
            self.writer_fail(id, "no document open");
            return;
        }
        self.writer_prompt_open(id, crate::app::writer::PromptKind::SaveAs);
    }

    /// Toolbar `(Close)`: clean closes at once, dirty confirms.
    pub fn writer_toolbar_close(&mut self, id: crate::session::SessionId) {
        self.writer_close_doc(id);
    }

    /// Toolbar `(Assistant)` / `(Assistant*)`: the real E10 toggle.
    pub fn writer_toolbar_assistant(&mut self, id: crate::session::SessionId) {
        self.writer_toggle_assistant(id);
    }

    /// Toggle the narrow-mode More menu; firing a menu row closes it.
    /// `pub(crate)`: the TUI input layer calls this directly.
    pub(crate) fn writer_toggle_more(&mut self, id: crate::session::SessionId) {
        if let Some(session) = self.writers.get_mut(&id) {
            session.more_open = !session.more_open;
            self.dirty = true;
        }
    }

    /// Toggle the assistant panel. The panel is hidden by default and
    /// the editor takes the full width then; hiding returns focus to
    /// the editor, since the chat box lives in the panel.
    pub fn writer_toggle_assistant(&mut self, id: crate::session::SessionId) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        session.panel_visible = !session.panel_visible;
        if !session.panel_visible && session.focus != WriterFocus::Editor {
            session.focus = WriterFocus::Editor;
        }
        self.dirty = true;
    }

    /// Cycle keyboard focus Editor → Chat → Thread → Editor. Chat
    /// and Thread live in the assistant panel, so while it is hidden
    /// focus stays in the editor instead of moving somewhere invisible.
    pub fn writer_cycle_focus(&mut self, id: crate::session::SessionId) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        if !session.panel_visible {
            session.focus = WriterFocus::Editor;
            self.dirty = true;
            return;
        }
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
fn word_edge(text: &str, cursor: usize, dir: i32) -> usize {
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
