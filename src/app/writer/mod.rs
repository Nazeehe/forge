//! AppState Writer state (S3+S4): per-session document, proposals,
//! requests, thread answers, the EdTUI editor behind the adapter, and
//! the view state the overlay paints.
//!
//! `writer_open` never touches `overlay_view` (Q1): it switches to the
//! Writer tab only when the human is already looking at it, which needs
//! no call at all — otherwise the document opens silently.

use super::*;
use crate::writer::document::Document;
use crate::writer::proposal::Proposals;
use crate::writer::request::WriterAction;

pub mod adapter;
pub mod tools;
#[cfg(test)]
mod tests;

/// A human → agent request. Proposals and answers arrive independently,
/// in either order; only a cancelled request refuses both.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WriterRequestState {
    Open,
    Proposed,
    Answered,
    Both,
    Cancelled,
}

/// One request record: the range (char offsets) the agent must work on,
/// plus the range text at request time for drift detection on arrival.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WriterRequestRecord {
    pub id: u64,
    pub action: WriterAction,
    pub range: std::ops::Range<usize>,
    pub original: String,
    pub rev: u64,
    pub state: WriterRequestState,
}

/// One agent answer in the Assistant thread (Markdown renders in S4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WriterThreadEntry {
    pub request_id: u64,
    pub answer: String,
}

/// Keyboard focus inside the Writer overlay: the editor, the chat
/// box, or the thread (proposal selection). Tab cycles all three.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WriterFocus {
    #[default]
    Editor,
    Chat,
    Thread,
}

/// The empty-state path prompt: typed path plus which pill opened it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WriterOpenPrompt {
    pub buffer: String,
    /// True from `(*New document)`, false from `(Open…)`; both submit
    /// through `writer_open`, only the prompt title differs.
    pub create: bool,
}

/// Per-session Writer state: the open document plus everything about it.
///
/// `EditorState` is `Clone` but not `Debug`, so this struct keeps `Clone`
/// and skips `Debug`.
#[derive(Clone, Default)]
pub struct WriterSession {
    pub doc: Option<Document>,
    pub proposals: Proposals,
    pub requests: Vec<WriterRequestRecord>,
    pub thread: Vec<WriterThreadEntry>,
    /// Editor selection as an exclusive char range; synced from the
    /// editor after every event by the adapter.
    pub selection: Option<std::ops::Range<usize>>,
    /// Keyboard-selection anchor as a char offset: set when a
    /// Shift+arrow gesture starts, cleared by any other key, the
    /// mouse, or a buffer rebuild. Lets one gesture cross back over
    /// its start without losing where it began.
    pub sel_anchor: Option<usize>,
    pub next_request_id: u64,
    /// Optional label from `writer_open`; titles the Writer tab.
    pub title: Option<String>,
    /// The live EdTUI buffer; `None` until a document opens.
    pub editor: Option<edtui::EditorState>,
    /// Forge-owned clipboard backing the editor (save/restore on Accept).
    pub clip: adapter::SharedClipboard,
    /// Where typing goes: editor or chat box.
    pub focus: WriterFocus,
    /// Chat box input (chars) plus cursor as a char index into it.
    pub chat_input: String,
    pub chat_cursor: usize,
    /// Proposal selected by clicking its thread entry, if any.
    pub selected_proposal: Option<u64>,
    /// Fixed error slot text; `None` renders the slot empty.
    pub error: Option<String>,
    /// Outbound request bodies awaiting the settle flush; cap 8, never drops.
    pub queue: std::collections::VecDeque<String>,
    /// Empty-state typed-path prompt, if open.
    pub open_prompt: Option<WriterOpenPrompt>,
}

/// Most finished requests kept; oldest evicted. Open requests and
/// Proposed requests with a still-pending proposal are never evicted.
const MAX_FINISHED_REQUESTS: usize = 64;

/// Typed-path prompt bound: long enough for any sane relative path,
/// short enough to stay one line.
pub const MAX_PATH_CHARS: usize = 256;

impl WriterSession {
    /// Record a human request over a char range of the current document
    /// text; snapshots the range text for drift detection. Returns its id.
    /// Called by S4 when an action pill or the chat box sends.
    pub fn new_request(
        &mut self,
        action: WriterAction,
        text: &str,
        range: std::ops::Range<usize>,
        rev: u64,
    ) -> Result<u64, String> {
        let bytes = crate::writer::byte_range_of(text, range.clone())
            .ok_or_else(|| "range outside the document".to_string())?;
        self.next_request_id += 1;
        let id = self.next_request_id;
        self.requests.push(WriterRequestRecord {
            id,
            action,
            range,
            original: text[bytes].to_string(),
            rev,
            state: WriterRequestState::Open,
        });
        Ok(id)
    }

