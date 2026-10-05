//! Adapter apply layer: Accept/Reject through exactly one method
//! each (doc + proposals + editor sync, so no caller can skip a
//! step), plus Save through S2 with conflicts in the error slot.

use super::{editor_selection_to_range, offset_to_index2, SharedClipboard};
use crate::app::AppState;
use edtui::{
    actions::{DeleteSelection, InsertChar, SwitchMode},
    EditorMode, EditorState, Lines,
};

impl AppState {
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
