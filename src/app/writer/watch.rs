//! File watch + reload for open Writer documents (E9). The TUI
//! loop settles through [`AppState::writer_poll_files`] every frame;
//! the poll itself runs at most once a second across all sessions:
//! metadata first, and the file is hashed only when its mtime moved.
//! A clean buffer auto-reloads (cursor clamped, proposals staled,
//! info banner); a dirty buffer raises an actionable banner
//! (Reload / Keep mine) in the fixed slot; a deleted file offers
//! (Save to recreate / Close). Save conflicts reuse the same banner
//! instead of the bare error. Poll state never grows: one timestamp
//! for the throttle, nothing per file.

use std::time::{Duration, Instant};

use crate::app::writer::{ConfirmAction, PendingConfirm};
use crate::app::AppState;

/// Minimum gap between two watch passes across all sessions.
const WATCH_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Info banner after a clean auto-reload.
const RELOADED_BANNER: &str = "Reloaded: changed on disk";

impl AppState {
    /// Throttled poll entry for the frame loop.
    pub fn writer_poll_files(&mut self) {
        self.writer_poll_files_now(Instant::now());
    }

    /// Poll once, unless the last poll is under a second old.
    /// `pub(crate)`: tests drive instants without sleeping.
    pub(crate) fn writer_poll_files_now(&mut self, now: Instant) {
        if self
            .last_watch_poll
            .is_some_and(|at| now.duration_since(at) < WATCH_POLL_INTERVAL)
        {
            return;
        }
        self.last_watch_poll = Some(now);
        let ids: Vec<crate::session::SessionId> = self.writers.keys().copied().collect();
        for id in ids {
            self.writer_poll_one(id);
        }
        self.dirty = true;
    }

    /// Reload the open document from disk, keeping the editor
    /// underneath: cursor clamped, selection dropped, pending
    /// proposals staled, fence cache restarted.
    /// `pub(crate)`: watch, Reload pills and conflict paths share it.
    pub(crate) fn writer_reload_from_disk(
        &mut self,
        id: crate::session::SessionId,
    ) -> Result<(), String> {
        let old_cursor = self
            .writers
            .get(&id)
            .and_then(|session| session.editor.as_ref())
            .map(|editor| {
                super::adapter::index2_to_offset(&editor.lines, editor.cursor)
            })
            .unwrap_or(0);
        let old_len = self
            .writers
            .get(&id)
            .and_then(|session| session.doc.as_ref())
            .map(|doc| doc.text.chars().count())
            .unwrap_or(0);
        // Under the process lock the buffer is always clean (edits
        // are denied), so the reload lands — but it must not
        // recreate the editor (that would wipe the undo stack
        // holding the pre-run capture) nor clear the banner (that
        // would drop the Processing notice).
        let locked = self
            .writers
            .get(&id)
            .is_some_and(|s| s.process.is_some());
        {
            let Some(session) = self.writers.get_mut(&id) else {
                return Err("no writer session".to_string());
            };
            let Some(doc) = session.doc.as_mut() else {
                return Err("no document open".to_string());
            };
            doc.reload().map_err(|e| e.to_string())?;
            session.proposals.on_edit(&(0..old_len));
            session.reset_fence_cache();
            session.type_group = None;
            if !locked {
                session.banner = None;
            }
        }
        if locked {
            // In place: lines assign directly, which never captures,
            // so the pre-run boundary survives every agent write.
            if let Some(session) = self.writers.get_mut(&id) {
                if let Some(text) =
                    session.doc.as_ref().map(|doc| doc.text.clone())
                {
                    if let Some(editor) = session.editor.as_mut() {
                        editor.lines = edtui::Lines::from(text.as_str());
                    }
                }
            }
        } else {
            // Rebuild the buffer from the new text, then clamp the cursor:
            // the offsets above belong to the old text.
            self.writer_open_editor(id);
        }
        let total = self
            .writers
            .get(&id)
            .and_then(|session| session.doc.as_ref())
            .map(|doc| doc.text.chars().count())
            .unwrap_or(0);
        if let Some(session) = self.writers.get_mut(&id) {
            if let Some(editor) = session.editor.as_mut() {
                let buffer = editor.lines.to_string();
                editor.cursor =
                    super::adapter::offset_to_index2(&buffer, old_cursor.min(total));
            }
            session.selection = None;
        }
        self.dirty = true;
        Ok(())
    }

