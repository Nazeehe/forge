//! AppState Writer dispatch (vertical slice S3): per-session document,
//! proposals, requests, and thread answers behind the four writer_* tools.
//!
//! Slice notes: there is no Writer overlay tab yet (S4), so `writer_open`
//! never touches `overlay_view` — it opens silently per the Q1 resolution.
//! Human-side request creation, the editor adapter, and rendering land in S4.

use super::*;
use crate::writer::document::Document;
use crate::writer::proposal::Proposals;
use crate::writer::request::WriterAction;

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

/// Per-session Writer state: the open document plus everything about it.
#[derive(Clone, Debug, Default)]
pub struct WriterSession {
    pub doc: Option<Document>,
    pub proposals: Proposals,
    pub requests: Vec<WriterRequestRecord>,
    pub thread: Vec<WriterThreadEntry>,
    /// Editor selection as a char range; set by the S4 adapter.
    pub selection: Option<std::ops::Range<usize>>,
    pub next_request_id: u64,
    /// Optional label from `writer_open`; titles the S4 tab.
    pub title: Option<String>,
}

/// Most finished requests kept; oldest evicted. Open requests and
/// Proposed requests with a still-pending proposal are never evicted.
const MAX_FINISHED_REQUESTS: usize = 64;

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

pub mod tools;
#[cfg(test)]
mod tests;
