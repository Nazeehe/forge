//! `@@` auto-wrap (§5.1): the gesture state machine plus Alt+M.
//!
//! Typing `@` over a live selection arms the gesture (the text
//! stays, a hint shows); a second `@` wraps the selection as
//! `@@verb prompt@@<selection>@@` with the placeholder selected, in
//! one undo step. Any other key commits the pending `@` as literal
//! text first (IME-style: the pending char lands, then the key), Esc
//! disarms into the normal Esc path. While wrapping, typing edits
//! the header, Enter/Tab finish after the closing `@@`, Esc restores
//! the original selection text in one undo step, and navigation,
//! focus moves, preview, or the mouse abandon the gesture (the text
//! stays). Alt+M wraps at once, no gesture; with no selection it is
//! a silent no-op, like `@@` is plain text then.
//!
//! Offsets are never stored across edits: finish and restore
//! re-derive the marker by re-parsing (anchored on the wrap start),
//! so header typing cannot desync them. A parse miss clears the
//! state and falls through to normal handling.

use std::ops::Range;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use edtui::{
    actions::{DeleteSelection, InsertChar, SwitchMode},
    EditorMode,
};

use super::super::offset_to_index2;
use crate::app::writer::WriterFocus;
use crate::app::AppState;
use crate::writer::markers::parse_markers;

/// Hint while armed: the selection is intact, `@` is pending.
pub(super) const WRAP_HINT: &str = "type @ again to wrap";

/// Placeholder the wrap selects for the user to type over.
const PLACEHOLDER: &str = "verb prompt";

/// §5.1 gesture state. Cloned per key (small); the buffer is the
/// truth, this only steers keys.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WrapState {
    /// First `@` swallowed over `range`; the hint shows.
    Armed { range: Range<usize> },
    /// Wrapped at `start`; `original` is the pre-wrap selection for
    /// Esc restore.
    Wrapping {
        start: usize,
        original: (Range<usize>, String),
    },
}

impl AppState {
    /// §5.1 key steering for the editor feed. Returns true when the
    /// key is consumed (armed, wrapped, finished, restored).
    /// Anything unconsumed flows into the normal feed path below.
    /// `pub(crate)`: the feed path owns gesture dispatch.
    pub(crate) fn writer_wrap_key(&mut self, id: crate::session::SessionId, key: KeyEvent) -> bool {
        match self.writers.get(&id).and_then(|s| s.wrap.clone()) {
            None => self.wrap_arm(id, key),
            Some(WrapState::Armed { range }) => self.wrap_armed_key(id, key, range),
            Some(WrapState::Wrapping { start, original }) => {
                self.wrap_wrapping_key(id, key, start, original)
            }
        }
    }

