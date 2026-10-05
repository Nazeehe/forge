//! Writer proposals (P2 slice): request ranges, accept/reject, staleness.
//!
//! Accept applies the replacement and marks the proposal accepted. Any other
//! pending proposal intersecting the accepted range — or sitting entirely
//! after the edit start in the interim slice rule — goes stale at once.

use std::ops::Range;

use super::document::Document;
use super::{byte_range_of, WriterError, MAX_PENDING_PROPOSALS, MAX_PROPOSAL_CHARS};

/// Lifecycle of one proposal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProposalState {
    Pending,
    Accepted,
    Rejected,
    Stale,
}

/// One agent-suggested edit: replace `range` (char offsets) with `text`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Proposal {
    pub id: u64,
    pub request_id: Option<u64>,
    pub range: Range<usize>,
    pub original: String,
    pub text: String,
    pub note: Option<String>,
    pub state: ProposalState,
}

/// Pending-proposal store for one document.
#[derive(Clone, Debug, Default)]
pub struct Proposals {
    next_id: u64,
    items: Vec<Proposal>,
}

impl Proposals {
    /// Record a proposal over `range` of `doc`; captures the original text.
    pub fn propose(
        &mut self,
        doc: &Document,
        request_id: Option<u64>,
        range: Range<usize>,
        text: String,
        note: Option<String>,
    ) -> Result<u64, WriterError> {
        let bytes = byte_range_of(&doc.text, range.clone()).ok_or(WriterError::RangeOutOfBounds)?;
        let proposal_chars = text.chars().count();
        if proposal_chars > MAX_PROPOSAL_CHARS {
            return Err(WriterError::ProposalTooLarge(proposal_chars));
        }
        if self.pending_count() >= MAX_PENDING_PROPOSALS {
            return Err(WriterError::TooManyPending);
        }
        self.next_id += 1;
        let id = self.next_id;
        self.items.push(Proposal {
            id,
            request_id,
            range,
            original: doc.text[bytes].to_string(),
            text,
            note,
            state: ProposalState::Pending,
        });
        Ok(id)
    }

    /// Accept a pending proposal: exact-range replace, revision bump.
    /// Overlapping pendings go stale via the accepted edit.
    pub fn accept(&mut self, doc: &mut Document, id: u64) -> Result<u64, WriterError> {
        let index = self
            .items
            .iter()
            .position(|p| p.id == id)
            .ok_or(WriterError::UnknownProposal(id))?;
        match self.items[index].state {
            ProposalState::Stale => return Err(WriterError::StaleProposal(id)),
            ProposalState::Accepted | ProposalState::Rejected => {
                return Err(WriterError::AlreadySettled(id));
            }
            ProposalState::Pending => {}
        }
        let range = self.items[index].range.clone();
        let text = self.items[index].text.clone();
        let rev = doc.apply_edit(range.clone(), &text)?;
        self.items[index].state = ProposalState::Accepted;
        self.on_edit(&range);
        Ok(rev)
    }

    /// Discard a pending or stale proposal.
    pub fn reject(&mut self, id: u64) -> Result<(), WriterError> {
        let item = self
            .items
            .iter_mut()
            .find(|p| p.id == id)
            .ok_or(WriterError::UnknownProposal(id))?;
        match item.state {
            ProposalState::Pending | ProposalState::Stale => {
                item.state = ProposalState::Rejected;
                Ok(())
            }
            ProposalState::Accepted | ProposalState::Rejected => {
                Err(WriterError::AlreadySettled(id))
            }
        }
    }

    /// Mark pendings stale after an edit (interim slice rule: intersecting
    /// or entirely preceding edits stale the proposal).
    pub fn on_edit(&mut self, edit: &Range<usize>) {
        for item in self.items.iter_mut() {
            if item.state != ProposalState::Pending {
                continue;
            }
            let intersects =
                edit.start < item.range.end && item.range.start < edit.end;
            let preceding = edit.end <= item.range.start;
            if intersects || preceding {
                item.state = ProposalState::Stale;
            }
        }
    }

    /// Look up a proposal by id.
    pub fn get(&self, id: u64) -> Option<&Proposal> {
        self.items.iter().find(|p| p.id == id)
    }