    /// Keep the buffer, adopt the on-disk hash/mtime as the new
    /// baseline: the next save overwrites instead of conflicting.
    /// `pub(crate)`: the (Keep mine) pill calls this.
    pub(crate) fn writer_keep_mine(&mut self, id: crate::session::SessionId) -> Result<(), String> {
        let Some(session) = self.writers.get_mut(&id) else {
            return Err("no writer session".to_string());
        };
        let Some(doc) = session.doc.as_mut() else {
            return Err("no document open".to_string());
        };
        let bytes =
            std::fs::read(&doc.abs_path).map_err(|e| format!("file unreadable: {e}"))?;
        doc.disk_hash = crate::writer::hash_bytes(&bytes);
        doc.mtime = std::fs::metadata(&doc.abs_path)
            .ok()
            .and_then(|m| m.modified().ok());
        session.banner = None;
        self.dirty = true;
        Ok(())
    }

    /// Save every dirty Writer doc (all sessions, or one for kill).
    /// Attempts all of them; returns the sessions whose save failed
    /// (their error slots and conflict banners already say why), so
    /// quit/kill can abort instead of losing work.
    /// `pub(crate)`: the quit/kill confirm settlement calls this.
    pub(crate) fn writer_save_all_dirty(
        &mut self,
        only: Option<crate::session::SessionId>,
    ) -> Vec<crate::session::SessionId> {
        let ids: Vec<crate::session::SessionId> = self
            .writers
            .iter()
            .filter(|(id, session)| {
                only.is_none_or(|want| want == **id)
                    && session.doc.as_ref().is_some_and(|doc| doc.dirty)
            })
            .map(|(id, _)| *id)
            .collect();
        let mut failed = Vec::new();
        for id in ids {
            self.writer_save(id);
            if self
                .writers
                .get(&id)
                .and_then(|session| session.doc.as_ref())
                .is_some_and(|doc| doc.dirty)
            {
                failed.push(id);
            }
        }
        failed
    }

    /// Dirty Writer docs as display paths, deduped, across sessions
    /// (`only` scopes to one session for kill). `pub(crate)`: the
    /// quit/kill confirms name them.
    pub(crate) fn writer_dirty_docs(&self, only: Option<crate::session::SessionId>) -> Vec<String> {
        let mut out = Vec::new();
        for (id, session) in self.writers.iter() {
            if only.is_some_and(|want| want != *id) {
                continue;
            }
            if let Some(rel) = session
                .doc
                .as_ref()
                .filter(|doc| doc.dirty)
                .map(|doc| doc.path_rel.clone())
            {
                if !out.contains(&rel) {
                    out.push(rel);
                }
            }
        }
        out.sort();
        out
    }