    /// Scope-reset open shared by the agent tool and the human prompt:
    /// same path reopens in place, a dirty different document refuses,
    /// otherwise the old scope (proposals, requests, thread, selection)
    /// drops with the previous text. Returns the opened revision.
    /// The caller owns the editor rebuild.
    pub(super) fn open_document_path(
        &mut self,
        cwd: &std::path::Path,
        path: &str,
    ) -> Result<u64, String> {
        if let Some(doc) = self.doc.as_ref() {
            if doc.path_rel == path {
                return Ok(doc.revision);
            }
            if doc.dirty {
                return Err(format!(
                    "unsaved document open: {} (save it before opening another)",
                    doc.path_rel,
                ));
            }
        }
        let doc = Document::open(cwd, path).map_err(|e| e.to_string())?;
        let rev = doc.revision;
        self.doc = Some(doc);
        self.proposals = Proposals::default();
        self.requests.clear();
        self.thread.clear();
        self.selection = None;
        self.sel_anchor = None;
        self.selected_proposal = None;
        self.error = None;
        Ok(rev)
    }

    /// Withdraw an open request; true when one was open.
    /// The agent's later calls for it fail as cancelled.
    pub fn cancel_request(&mut self, id: u64) -> bool {
        let cancelled = match self.requests.iter_mut().find(|r| r.id == id) {
            Some(rec) if rec.state == WriterRequestState::Open => {
                rec.state = WriterRequestState::Cancelled;
                true
            }
            _ => false,
        };
        if cancelled {
            self.evict_finished();
        }
        cancelled
    }

    /// A request is finished when the agent owes it nothing more: answered,
    /// cancelled, answered-plus-proposed, or proposed with every tied
    /// proposal settled (accepted, rejected, or stale).
    fn is_request_finished(&self, id: u64) -> bool {
        match self.requests.iter().find(|r| r.id == id) {
            Some(rec) => match rec.state {
                WriterRequestState::Open => false,
                WriterRequestState::Answered
                | WriterRequestState::Cancelled
                | WriterRequestState::Both => true,
                WriterRequestState::Proposed => !self.proposals.has_pending_for(id),
            },
            None => true,
        }
    }

    /// Drop the oldest finished requests past the cap. Open requests and
    /// Proposed requests with a still-pending proposal always survive.
    /// Runs after every request/proposal mutation, including accept and
    /// reject, which the S4 adapter performs through the domain store.
    pub fn evict_finished(&mut self) {
        let finished: Vec<u64> = self
            .requests
            .iter()
            .filter(|r| self.is_request_finished(r.id))
            .map(|r| r.id)
            .collect();
        if finished.len() <= MAX_FINISHED_REQUESTS {
            return;
        }
        let mut ordered = finished;
        ordered.sort_unstable();
        let drop_count = ordered.len() - MAX_FINISHED_REQUESTS;
        let drop: std::collections::HashSet<u64> =
            ordered.into_iter().take(drop_count).collect();
        self.requests.retain(|r| !drop.contains(&r.id));
    }
}

impl AppState {
    /// Absolute topbar index of the Writer overlay slot for one
    /// session, or `None` for an unknown session.
    pub(super) fn writer_slot(&self, id: crate::session::SessionId) -> Option<usize> {
        let rec = self.manager.get(id)?;
        OVERLAY_TABS
            .iter()
            .position(|tab| *tab == "Writer")
            .map(|slot| rec.tabs.len() + slot)
    }

    /// The focused Writer overlay, if the human is looking at one:
    /// the active session whose overlay slot is the Writer tab.
    pub fn writer_overlay_active(&self) -> Option<crate::session::SessionId> {
        let active = self.manager.active()?;
        let (view_id, index) = self.overlay_view?;
        if view_id != active || Some(index) != self.writer_slot(active) {
            return None;
        }
        Some(active)
    }

    /// Open the active session's Writer tab (`Ctrl-b d`). Always
    /// switches: this is the human asking, not an agent opening
    /// silently (Q1). No document need be open yet.
    pub fn open_writer_overlay(&mut self) {
        let Some(active) = self.manager.active() else {
            return;
        };
        if let Some(slot) = self.writer_slot(active) {
            // The entry exists from here on: even the empty state
            // paints and takes keys through it.
            self.writers.entry(active).or_default();
            self.overlay_view = Some((active, slot));
            self.dirty = true;
        }
    }

    /// True while Writer keys own input: the overlay slot is focused.
    /// A document need not be open (the empty state takes keys too).
    pub fn writer_keys_active(&self) -> bool {
        self.writer_overlay_active().is_some()
    }

    /// Execute one Writer MCP tool. `None` when the name is not a
    /// Writer tool and the broker should answer instead.
    pub(super) fn writer_tool(
        &mut self,
        run_id: &str,
        tool: &str,
        args: &str,
    ) -> Option<Result<String, String>> {
        match tool {
            "writer_open" | "writer_read" | "writer_propose" | "writer_answer" => {}
            _ => return None,
        }
        let id = match self.resolve_tool_caller(run_id) {
            Ok(id) => id,
            Err(e) => return Some(Err(e)),
        };
        match tool {
            "writer_open" => Some(self.writer_open(id, args)),
            "writer_read" => Some(self.writer_read(id, args)),
            "writer_propose" => Some(self.writer_propose(id, args)),
            _ => Some(self.writer_answer(id, args)),
        }
    }
}