    /// Abandon the gesture, keeping the text. Only an armed hint
    /// owns the banner; anything else on it (watch notices) stays.
    /// `pub(crate)`: mouse, focus, and preview moves share it.
    pub(crate) fn abandon_wrap(&mut self, id: crate::session::SessionId) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        let was_armed = matches!(session.wrap, Some(WrapState::Armed { .. }));
        session.wrap = None;
        if was_armed {
            session.banner = None;
        }
        self.dirty = true;
    }

    /// Wrap the live editor selection at once (Alt+M and the second
    /// `@`). False when there is nothing to wrap.
    /// `pub(crate)`: the TUI input layer calls this directly.
    pub(crate) fn writer_wrap_selection(&mut self, id: crate::session::SessionId) -> bool {
        if self.writer_process_locked(id) {
            self.writer_process_deny(id);
            return false;
        }
        let Some(session) = self.writers.get(&id) else {
            return false;
        };
        if session.focus != WriterFocus::Editor
            || session.editor.is_none()
            || session.doc.is_none()
        {
            return false;
        }
        let range = match session.selection.clone() {
            Some(range) if range.end > range.start => range,
            _ => return false,
        };
        let buffer = session
            .editor
            .as_ref()
            .expect("checked above")
            .lines
            .to_string();
        let text: String = buffer
            .chars()
            .skip(range.start)
            .take(range.end.saturating_sub(range.start))
            .collect();
        // Edge whitespace stays outside the markers (`beta ` wraps
        // as `@@…@@beta@@ `), so the closer never glues to the next
        // word. An all-whitespace selection wraps whole (degenerate).
        let leading: String = text.chars().take_while(|c| c.is_whitespace()).collect();
        let trailing: String = text
            .chars()
            .rev()
            .take_while(|c| c.is_whitespace())
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        let core = text
            .strip_prefix(leading.as_str())
            .unwrap_or(text.as_str())
            .strip_suffix(trailing.as_str())
            .unwrap_or(text.as_str());
        let (lead, core, trail) = if core.is_empty() {
            (String::new(), text.as_str(), String::new())
        } else {
            (leading, core, trailing)
        };
        let wrapped = format!("{lead}@@{PLACEHOLDER}@@{core}@@{trail}");
        let marker_start = range.start + lead.chars().count();
        let Some(session) = self.writers.get_mut(&id) else {
            return false;
        };
        let Some(editor) = session.editor.as_mut() else {
            return false;
        };
        // One undo step: DeleteSelection captures, raw inserts never
        // do (the accept path works the same way). EdTUI's clipboard
        // is saved and restored around the delete.
        let saved = session.clip.0.borrow().clone();
        editor.cursor = offset_to_index2(&buffer, range.start);
        editor.execute(SwitchMode(EditorMode::Visual));
        if let Some(sel) = editor.selection.as_mut() {
            sel.start = offset_to_index2(&buffer, range.start);
            sel.end = offset_to_index2(&buffer, range.end.saturating_sub(1).max(range.start));
        }
        editor.execute(DeleteSelection);
        for c in wrapped.chars() {
            editor.execute(InsertChar(c));
        }
        editor.mode = EditorMode::Insert;
        *session.clip.0.borrow_mut() = saved;
        // The placeholder stays selected for type-over; the cursor
        // sits past it, CUA-style. DeleteSelection cleared the
        // selection object, so re-enter Visual first (the accept
        // path does the same).
        let after = editor.lines.to_string();
        editor.cursor = offset_to_index2(&after, marker_start + 2 + PLACEHOLDER.len());
        editor.execute(SwitchMode(EditorMode::Visual));
        if let Some(sel) = editor.selection.as_mut() {
            sel.start = offset_to_index2(&after, marker_start + 2);
            sel.end = offset_to_index2(&after, marker_start + 2 + PLACEHOLDER.len() - 1);
        }
        editor.mode = EditorMode::Insert;
        session.type_group = None;
        session.sel_anchor = None;
        session.nav_goal = None;
        session.wrap = Some(WrapState::Wrapping {
            start: marker_start,
            original: (range, text),
        });
        session.banner = None;
        self.writer_sync_editor(id);
        self.dirty = true;
        true
    }

    /// First `@`: arm over a live selection, swallow the char.
    fn wrap_arm(&mut self, id: crate::session::SessionId, key: KeyEvent) -> bool {
        if key.code != KeyCode::Char('@')
            || !(key.modifiers == KeyModifiers::NONE || key.modifiers == KeyModifiers::SHIFT)
        {
            return false;
        }
        let Some(session) = self.writers.get(&id) else {
            return false;
        };
        if session.focus != WriterFocus::Editor
            || session.editor.is_none()
            || session.doc.is_none()
        {
            return false;
        }
        let range = match session.selection.clone() {
            Some(range) if range.end > range.start => range,
            _ => return false,
        };
        let Some(session) = self.writers.get_mut(&id) else {
            return false;
        };
        session.wrap = Some(WrapState::Armed { range });
        session.banner = Some(WRAP_HINT.to_string());
        self.dirty = true;
        true
    }

    /// Armed keys: Esc disarms into the normal path, the second `@`
    /// wraps, anything else commits the pending `@` as text first.
    fn wrap_armed_key(
        &mut self,
        id: crate::session::SessionId,
        key: KeyEvent,
        range: Range<usize>,
    ) -> bool {
        if key.code == KeyCode::Esc && key.modifiers == KeyModifiers::empty() {
            self.abandon_wrap(id);
            return false;
        }
        if key.code == KeyCode::Char('@')
            && (key.modifiers == KeyModifiers::NONE || key.modifiers == KeyModifiers::SHIFT)
            && self.writers.get(&id).and_then(|s| s.selection.clone()) == Some(range)
        {
            return self.writer_wrap_selection(id);
        }
        // Commit the pending `@` over the live selection, then let
        // the key process normally (type-over, move, newline, …).
        let Some(session) = self.writers.get_mut(&id) else {
            return false;
        };
        let Some(editor) = session.editor.as_mut() else {
            return false;
        };
        if editor.selection.is_some() {
            editor.execute(DeleteSelection);
            editor.mode = EditorMode::Insert;
        }
        self.abandon_wrap(id);
        self.writer_type_char(id, '@', None, true);
        false
    }

    /// Wrapping keys: Enter/Tab finish, Esc restores, text editing
    /// continues the header, undo/redo/copy/paste/save pass through,
    /// and everything else abandons the gesture into normal handling.
    fn wrap_wrapping_key(
        &mut self,
        id: crate::session::SessionId,
        key: KeyEvent,
        start: usize,
        original: (Range<usize>, String),
    ) -> bool {
        if key.code == KeyCode::Esc && key.modifiers == KeyModifiers::empty() {
            return self.wrap_restore(id, start, original);
        }
        if (key.code == KeyCode::Enter && key.modifiers == KeyModifiers::NONE)
            || key.code == KeyCode::Tab
        {
            return self.wrap_finish(id, start);
        }
        if edits_text(&key) {
            return false;
        }
        self.abandon_wrap(id);
        false
    }

    /// Finish: the cursor lands past the closing `@@`. The marker is
    /// re-derived by parsing (never from stored offsets); a parse
    /// miss clears the state and falls through to normal handling.
    fn wrap_finish(&mut self, id: crate::session::SessionId, start: usize) -> bool {
        let Some(session) = self.writers.get(&id) else {
            return false;
        };
        let Some(editor) = session.editor.as_ref() else {
            return false;
        };
        let buffer = editor.lines.to_string();
        let end = parse_markers(&buffer)
            .markers
            .iter()
            .find(|m| m.whole.start == start)
            .map(|m| m.whole.end);
        let Some(end) = end else {
            self.abandon_wrap(id);
            return false;
        };
        let Some(session) = self.writers.get_mut(&id) else {
            return false;
        };
        let Some(editor) = session.editor.as_mut() else {
            return false;
        };
        editor.cursor = offset_to_index2(&buffer, end);
        editor.selection = None;
        editor.mode = EditorMode::Insert;
        session.wrap = None;
        session.sel_anchor = None;
        session.nav_goal = None;
        self.writer_sync_editor(id);
        self.dirty = true;
        true
    }

    /// Restore: the wrap span goes back to the original selection
    /// text in one undo step (DeleteSelection captures, raw inserts
    /// do not). The cursor lands where retyping would leave it.
    fn wrap_restore(
        &mut self,
        id: crate::session::SessionId,
        start: usize,
        original: (Range<usize>, String),
    ) -> bool {
        let Some(session) = self.writers.get(&id) else {
            return false;
        };
        let Some(editor) = session.editor.as_ref() else {
            return false;
        };
        let buffer = editor.lines.to_string();
        let span = parse_markers(&buffer)
            .markers
            .iter()
            .find(|m| m.whole.start == start)
            .map(|m| m.whole.clone());
        let Some(span) = span else {
            self.abandon_wrap(id);
            return false;
        };
        let Some(session) = self.writers.get_mut(&id) else {
            return false;
        };
        let Some(editor) = session.editor.as_mut() else {
            return false;
        };
        let saved = session.clip.0.borrow().clone();
        editor.cursor = offset_to_index2(&buffer, span.start);
        editor.execute(SwitchMode(EditorMode::Visual));
        if let Some(sel) = editor.selection.as_mut() {
            sel.start = offset_to_index2(&buffer, span.start);
            sel.end = offset_to_index2(&buffer, span.end.saturating_sub(1).max(span.start));
        }
        editor.execute(DeleteSelection);
        for c in original.1.chars() {
            editor.execute(InsertChar(c));
        }
        editor.mode = EditorMode::Insert;
        *session.clip.0.borrow_mut() = saved;
        let after = editor.lines.to_string();
        editor.cursor =
            offset_to_index2(&after, span.start + original.1.chars().count());
        editor.selection = None;
        session.type_group = None;
        session.sel_anchor = None;
        session.nav_goal = None;
        session.wrap = None;
        session.banner = None;
        self.writer_sync_editor(id);
        self.dirty = true;
        true
    }
}

/// Text-shaping keys that continue a wrapping header (or pass
/// through undo/redo/copy/paste/save): typing, backspace, delete,
/// undo, redo, copy, paste, save. Everything else navigates or
/// reframes, which ends the gesture.
fn edits_text(key: &KeyEvent) -> bool {
    use KeyCode as C;
    use KeyModifiers as M;
    match (key.code, key.modifiers) {
        (C::Char(_), M::NONE) | (C::Char(_), M::SHIFT) => true,
        (C::Backspace, M::NONE) | (C::Delete, M::NONE) => true,
        (C::Char('z'), M::CONTROL) | (C::Char('y'), M::CONTROL) => true,
        (C::Char('c'), M::CONTROL) | (C::Char('v'), M::CONTROL) => true,
        (C::Char('s'), M::CONTROL) => true,
        _ => false,
    }
}