    /// One session's poll: metadata first, hash only on an mtime move.
    fn writer_poll_one(&mut self, id: crate::session::SessionId) {
        let Some(session) = self.writers.get(&id) else {
            return;
        };
        let Some(doc) = session.doc.as_ref() else {
            return;
        };
        let abs = doc.abs_path.clone();
        let rel = doc.path_rel.clone();
        let known_mtime = doc.mtime;
        let dirty = doc.dirty;
        match std::fs::metadata(&abs) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // Deleted: offer to write the buffer back, or close.
                if self
                    .writers
                    .get(&id)
                    .is_some_and(|session| session.pending_confirm.is_none())
                {
                    if let Some(session) = self.writers.get_mut(&id) {
                        session.pending_confirm = Some(PendingConfirm {
                            message: format!("Deleted on disk: {rel}"),
                            actions: vec![
                                ConfirmAction::SaveToRecreate,
                                ConfirmAction::CloseDoc,
                            ],
                        });
                    }
                }
            }
            Err(_) => {
                // Transient (permissions, races): retry next poll.
            }
            Ok(meta) => {
                let mtime = meta.modified().ok();
                if mtime == known_mtime {
                    return;
                }
                let bytes = match std::fs::read(&abs) {
                    Ok(bytes) => bytes,
                    Err(_) => return,
                };
                let hash = crate::writer::hash_bytes(&bytes);
                let same = self
                    .writers
                    .get(&id)
                    .and_then(|session| session.doc.as_ref())
                    .is_some_and(|doc| doc.disk_hash == hash);
                if same {
                    // Touched, same content: adopt the mtime quietly.
                    if let Some(session) = self.writers.get_mut(&id) {
                        if let Some(doc) = session.doc.as_mut() {
                            doc.mtime = mtime;
                        }
                    }
                    return;
                }
                if !dirty {
                    match self.writer_reload_from_disk(id) {
                        Ok(()) => {
                            if let Some(session) = self.writers.get_mut(&id) {
                                session.banner = Some(RELOADED_BANNER.to_string());
                            }
                        }
                        Err(e) => self.writer_adopt_unreadable(id, &e),
                    }
                } else if self
                    .writers
                    .get(&id)
                    .is_some_and(|session| session.pending_confirm.is_none())
                {
                    // An existing confirm (close, save-as, another
                    // banner) wins; the next poll retries.
                    if let Some(session) = self.writers.get_mut(&id) {
                        // Keep mine first: the default must never throw
                        // away unsaved edits.
                        session.pending_confirm = Some(PendingConfirm {
                            message: format!("Changed on disk: {rel}"),
                            actions: vec![
                                ConfirmAction::KeepMine,
                                ConfirmAction::ReloadFromDisk,
                            ],
                        });
                    }
                }
            }
        }
    }

    /// The disk changed but the new bytes cannot come in (too large,
    /// invalid UTF-8, special file): say so, keep the last good text,
    /// and adopt the baseline as dirty so the next save conflicts
    /// instead of silently overwriting.
    fn writer_adopt_unreadable(&mut self, id: crate::session::SessionId, detail: &str) {
        let Some(session) = self.writers.get_mut(&id) else {
            return;
        };
        let Some(doc) = session.doc.as_mut() else {
            return;
        };
        let rel = doc.path_rel.clone();
        doc.mtime = std::fs::metadata(&doc.abs_path)
            .ok()
            .and_then(|m| m.modified().ok());
        if let Ok(bytes) = std::fs::read(&doc.abs_path) {
            doc.disk_hash = crate::writer::hash_bytes(&bytes);
        }
        doc.dirty = true;
        session.error = Some(format!("changed on disk ({detail}); showing last good text: {rel}"));
        self.dirty = true;
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use crate::app::writer::ConfirmAction;

    fn open_d(
        state: &mut crate::app::AppState,
        id: crate::session::SessionId,
        run: &str,
        dir: &std::path::Path,
        text: &str,
    ) {
        std::fs::write(dir.join("d.md"), text).unwrap();
        open_doc(state, run, "d.md");
        open_editor(state, id);
    }

    #[test]
    fn clean_buffer_auto_reloads_with_banner() {
        let (mut state, id, run, dir) = writer_agent();
        open_d(&mut state, id, &run, &dir, "aaa");
        // External change after load.
        std::fs::write(dir.join("d.md"), "external").unwrap();
        state.writer_poll_files();
        let session = state.writers.get(&id).unwrap();
        let doc = session.doc.as_ref().unwrap();
        assert_eq!(doc.text, "external");
        assert!(!doc.dirty, "reload stays clean");
        assert_eq!(
            session.banner.as_deref(),
            Some("Reloaded: changed on disk")
        );
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn dirty_buffer_raises_reload_or_keep() {
        let (mut state, id, run, dir) = writer_agent();
        open_d(&mut state, id, &run, &dir, "aaa");
        state
            .writers
            .get_mut(&id)
            .unwrap()
            .doc
            .as_mut()
            .unwrap()
            .apply_edit(0..0, "x")
            .unwrap();
        std::fs::write(dir.join("d.md"), "external").unwrap();
        state.writer_poll_files();
        let session = state.writers.get(&id).unwrap();
        assert_eq!(session.doc.as_ref().unwrap().text, "xaaa", "buffer kept");
        let actions = &session.pending_confirm.as_ref().expect("banner").actions;
        assert!(actions.contains(&ConfirmAction::ReloadFromDisk));
        assert!(actions.contains(&ConfirmAction::KeepMine));
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn keep_mine_adopts_the_baseline() {
        let (mut state, id, run, dir) = writer_agent();
        open_d(&mut state, id, &run, &dir, "aaa");
        state
            .writers
            .get_mut(&id)
            .unwrap()
            .doc
            .as_mut()
            .unwrap()
            .apply_edit(0..0, "x")
            .unwrap();
        std::fs::write(dir.join("d.md"), "external").unwrap();
        state.writer_poll_files();
        let at = find_action(&state, id, &ConfirmAction::KeepMine);
        state.writer_fire_confirm(id, at);
        let t0 = std::time::Instant::now();
        // A later poll stays quiet: the baseline moved, not the text.
        state.writer_poll_files_now(t0 + std::time::Duration::from_secs(2));
        let session = state.writers.get(&id).unwrap();
        assert!(session.pending_confirm.is_none(), "no re-banner");
        assert_eq!(session.doc.as_ref().unwrap().text, "xaaa");
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn deleted_file_offers_recreate_or_close() {
        let (mut state, id, run, dir) = writer_agent();
        open_d(&mut state, id, &run, &dir, "aaa");
        std::fs::remove_file(dir.join("d.md")).unwrap();
        state.writer_poll_files();
        {
            let session = state.writers.get(&id).unwrap();
            let actions = &session.pending_confirm.as_ref().expect("banner").actions;
            assert!(actions.contains(&ConfirmAction::SaveToRecreate));
        }
        let at = find_action(&state, id, &ConfirmAction::SaveToRecreate);
        state.writer_fire_confirm(id, at);
        assert_eq!(std::fs::read_to_string(dir.join("d.md")).unwrap(), "aaa");
        assert!(!state.writers.get(&id).unwrap().doc.as_ref().unwrap().dirty);
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn save_conflict_shows_the_banner_not_the_bare_error() {
        let (mut state, id, run, dir) = writer_agent();
        open_d(&mut state, id, &run, &dir, "aaa");
        state
            .writers
            .get_mut(&id)
            .unwrap()
            .doc
            .as_mut()
            .unwrap()
            .apply_edit(0..0, "x")
            .unwrap();
        std::fs::write(dir.join("d.md"), "external").unwrap();
        state.writer_save(id);
        let session = state.writers.get(&id).unwrap();
        assert!(
            session.pending_confirm.as_ref().is_some_and(|c| c.actions.contains(
                &ConfirmAction::ReloadFromDisk
            )),
            "conflict banner, not a bare error"
        );
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn throttle_skips_rapid_polls() {
        let (mut state, id, run, dir) = writer_agent();
        open_d(&mut state, id, &run, &dir, "aaa");
        std::fs::write(dir.join("d.md"), "external").unwrap();
        let t0 = std::time::Instant::now();
        state.writer_poll_files_now(t0);
        assert_eq!(state.writers.get(&id).unwrap().doc.as_ref().unwrap().text, "external");
        std::fs::write(dir.join("d.md"), "external2").unwrap();
        // Same instant: skipped, still the old text.
        state.writer_poll_files_now(t0);
        assert_eq!(state.writers.get(&id).unwrap().doc.as_ref().unwrap().text, "external");
        // Past the interval: runs again.
        state.writer_poll_files_now(t0 + std::time::Duration::from_secs(2));
        assert_eq!(
            state.writers.get(&id).unwrap().doc.as_ref().unwrap().text,
            "external2"
        );
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Index of an action in the session's pending confirm, for
    /// firing through the same path a mouse click takes.
    fn find_action(
        state: &crate::app::AppState,
        id: crate::session::SessionId,
        want: &ConfirmAction,
    ) -> usize {
        state
            .writers
            .get(&id)
            .unwrap()
            .pending_confirm
            .as_ref()
            .unwrap()
            .actions
            .iter()
            .position(|a| a == want)
            .unwrap()
    }
}

#[cfg(test)]
mod default_tests {
    use super::super::test_support::*;

    fn open_d(
        state: &mut crate::app::AppState,
        id: crate::session::SessionId,
        run: &str,
        dir: &std::path::Path,
        text: &str,
    ) {
        std::fs::write(dir.join("d.md"), text).unwrap();
        open_doc(state, run, "d.md");
        open_editor(state, id);
    }

    fn dirty(state: &mut crate::app::AppState, id: crate::session::SessionId) {
        state
            .writers
            .get_mut(&id)
            .unwrap()
            .doc
            .as_mut()
            .unwrap()
            .apply_edit(0..0, "x")
            .unwrap();
    }

    #[test]
    fn dirty_banner_enter_keeps_text() {
        let (mut state, id, run, dir) = writer_agent();
        open_d(&mut state, id, &run, &dir, "aaa");
        dirty(&mut state, id);
        std::fs::write(dir.join("d.md"), "external").unwrap();
        state.writer_poll_files();
        // Enter fires the default (first) action: it must not throw
        // away unsaved edits.
        state.writer_fire_confirm(id, 0);
        let session = state.writers.get(&id).unwrap();
        assert_eq!(session.doc.as_ref().unwrap().text, "xaaa", "text kept");
        assert!(session.pending_confirm.is_none(), "banner settled");
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn deleted_banner_enter_recreates() {
        let (mut state, id, run, dir) = writer_agent();
        open_d(&mut state, id, &run, &dir, "aaa");
        dirty(&mut state, id);
        std::fs::remove_file(dir.join("d.md")).unwrap();
        state.writer_poll_files();
        // Enter fires the default (first) action: recreate, not close.
        state.writer_fire_confirm(id, 0);
        assert_eq!(std::fs::read_to_string(dir.join("d.md")).unwrap(), "xaaa");
        let session = state.writers.get(&id).unwrap();
        assert!(session.doc.is_some(), "doc kept open");
        assert!(!session.doc.as_ref().unwrap().dirty, "recreate is clean");
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }
}
