//! Writer clipboard: CUA copy/cut/paste on the Forge-owned clip
//! plus OSC 52 announcements for the outer terminal.
//!
//! Copy keeps the selection (EdTUI's `CopySelection` clears it, so
//! the adapter copies by hand). Cut is one `DeleteSelection`: a
//! single undo step that also lands in the EdTUI clipboard, which
//! is the same [`SharedClipboard`](super::SharedClipboard), so
//! nothing is lost. Paste replaces the selection in one step via
//! `PasteOverSelection`, or inserts at the cursor via `Paste`.
//! Without a selection copy and cut do nothing; an empty clipboard
//! makes paste a no-op. Every clip change queues one bounded OSC 52
//! sequence, flushed after the next painted frame.

use edtui::actions::cpaste::PasteOverSelection;
use edtui::actions::{DeleteSelection, Paste};
use edtui::EditorMode;

use super::{editor_cursor_offset, editor_selection_to_range, offset_to_index2};
use crate::app::AppState;

/// Queued OSC 52 announcements; human-paced ops drain every frame,
/// so the bound only bites when frames stop flowing.
pub const MAX_OSC52: usize = 8;

/// OSC 52 clipboard announcement (`c` selection) for `text`.
/// Shares the standard base64 in [`crate::visual`].
#[must_use]
pub fn osc52_sequence(text: &str) -> String {
    format!("\x1b]52;c;{}\x07", crate::visual::base64_encode(text.as_bytes()))
}

impl AppState {
    /// Queue one OSC 52 announcement, evicting the oldest past the
    /// cap. Drains in [`take_osc52`](AppState::take_osc52).
    pub fn queue_osc52(&mut self, text: &str) {
        if self.osc52.len() >= MAX_OSC52 {
            self.osc52.remove(0);
        }
        self.osc52.push(osc52_sequence(text));
    }

    /// Drain queued sequences for post-frame emission.
    pub fn take_osc52(&mut self) -> Vec<String> {
        std::mem::take(&mut self.osc52)
    }

    /// Copy the selection to the Forge clip and announce it. No
    /// selection: nothing happens. The selection survives.
    /// `pub(crate)`: Ctrl+C arrives through the TUI input layer.
    pub(crate) fn writer_clip_copy(&mut self, id: crate::session::SessionId) {
        let text = match self.writers.get(&id) {
            Some(session) => match session.editor.as_ref() {
                Some(editor) => match editor_selection_to_range(editor) {
                    Some(range) => {
                        let buffer = editor.lines.to_string();
                        buffer
                            .chars()
                            .skip(range.start)
                            .take(range.end - range.start)
                            .collect()
                    }
                    None => return,
                },
                None => return,
            },
            None => return,
        };
        let text: String = text;
        if let Some(session) = self.writers.get_mut(&id) {
            use edtui::clipboard::ClipboardTrait;
            session.clip.set_text(text.clone());
        }
        self.queue_osc52(&text);
        self.dirty = true;
    }

    /// Cut the selection: copy, announce, and delete in one undo
    /// step. No selection: nothing happens.
    /// `pub(crate)`: Ctrl+X arrives through the TUI input layer.
    pub(crate) fn writer_clip_cut(&mut self, id: crate::session::SessionId) {
        if self.writer_process_locked(id) {
            self.writer_process_deny(id);
            return;
        }
        let has_selection = self.writers.get(&id).is_some_and(|session| {
            session.editor.as_ref().is_some_and(|editor| {
                editor_selection_to_range(editor).is_some()
            })
        });
        if !has_selection {
            return;
        }
        self.writer_clip_copy(id);
        if let Some(session) = self.writers.get_mut(&id) {
            if let Some(editor) = session.editor.as_mut() {
                editor.execute(DeleteSelection);
                editor.mode = EditorMode::Insert;
            }
            session.sel_anchor = None;
            session.nav_goal = None;
        }
        self.writer_sync_editor(id);
        self.dirty = true;
    }

    /// Paste the Forge clip: over the selection in one undo step,
    /// else at the cursor. Empty clip: nothing happens.
    /// `pub(crate)`: Ctrl+V arrives through the TUI input layer.
    pub(crate) fn writer_clip_paste(&mut self, id: crate::session::SessionId) {
        if self.writer_process_locked(id) {
            self.writer_process_deny(id);
            return;
        }
        let clip: String = match self.writers.get(&id) {
            Some(session) => session.clip.0.borrow().clone(),
            None => return,
        };
        if clip.is_empty() {
            return;
        }
        if let Some(session) = self.writers.get_mut(&id) {
            if let Some(editor) = session.editor.as_mut() {
                if editor.selection.is_some() {
                    editor.execute(PasteOverSelection);
                } else {
                    editor.execute(Paste);
                }
                editor.mode = EditorMode::Insert;
            }
            session.sel_anchor = None;
            session.nav_goal = None;
        }
        self.writer_sync_editor(id);
        self.dirty = true;
    }
}

