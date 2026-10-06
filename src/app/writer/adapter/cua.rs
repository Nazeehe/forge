//! Adapter CUA layer: the standard keymap's text ops plus the custom
//! EdTUI event register that retires the emacs map.
//!
//! The register binds ONLY what CUA text entry needs — Enter,
//! Backspace, Delete, Ctrl+Z / Ctrl+Y / Ctrl+Shift+Z — in Insert and
//! Visual alike. Every other CUA binding is adapter-owned on char
//! offsets (moves in `nav`, selection/indent/word kills here), and
//! unbound keys (find until E7, all retired emacs chords) fall
//! through to nothing: no binding reaches the agent pane. Clipboard
//! (E5) is adapter-owned in `clipboard`, beside the selection ops.
//!
//! Undo granularity: each inserted space captures its own undo step
//! until E6 coalesces typing into word/pause groups. Deletions go
//! through one `DeleteSelection` and undo in one step.

use std::collections::HashMap;

use edtui::actions::{
    Action, DeleteChar, DeleteCharForward, DeleteSelection, LineBreak, Redo, SwitchMode, Undo,
};
use edtui::events::{KeyEventHandler, KeyEventRegister, KeyInput};
use edtui::{EditorEventHandler, EditorMode};

use super::nav::{word_left, word_right};
use super::{index2_to_offset, offset_to_index2};
use crate::app::AppState;

/// Two spaces per indent level: the EditorConfig-ish default the wrap
/// math already assumes for tabs.
pub(super) const INDENT_WIDTH: usize = 2;

/// The CUA event register, shared by key and mouse feeds (mouse
/// handling ignores the key register). Replaces
/// `EditorEventHandler::emacs_mode` everywhere.
pub(super) fn cua_handler() -> EditorEventHandler {
    use crossterm::event::{KeyCode as CTKey, KeyModifiers as CTMod};
    let mut register: HashMap<KeyEventRegister, Action> = HashMap::new();
    let mut bind = |input: KeyInput, action: Action| {
        register.insert(KeyEventRegister::i(vec![input]), action.clone());
        register.insert(KeyEventRegister::v(vec![input]), action);
    };
    bind(KeyInput::new(CTKey::Enter), LineBreak(1).into());
    bind(KeyInput::new(CTKey::Backspace), DeleteChar(1).into());
    bind(KeyInput::new(CTKey::Delete), DeleteCharForward(1).into());
    bind(KeyInput::ctrl('z'), Undo.into());
    bind(KeyInput::ctrl('y'), Redo.into());
    bind(
        KeyInput::with_modifiers('Z', CTMod::CONTROL | CTMod::SHIFT),
        Redo.into(),
    );
    EditorEventHandler::new(KeyEventHandler::new(register, true))
}