    /// Number of proposals still awaiting a decision.
    pub fn pending_count(&self) -> usize {
        self.items
            .iter()
            .filter(|p| p.state == ProposalState::Pending)
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(text: &str) -> Document {
        Document {
            path_rel: "t.md".to_string(),
            abs_path: std::path::PathBuf::from("/tmp/t.md"),
            text: text.to_string(),
            revision: 0,
            disk_hash: 0,
            dirty: false,
        }
    }

    #[test]
    fn propose_captures_original_over_request_range() {
        let d = doc("hello world");
        let mut p = Proposals::default();
        let id = p
            .propose(&d, Some(3), 6..11, "Forge".to_string(), Some("tightened".to_string()))
            .unwrap();
        assert_eq!(id, 1);
        let got = p.get(1).unwrap();
        assert_eq!(got.request_id, Some(3));
        assert_eq!(got.original, "world");
        assert_eq!(got.text, "Forge");
        assert_eq!(got.state, ProposalState::Pending);
        assert_eq!(p.pending_count(), 1);
    }

    #[test]
    fn propose_accepts_empty_insert_range() {
        let d = doc("ac");
        let mut p = Proposals::default();
        let id = p.propose(&d, None, 1..1, "b".to_string(), None).unwrap();
        assert_eq!(p.get(id).unwrap().original, "");
    }

    #[test]
    fn propose_rejects_bad_range_and_oversize() {
        let d = doc("abc");
        let mut p = Proposals::default();
        assert_eq!(
            p.propose(&d, None, 0..9, "x".to_string(), None).unwrap_err(),
            WriterError::RangeOutOfBounds
        );
        assert_eq!(
            p.propose(&d, None, 2..1, "x".to_string(), None).unwrap_err(),
            WriterError::RangeOutOfBounds
        );
        let big = "x".repeat(MAX_PROPOSAL_CHARS + 1);
        assert_eq!(
            p.propose(&d, None, 0..0, big, None).unwrap_err(),
            WriterError::ProposalTooLarge(MAX_PROPOSAL_CHARS + 1)
        );
        // Exactly at the cap is fine.
        let capped = "y".repeat(MAX_PROPOSAL_CHARS);
        assert!(p.propose(&d, None, 0..0, capped, None).is_ok());
    }

    #[test]
    fn sixteenth_pending_ok_seventeenth_refused() {
        let d = doc("abc");
        let mut p = Proposals::default();
        for _ in 0..MAX_PENDING_PROPOSALS {
            p.propose(&d, None, 0..0, "x".to_string(), None).unwrap();
        }
        assert_eq!(p.pending_count(), MAX_PENDING_PROPOSALS);
        assert_eq!(
            p.propose(&d, None, 0..0, "x".to_string(), None).unwrap_err(),
            WriterError::TooManyPending
        );
    }

    #[test]
    fn accept_replaces_exact_range_and_bumps_revision() {
        let mut d = doc("hello world, hello");
        let mut p = Proposals::default();
        // Only the second "hello" (chars 13..18) is targeted.
        let id = p.propose(&d, Some(7), 13..18, "bye".to_string(), None).unwrap();
        let rev = p.accept(&mut d, id).unwrap();
        assert_eq!(rev, 1);
        assert_eq!(d.text, "hello world, bye");
        assert_eq!(p.get(id).unwrap().state, ProposalState::Accepted);
        assert_eq!(p.pending_count(), 0);
    }

    #[test]
    fn accept_marks_intersecting_pending_stale() {
        let mut d = doc("aaaa bbbb cccc");
        let mut p = Proposals::default();
        let first = p.propose(&d, None, 0..4, "A".to_string(), None).unwrap();
        let second = p.propose(&d, None, 2..7, "B".to_string(), None).unwrap();
        p.accept(&mut d, first).unwrap();
        assert_eq!(d.text, "A bbbb cccc");
        assert_eq!(p.get(second).unwrap().state, ProposalState::Stale);
    }

    #[test]
    fn preceding_edit_stales_pending_interim_rule() {
        let mut d = doc("aaaa bbbb");
        let mut p = Proposals::default();
        let id = p.propose(&d, None, 5..9, "B".to_string(), None).unwrap();
        // Human edit before the range: no shifting in the slice, it goes stale.
        d.apply_edit(0..1, "X").unwrap();
        p.on_edit(&(0..1));
        assert_eq!(p.get(id).unwrap().state, ProposalState::Stale);
    }

    #[test]
    fn edit_after_range_leaves_pending_alone() {
        let mut d = doc("aaaa bbbb");
        let mut p = Proposals::default();
        let id = p.propose(&d, None, 0..4, "A".to_string(), None).unwrap();
        d.apply_edit(5..9, "B").unwrap();
        p.on_edit(&(5..9));
        assert_eq!(p.get(id).unwrap().state, ProposalState::Pending);
    }

    #[test]
    fn accept_stale_is_refused() {
        let mut d = doc("aaaa bbbb");
        let mut p = Proposals::default();
        let id = p.propose(&d, None, 5..9, "B".to_string(), None).unwrap();
        d.apply_edit(0..9, "changed!").unwrap();
        p.on_edit(&(0..9));
        assert_eq!(p.accept(&mut d, id).unwrap_err(), WriterError::StaleProposal(id));
        assert!(d.dirty);
    }

    #[test]
    fn accept_unknown_and_double_settle() {
        let mut d = doc("abc");
        let mut p = Proposals::default();
        assert_eq!(
            p.accept(&mut d, 99).unwrap_err(),
            WriterError::UnknownProposal(99)
        );
        let id = p.propose(&d, None, 0..1, "X".to_string(), None).unwrap();
        p.accept(&mut d, id).unwrap();
        assert_eq!(
            p.accept(&mut d, id).unwrap_err(),
            WriterError::AlreadySettled(id)
        );
        assert_eq!(
            p.reject(id).unwrap_err(),
            WriterError::AlreadySettled(id)
        );
    }

    #[test]
    fn reject_pending_and_stale() {
        let mut d = doc("aaaa bbbb");
        let mut p = Proposals::default();
        let pending = p.propose(&d, None, 0..4, "A".to_string(), None).unwrap();
        let doomed = p.propose(&d, None, 5..9, "B".to_string(), None).unwrap();
        p.reject(pending).unwrap();
        assert_eq!(p.get(pending).unwrap().state, ProposalState::Rejected);
        d.apply_edit(0..9, "changed!").unwrap();
        p.on_edit(&(0..9));
        p.reject(doomed).unwrap();
        assert_eq!(p.get(doomed).unwrap().state, ProposalState::Rejected);
        assert_eq!(p.pending_count(), 0);
        assert_eq!(
            p.reject(77).unwrap_err(),
            WriterError::UnknownProposal(77)
        );
    }
}
