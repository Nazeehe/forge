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
        self.evict_old_settled();
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
        let original = self.items[index].original.clone();
        let text = self.items[index].text.clone();
        // Snapshot check: the text under the range must still be what the
        // proposal was made against. Any caller that changed the document
        // without going through on_edit lands here; refuse instead of
        // overwriting the human's newer text.
        let current = byte_range_of(&doc.text, range.clone())
            .map(|bytes| doc.text[bytes].to_string());
        if current.as_deref() != Some(original.as_str()) {
            self.items[index].state = ProposalState::Stale;
            return Err(WriterError::StaleProposal(id));
        }
        let rev = doc.apply_edit(range.clone(), &text)?;
        self.items[index].state = ProposalState::Accepted;
        self.on_edit(&range);
        self.evict_old_settled();
        Ok(rev)
    }

    /// Discard a pending or stale proposal.
    pub fn reject(&mut self, id: u64) -> Result<(), WriterError> {
        let index = self
            .items
            .iter()
            .position(|p| p.id == id)
            .ok_or(WriterError::UnknownProposal(id))?;
        match self.items[index].state {
            ProposalState::Pending | ProposalState::Stale => {
                self.items[index].state = ProposalState::Rejected;
                self.evict_old_settled();
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
        self.evict_old_settled();
    }

    /// Record a proposal that arrived after its text drifted: keeps the
    /// request-time `original` as the snapshot and lands Stale, so it
    /// stays visible but the snapshot check refuses every accept (§4.4).
    /// The range may no longer address the current text; that is what
    /// makes it stale. Settled eviction still applies.
    pub fn propose_stale(
        &mut self,
        request_id: Option<u64>,
        range: Range<usize>,
        original: String,
        text: String,
        note: Option<String>,
    ) -> u64 {
        self.next_id += 1;
        let id = self.next_id;
        self.items.push(Proposal {
            id,
            request_id,
            range,
            original,
            text,
            note,
            state: ProposalState::Stale,
        });
        self.evict_old_settled();
        id
    }

    /// Look up a proposal by id.
    pub fn get(&self, id: u64) -> Option<&Proposal> {
        self.items.iter().find(|p| p.id == id)
    }

    /// True when a still-pending proposal carries this request id.
    pub fn has_pending_for(&self, request_id: u64) -> bool {
        self.items
            .iter()
            .any(|p| p.request_id == Some(request_id) && p.state == ProposalState::Pending)
    }

    /// Drop the oldest settled proposals past [`MAX_SETTLED_PROPOSALS`].
    /// Pending proposals are never evicted.
    fn evict_old_settled(&mut self) {
        use super::MAX_SETTLED_PROPOSALS;
        let mut settled: Vec<u64> = self
            .items
            .iter()
            .filter(|p| p.state != ProposalState::Pending)
            .map(|p| p.id)
            .collect();
        if settled.len() <= MAX_SETTLED_PROPOSALS {
            return;
        }
        settled.sort_unstable();
        let drop_count = settled.len() - MAX_SETTLED_PROPOSALS;
        let drop: std::collections::HashSet<u64> =
            settled.into_iter().take(drop_count).collect();
        self.items
            .retain(|p| p.state == ProposalState::Pending || !drop.contains(&p.id));
    }

    /// Number of proposals still awaiting a decision.
    pub fn pending_count(&self) -> usize {
        self.items
            .iter()
            .filter(|p| p.state == ProposalState::Pending)
            .count()
    }

    /// Proposals still awaiting a decision, oldest first. The panel
    /// renders these with their Accept/Reject pills.
    pub fn pending(&self) -> impl Iterator<Item = &Proposal> {
        self.items
            .iter()
            .filter(|p| p.state == ProposalState::Pending)
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
    fn accept_refuses_when_text_changed_without_on_edit() {
        let mut d = doc("abcdef");
        let mut p = Proposals::default();
        let id = p.propose(&d, Some(1), 0..3, "X".to_string(), None).unwrap();
        // Direct change, no on_edit call (S4 adapter bug or reload path).
        d.apply_edit(0..1, "Z").unwrap();
        assert_eq!(d.text, "Zbcdef");
        assert_eq!(
            p.accept(&mut d, id).unwrap_err(),
            WriterError::StaleProposal(id)
        );
        // The proposal went stale and the human's text is untouched.
        assert_eq!(p.get(id).unwrap().state, ProposalState::Stale);
        assert_eq!(d.text, "Zbcdef");
    }

    #[test]
    fn accept_refuses_when_range_shrank_away() {
        let mut d = doc("abcdef");
        let mut p = Proposals::default();
        let id = p.propose(&d, None, 4..6, "X".to_string(), None).unwrap();
        d.apply_edit(0..6, "hi").unwrap();
        assert_eq!(
            p.accept(&mut d, id).unwrap_err(),
            WriterError::StaleProposal(id)
        );
        assert_eq!(p.get(id).unwrap().state, ProposalState::Stale);
    }

    #[test]
    fn settled_proposals_are_bounded_pending_survive() {
        let mut d = doc("aaa");
        let mut p = Proposals::default();
        let mut keepers = Vec::new();
        for _ in 0..3 {
            keepers.push(p.propose(&d, None, 0..0, "k".to_string(), None).unwrap());
        }
        // Loop edits land after the keepers so interim staleness
        // (any preceding edit stales) never touches them.
        for i in 0..70u64 {
            let id = p
                .propose(&d, None, 3..3, format!("v{i}").repeat(100), None)
                .unwrap();
            p.accept(&mut d, id).unwrap();
        }
        // 3 pending + newest 64 settled; oldest settled evicted.
        assert_eq!(p.items.len(), 67, "len: {}", p.items.len());
        for id in &keepers {
            assert_eq!(
                p.get(*id).unwrap().state,
                ProposalState::Pending,
                "pending {id} evicted"
            );
        }
        assert_eq!(p.pending_count(), 3);
        let settled: Vec<u64> = p
            .items
            .iter()
            .filter(|item| item.state != ProposalState::Pending)
            .map(|item| item.id)
            .collect();
        assert_eq!(settled.len(), 64);
        assert!(settled.windows(2).all(|w| w[0] < w[1]), "settled not newest-first-ordered");
    }

    #[test]
    fn stale_arrival_keeps_request_snapshot_and_refuses_accept() {
        let mut d = doc("abcdef");
        let mut p = Proposals::default();
        let id = p.propose_stale(Some(9), 0..3, "XXX".to_string(), "Y".to_string(), None);
        let got = p.get(id).unwrap();
        assert_eq!(got.original, "XXX");
        assert_eq!(got.state, ProposalState::Stale);
        assert_eq!(
            p.accept(&mut d, id).unwrap_err(),
            WriterError::StaleProposal(id)
        );
        assert_eq!(d.text, "abcdef", "refused accept changes nothing");
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

    #[test]
    fn pending_lists_only_undecided_oldest_first() {
        let d = doc("aaaa bbbb");
        let mut p = Proposals::default();
        let first = p.propose(&d, None, 0..4, "A".to_string(), None).unwrap();
        let second = p.propose(&d, None, 5..9, "B".to_string(), None).unwrap();
        p.reject(first).unwrap();
        let ids: Vec<u64> = p.pending().map(|proposal| proposal.id).collect();
        assert_eq!(ids, vec![second]);
    }
}
