//! Adapter apply layer: Accept/Reject through exactly one method
//! each (doc + proposals + editor sync, so no caller can skip a
//! step), plus Save through S2 with conflicts in the error slot.

use super::{editor_selection_to_range, offset_to_index2, SharedClipboard};
use crate::writer::{document::Document, proposal::Proposals};
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
        session.nav_goal = None;
        session.selected_proposal = None;
        // The accept rewrote `range`: fence parity above its first
        // line survives, the rest re-extends on the next paint.
        let accept_line = crate::app::writer::WriterSession::line_of_offset(
            &session.doc.as_ref().expect("checked above").text,
            range.start,
        );
        session.note_fence_edit(accept_line);
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
                session.banner = None;
                session.save_note = Some("saved".to_string());
            }
            Err(crate::writer::WriterError::ConflictOnSave) => {
                // The same banner the watch raises for a dirty buffer
                // whose disk moved: Reload drops the buffer, Keep
                // mine adopts the baseline so the next save wins. A
                // crowded slot keeps the bare error instead.
                let rel = doc.path_rel.clone();
                session.save_note = Some("not saved".to_string());
                if session.pending_confirm.is_none() {
                    session.pending_confirm = Some(crate::app::writer::PendingConfirm {
                        message: format!("Changed on disk: {rel}"),
                        actions: vec![
                            crate::app::writer::ConfirmAction::ReloadFromDisk,
                            crate::app::writer::ConfirmAction::KeepMine,
                        ],
                    });
                    session.error = None;
                } else {
                    session.error =
                        Some("save conflict: file changed on disk".to_string());
                }
            }
            Err(other) => {
                session.error = Some(format!("save failed: {other}"));
                session.save_note = Some("not saved".to_string());
            }
        }
        self.dirty = true;
    }

    /// Save-as: write the live text to `rel` (confined, Markdown-only
    /// by the caller) and switch the doc onto the new file. The text
    /// is identical, so proposals, selection and the editor buffer all
    /// stay valid; Recent updates through the opened list.
    /// `pub(crate)`: the prompt submit and the Overwrite confirm share it.
    pub(crate) fn writer_save_as_to(
        &mut self,
        id: crate::session::SessionId,
        rel: &str,
        abs: std::path::PathBuf,
    ) {
        let text = match self.writers.get(&id).and_then(|s| s.doc.as_ref()) {
            Some(doc) => doc.text.clone(),
            None => {
                self.writer_fail(id, "no document open");
                return;
            }
        };
        let cwd = super::requests::writer_cwd(self, id);
        let mut target = match Document::open(&cwd, rel) {
            Ok(doc) => doc,
            Err(e) => {
                self.writer_fail(id, &e.to_string());
                return;
            }
        };
        target.text = text;
        target.dirty = true;
        match target.save() {
            Ok(()) => {
                // Same text under a new revision: the cached parity
                // survives, re-keyed to the target revision.
                let rev = target.revision;
                if let Some(session) = self.writers.get_mut(&id) {
                    session.doc = Some(target);
                    session.fence_cache.rev = rev;
                    session.fence_dirty_from = None;
                    session.open_prompt = None;
                    session.pending_confirm = None;
                    session.error = None;
                }
                self.writer_note_opened(id, abs);
                self.dirty = true;
            }
            Err(crate::writer::WriterError::ConflictOnSave) => {
                self.writer_fail(id, "save conflict: file changed on disk");
            }
            Err(other) => self.writer_fail(id, &format!("save failed: {other}")),
        }
    }

    /// Close the document: a clean doc closes to the empty state at
    /// once; a dirty doc raises the Save&close / Discard / Cancel
    /// confirm in the fixed slot instead of closing.
    pub fn writer_close_doc(&mut self, id: crate::session::SessionId) {
        let Some(session) = self.writers.get(&id) else {
            return;
        };
        let Some(doc) = session.doc.as_ref() else {
            return;
        };
        if !doc.dirty {
            self.writer_do_close(id);
            return;
        }
        let rel = doc.path_rel.clone();
        if let Some(session) = self.writers.get_mut(&id) {
            session.pending_confirm = Some(crate::app::writer::PendingConfirm {
                message: format!("Unsaved changes in {rel}:"),
                actions: vec![
                    crate::app::writer::ConfirmAction::SaveAndClose,
                    crate::app::writer::ConfirmAction::DiscardClose,
                    crate::app::writer::ConfirmAction::Cancel,
                ],
            });
        }
        self.dirty = true;
    }

    /// Drop the document and its editor, proposals, requests, thread
    /// and draft: the empty state owns the screen again. The panel
    /// toggle, the opened list and the recent cache survive, so the
    /// list still offers what this run opened.
    pub(super) fn writer_do_close(&mut self, id: crate::session::SessionId) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        session.doc = None;
        session.editor = None;
        session.selection = None;
        session.sel_anchor = None;
        session.selected_proposal = None;
        session.reset_fence_cache();
        session.proposals = Proposals::default();
        session.requests.clear();
        session.thread.clear();
        session.chat_input.clear();
        session.nav_goal = None;
        session.open_prompt = None;
        session.pending_confirm = None;
        session.error = None;
        session.find = None;
        session.focus = crate::app::writer::WriterFocus::Editor;
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