impl AppState {
    /// Select the whole document (empty docs select nothing): the
    /// selection feeds Rephrase and the replace-on-type path.
    /// `pub(crate)`: Ctrl+A arrives through the TUI input layer.
    pub(crate) fn writer_select_all(&mut self, id: crate::session::SessionId) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        let Some(editor) = session.editor.as_mut() else {
            return;
        };
        let buffer = editor.lines.to_string();
        let total = buffer.chars().count();
        if total == 0 {
            editor.selection = None;
            return;
        }
        editor.cursor = offset_to_index2(&buffer, total);
        editor.execute(SwitchMode(EditorMode::Visual));
        if let Some(sel) = editor.selection.as_mut() {
            sel.start = offset_to_index2(&buffer, 0);
            sel.end = offset_to_index2(&buffer, total - 1);
        }
        session.sel_anchor = None;
        session.nav_goal = None;
        self.writer_sync_editor(id);
        self.dirty = true;
    }

    /// CUA word kill: with a live selection the selection goes (like
    /// Backspace/Delete); otherwise the CUA word before (`backward`)
    /// or after (`forward`) the cursor, using the same word starts as
    /// the word jumps so kills eat what jumps cross. One
    /// `DeleteSelection`: one undo step.
    /// `pub(crate)`: Ctrl+Backspace/Delete arrive through the TUI layer.
    pub(crate) fn writer_delete_word(&mut self, id: crate::session::SessionId, backward: bool) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        let Some(editor) = session.editor.as_mut() else {
            return;
        };
        let buffer = editor.lines.to_string();
        let total = buffer.chars().count();
        if total == 0 {
            return;
        }
        let cursor = index2_to_offset(&editor.lines, editor.cursor).min(total);
        let chars: Vec<char> = buffer.chars().collect();
        let (start, end) = match editor_selection_range(editor, &buffer) {
            Some(range) => (range.start, range.end),
            None => {
                if backward {
                    if cursor == 0 {
                        return;
                    }
                    (word_left(&chars, cursor), cursor)
                } else {
                    if cursor >= total {
                        return;
                    }
                    (cursor, word_right(&chars, cursor))
                }
            }
        };
        if start >= end {
            return;
        }
        editor.cursor = offset_to_index2(&buffer, start);
        editor.execute(SwitchMode(EditorMode::Visual));
        if let Some(sel) = editor.selection.as_mut() {
            sel.start = offset_to_index2(&buffer, start);
            sel.end = offset_to_index2(&buffer, end.saturating_sub(1).max(start));
        }
        editor.execute(DeleteSelection);
        editor.mode = EditorMode::Insert;
        session.sel_anchor = None;
        session.nav_goal = None;
        self.writer_sync_editor(id);
        self.dirty = true;
    }

    /// Tab: indent the selection's touched lines, or the current line
    /// (list markers survive: spaces go before them), or — with no
    /// selection on a line this leaves alone — nothing extra beyond
    /// the spaces. Plain indent inserts [`INDENT_WIDTH`] spaces at the
    /// cursor. Each space is its own undo step until E6 groups them.
    /// `pub(crate)`: Tab arrives through the TUI input layer.
    pub(crate) fn writer_indent(&mut self, id: crate::session::SessionId) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        let Some(editor) = session.editor.as_mut() else {
            return;
        };
        let buffer = editor.lines.to_string();
        let range = editor_selection_range(editor, &buffer);
        let rows = touched_rows(&buffer, range.clone(), index2_to_offset(&editor.lines, editor.cursor));
        if rows.is_empty() {
            return;
        }
        // Indent bottom-up so earlier inserts never shift later rows.
        // The cursor tracks its own row's insert. Spaces go through
        // the handler (not raw executes) so each captures its undo
        // step: raw `InsertChar` never captures.
        let cursor_row = row_of_offset(&buffer, index2_to_offset(&editor.lines, editor.cursor));
        let mut cursor_off = index2_to_offset(&editor.lines, editor.cursor);
        let mut handler = cua_handler();
        for row in rows.iter().rev() {
            // Rows only ever gain a prefix, so row indices stay put
            // while offsets move: re-derive the live buffer per row.
            let live = editor.lines.to_string();
            let line_start = line_start_offset(&live, *row);
            editor.cursor = offset_to_index2(&live, line_start);
            editor.mode = EditorMode::Insert;
            editor.selection = None;
            for _ in 0..INDENT_WIDTH {
                handler.on_event(
                    crossterm::event::Event::Key(crossterm::event::KeyEvent::new(
                        crossterm::event::KeyCode::Char(' '),
                        crossterm::event::KeyModifiers::NONE,
                    )),
                    editor,
                );
            }
            if *row == cursor_row {
                cursor_off += INDENT_WIDTH;
            }
        }
        let fresh = editor.lines.to_string();
        editor.cursor = offset_to_index2(&fresh, cursor_off.min(fresh.chars().count()));
        editor.selection = None;
        editor.mode = EditorMode::Insert;
        session.sel_anchor = None;
        session.nav_goal = None;
        self.writer_sync_editor(id);
        self.dirty = true;
    }

    /// Shift+Tab: strip up to [`INDENT_WIDTH`] leading spaces per
    /// touched line (selection or current line). Lines without indent
    /// are untouched; a fully plain target edits nothing and bumps no
    /// revision. One `DeleteSelection` per stripped line.
    /// `pub(crate)`: Shift+Tab arrives through the TUI input layer.
    pub(crate) fn writer_outdent(&mut self, id: crate::session::SessionId) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        let Some(editor) = session.editor.as_mut() else {
            return;
        };
        let buffer = editor.lines.to_string();
        let range = editor_selection_range(editor, &buffer);
        let rows = touched_rows(&buffer, range, index2_to_offset(&editor.lines, editor.cursor));
        // Strip bottom-up; at most one capture per line.
        let mut stripped_any = false;
        for row in rows.iter().rev() {
            let text = editor.lines.to_string();
            let line_start = line_start_offset(&text, *row);
            let line = line_text(&text, *row);
            let strip = line.chars().take_while(|c| *c == ' ').take(INDENT_WIDTH).count();
            if strip == 0 {
                continue;
            }
            editor.cursor = offset_to_index2(&text, line_start);
            editor.execute(SwitchMode(EditorMode::Visual));
            if let Some(sel) = editor.selection.as_mut() {
                sel.start = offset_to_index2(&text, line_start);
                sel.end = offset_to_index2(&text, line_start + strip - 1);
            }
            editor.execute(DeleteSelection);
            stripped_any = true;
        }
        if !stripped_any {
            return;
        }
        editor.selection = None;
        editor.mode = EditorMode::Insert;
        session.sel_anchor = None;
        session.nav_goal = None;
        self.writer_sync_editor(id);
        self.dirty = true;
    }
}

/// Exclusive selection range over the live buffer, if any.
fn editor_selection_range(
    editor: &edtui::EditorState,
    buffer: &str,
) -> Option<std::ops::Range<usize>> {
    let sel = editor.selection.as_ref()?;
    let total = buffer.chars().count();
    let start = index2_to_offset(&editor.lines, sel.start);
    let end = (index2_to_offset(&editor.lines, sel.end) + 1).min(total);
    if start >= end || start >= total {
        return None;
    }
    Some(start..end)
}

/// Doc rows a Tab/Shift+Tab touches: every row the selection spans,
/// or the cursor's row when nothing is selected.
fn touched_rows(
    buffer: &str,
    range: Option<std::ops::Range<usize>>,
    cursor: usize,
) -> Vec<usize> {
    match range {
        Some(range) => {
            let first = row_of_offset(buffer, range.start);
            let last = row_of_offset(buffer, range.end.saturating_sub(1).max(range.start));
            (first..=last).collect()
        }
        None => vec![row_of_offset(buffer, cursor)],
    }
}

/// Doc row holding `offset`.
fn row_of_offset(text: &str, offset: usize) -> usize {
    text.chars()
        .take(offset.min(text.chars().count()))
        .filter(|&c| c == '\n')
        .count()
}

/// Char offset where doc `row` starts.
fn line_start_offset(text: &str, row: usize) -> usize {
    let mut current = 0usize;
    let mut off = 0usize;
    for (i, c) in text.chars().enumerate() {
        if current >= row {
            off = i;
            break;
        }
        if c == '\n' {
            current += 1;
            off = i + 1;
        }
    }
    if current < row {
        off = text.chars().count();
    }
    off
}

/// Text of doc `row` without its newline.
fn line_text(text: &str, row: usize) -> String {
    text.split('\n').nth(row).unwrap_or("").to_string()
}
