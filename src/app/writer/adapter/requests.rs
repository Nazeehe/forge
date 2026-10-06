//! Adapter request plumbing: Rephrase/chat request building over
//! live text, the bounded per-session queue, and the settle-path
//! flush under the same gate as comms. Also the recent-documents
//! bookkeeping (opened-this-run list plus the cached scan).

use super::{editor_selection_to_range, index2_to_offset, MAX_WRITER_QUEUE};
use crate::app::AppState;
use crate::writer::request::WriterAction;

/// Session working folder; "." when the record is gone.
pub(super) fn writer_cwd(state: &AppState, id: crate::session::SessionId) -> std::path::PathBuf {
    state
        .manager
        .get(id)
        .map(|rec| rec.cwd.clone())
        .unwrap_or_else(|| std::path::PathBuf::from("."))
}

impl AppState {
    /// Remember `abs` as opened this run (most-recent first, deduped,
    /// capped) and refresh the recent cache so the list shows it now.
    /// `pub(crate)`: the agent tool path in `tools.rs` shares it.
    pub(crate) fn writer_note_opened(
        &mut self,
        id: crate::session::SessionId,
        abs: std::path::PathBuf,
    ) {
        if let Some(sess) = self.writers.get_mut(&id) {
            sess.opened.retain(|p| p != &abs);
            sess.opened.insert(0, abs);
            while sess.opened.len() > super::MAX_OPENED_THIS_RUN {
                sess.opened.pop();
            }
        }
        self.writer_refresh_recent(id);
    }

    /// Re-scan the session folder and rebuild the recent cache:
    /// opened-this-run first, then the bounded mtime walk. Newest
    /// scans reset the selection when it points outside the list.
    /// A live doc with no file on disk yet (New, not yet saved) has
    /// no disk mtime, so the scan drops it; it heads the cache
    /// anyway, stamped now, since it IS open this run.
    pub(crate) fn writer_refresh_recent(&mut self, id: crate::session::SessionId) {
        let cwd = writer_cwd(self, id);
        let Some(sess) = self.writers.get_mut(&id) else {
            return;
        };
        let mut cache = crate::writer::recent::scan(&cwd, &sess.opened);
        let live_abs = sess.doc.as_ref().map(|d| d.abs_path.clone());
        if let Some(abs) = live_abs {
            let listed = cache.iter().any(|e| cwd.join(&e.rel) == abs);
            if !listed {
                if let Ok(rel) = abs.strip_prefix(&cwd) {
                    cache.insert(
                        0,
                        crate::writer::recent::RecentEntry {
                            rel: rel.to_string_lossy().into_owned(),
                            mtime: std::time::SystemTime::now(),
                            opened_this_run: true,
                        },
                    );
                }
            }
        }
        sess.recent_cache = cache;
        sess.recent_cwd = Some(cwd);
        let len = sess.recent_cache.len();
        sess.recent_sel = sess.recent_sel.min(len.saturating_sub(1));
    }
}

impl AppState {
    /// Rephrase the live selection, or the paragraph under the cursor.
    /// Sends no instruction and leaves the chat draft untouched: a
    /// half-typed message must never ride along and be lost.
    /// `pub(crate)`: the TUI input layer calls this directly.
    pub(crate) fn writer_rephrase(&mut self, id: crate::session::SessionId) {
        if self.writer_process_locked(id) {
            self.writer_process_deny(id);
            return;
        }
        self.writer_send_request(id, WriterAction::Rephrase, String::new());
    }

    /// Chat over the live selection, or the whole document when there
    /// is none. The chat box is the instruction and clears on send.
    /// `pub(crate)`: the TUI input layer calls this directly.
    pub(crate) fn writer_chat_send(&mut self, id: crate::session::SessionId) {
        if self.writer_process_locked(id) {
            self.writer_process_deny(id);
            return;
        }
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
    /// `pub(crate)`: run starts share it.
    pub(crate) fn writer_doc_name(path_rel: &str) -> String {
        std::path::Path::new(path_rel)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    /// Record a fixed-slot error without touching anything else.
    /// `pub(crate)`: the process toggle surfaces start failures (a
    /// caller outside the adapter folder).
    pub(crate) fn writer_fail(&mut self, id: crate::session::SessionId, message: &str) {
        if let Some(session) = self.writers.get_mut(&id) {
            session.error = Some(message.to_string());
        }
        self.dirty = true;
    }

    /// True while the session looks deliverable to: the record exists,
    /// the session hasn't exited, and it has an agent tab. A fast
    /// pre-check only — the flush path re-gates on the agent's live
    /// state and holds (never drops) when the pane write fails.
    pub(crate) fn writer_agent_live(&self, id: crate::session::SessionId) -> bool {
        let Some(rec) = self.manager.get(id) else {
            return false;
        };
        rec.state.is_live()
            && rec
                .tabs
                .iter()
                .any(|tab| tab.kind == crate::session::TabKind::Agent)
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
}