impl AppState {
    /// Select the word under the cursor (double-click): a maximal
    /// run of word chars. On a separator or past the end, the
    /// selection collapses to nothing.
    /// `pub(crate)`: the mouse layer calls this directly.
    pub(crate) fn writer_select_word(&mut self, id: crate::session::SessionId) {
        let span = match self.writers.get(&id) {
            Some(session) => match session.editor.as_ref() {
                Some(editor) => {
                    let chars: Vec<char> = editor.lines.to_string().chars().collect();
                    let at = editor_cursor_offset(editor);
                    if at >= chars.len() || !super::nav::is_word_char(chars[at]) {
                        None
                    } else {
                        let mut start = at;
                        while start > 0 && super::nav::is_word_char(chars[start - 1]) {
                            start -= 1;
                        }
                        let mut end = at;
                        while end < chars.len() && super::nav::is_word_char(chars[end]) {
                            end += 1;
                        }
                        Some((start, end))
                    }
                }
                None => return,
            },
            None => return,
        };
        if let Some(session) = self.writers.get_mut(&id) {
            if let Some(editor) = session.editor.as_mut() {
                match span {
                    Some((start, end)) => {
                        let buffer = editor.lines.to_string();
                        editor.cursor = offset_to_index2(&buffer, end - 1);
                        editor.execute(edtui::actions::SwitchMode(edtui::EditorMode::Visual));
                        if let Some(sel) = editor.selection.as_mut() {
                            sel.start = offset_to_index2(&buffer, start);
                            sel.end = offset_to_index2(&buffer, end - 1);
                        }
                    }
                    None => {
                        editor.selection = None;
                    }
                }
            }
            session.sel_anchor = None;
            session.nav_goal = None;
        }
        self.writer_sync_editor(id);
        self.dirty = true;
    }

    /// Select the cursor's line without its newline (triple-click).
    /// On an empty line, the selection collapses to nothing.
    /// `pub(crate)`: the mouse layer calls this directly.
    pub(crate) fn writer_select_line(&mut self, id: crate::session::SessionId) {
        let span = match self.writers.get(&id) {
            Some(session) => match session.editor.as_ref() {
                Some(editor) => {
                    let buffer = editor.lines.to_string();
                    let row = editor.cursor.row;
                    let mut start = 0;
                    for line in buffer.split('\n').take(row) {
                        start += line.chars().count() + 1;
                    }
                    let len = buffer
                        .split('\n')
                        .nth(row)
                        .map(|line| line.chars().count())
                        .unwrap_or(0);
                    (len > 0).then_some((start, start + len))
                }
                None => return,
            },
            None => return,
        };
        if let Some(session) = self.writers.get_mut(&id) {
            if let Some(editor) = session.editor.as_mut() {
                match span {
                    Some((start, end)) => {
                        let buffer = editor.lines.to_string();
                        editor.cursor = offset_to_index2(&buffer, end - 1);
                        editor.execute(edtui::actions::SwitchMode(edtui::EditorMode::Visual));
                        if let Some(sel) = editor.selection.as_mut() {
                            sel.start = offset_to_index2(&buffer, start);
                            sel.end = offset_to_index2(&buffer, end - 1);
                        }
                    }
                    None => {
                        editor.selection = None;
                    }
                }
            }
            session.sel_anchor = None;
            session.nav_goal = None;
        }
        self.writer_sync_editor(id);
        self.dirty = true;
    }

    /// Scroll the editor viewport by `dy` screen rows, clamped to
    /// the document. Drag autoscroll calls this, then feeds the
    /// edge-clamped drag so the selection grows.
    /// `pub(crate)`: the mouse layer calls this directly.
    pub(crate) fn writer_scroll_editor(&mut self, id: crate::session::SessionId, dy: isize) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        let Some(editor) = session.editor.as_mut() else {
            return;
        };
        let (x, y) = editor.viewport_offset();
        let max = editor.lines.len().saturating_sub(1);
        let next = y.saturating_add_signed(dy).min(max);
        editor.set_viewport_offset(x, next);
        self.dirty = true;
    }
}

#[cfg(test)]
mod tests {
    use super::osc52_sequence;

    #[test]
    fn osc52_wraps_base64_with_the_terminator() {
        assert_eq!(osc52_sequence("bbb"), "\x1b]52;c;YmJi\x07");
        assert_eq!(osc52_sequence(""), "\x1b]52;c;\x07");
    }
}
