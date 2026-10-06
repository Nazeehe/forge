//! Process-run UI side (spec §6): the read-only lock, pre-flight,
//! live reload, one-undo collapse, violation check, Stop, and
//! Revert. The agent protocol (markup, report/done tools, finish
//! paths) lives in [`super::runs`]; this module owns the human side.

use std::time::Instant;

use crate::app::AppState;

/// The read-only lock held while a process run is open: the
/// pre-run snapshot plus the outside-marker segments for the
/// finish-time violation check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessLock {
    /// The run this lock belongs to.
    pub run_id: u64,
    /// Document text at start (post-save): revert target and the
    /// source of the outside segments.
    pub pre_text: String,
    /// Whole char ranges of every parsed marker, in pre-run order:
    /// the finish walk pairs them with the run's statuses (skipped
    /// and failed spans stay verbatim).
    pub spans: Vec<std::ops::Range<usize>>,
    /// Text outside the pre-run marker spans, in order: head gap,
    /// middle gaps, tail gap. The finish check walks them in order
    /// over the final text.
    pub outside: Vec<String>,
    /// Stop requested: the agent is told to stop; the lock lifts on
    /// done or turn-end like any finish.
    pub stopped: bool,
}

/// Counts of the last finished run, for the status row
/// (`Run: 3 done · 1 skipped (Details)`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LastRun {
    /// Finished run id (its details ride the thread note).
    pub id: u64,
    /// Markers reported done (answers count as done).
    pub done: usize,
    /// Markers reported skipped.
    pub skipped: usize,
    /// Markers reported failed.
    pub failed: usize,
}

/// Notice shown when a blocked edit is tried under the lock. It
/// lives in the text-only banner slot, so the find bar keeps its
/// error-slot home while locked.
pub(crate) const PROCESS_NOTICE: &str = "Processing… (Stop)";

impl AppState {
    /// True while the read-only lock holds the session.
    /// `pub(crate)`: every mutating entry point checks it first.
    pub(crate) fn writer_process_locked(&self, id: crate::session::SessionId) -> bool {
        self.writers.get(&id).is_some_and(|s| s.process.is_some())
    }

    /// Refuse a mutating action under the lock: the notice, nothing
    /// else. `pub(crate)`: the adapter gate and the guarded fns
    /// share it.
    pub(crate) fn writer_process_deny(&mut self, id: crate::session::SessionId) {
        if let Some(session) = self.writers.get_mut(&id) {
            session.banner = Some(PROCESS_NOTICE.to_string());
        }
        self.dirty = true;
    }

    /// (Process)/(Stop) pill and Alt+Enter funnel: stop when locked,
    /// start over the live selection (if any) otherwise. Start
    /// failures surface in the fixed slot unless pre-flight already
    /// said why in the banner.
    pub fn writer_process_toggle(&mut self, id: crate::session::SessionId) {
        if self.writer_process_locked(id) {
            self.writer_process_stop(id);
            return;
        }
        let selection = self.writers.get(&id).and_then(|s| s.selection.clone());
        let now = Instant::now();
        if let Err(message) = self.writer_process_start(id, selection, now) {
            let banner_set = self.writers.get(&id).is_some_and(|s| s.banner.is_some());
            if !banner_set {
                self.writer_fail(id, &message);
            }
        }
    }

    /// Pre-flight and start: parse, filter to the selection,
    /// save-or-abort, snapshot, capture the one-undo boundary, start
    /// the run, and lock. Returns the run id.
    /// `pub(crate)`: the toggle and tests share it.
    pub(crate) fn writer_process_start(
        &mut self,
        id: crate::session::SessionId,
        only_in: Option<std::ops::Range<usize>>,
        now: Instant,
    ) -> Result<u64, String> {
        // The editor buffer is the source of truth: sync first so the
        // save below writes what the human sees.
        self.writer_sync_editor(id);
        let (eligible, excluded) = match self.writers.get(&id).and_then(|s| s.doc.as_ref()) {
            Some(doc) => {
                let parsed = crate::writer::markers::parse_markers(&doc.text);
                let count = parsed
                    .markers
                    .iter()
                    .filter(|m| {
                        only_in.as_ref().is_none_or(|r| {
                            r.start <= m.whole.start && m.whole.end <= r.end
                        })
                    })
                    .count();
                let lines: Vec<u32> = parsed
                    .errors
                    .iter()
                    .filter(|e| {
                        e.kind == crate::writer::markers::ErrorKind::PromptTooLong
                            && only_in.as_ref().is_none_or(|r| r.contains(&e.range.start))
                    })
                    .filter_map(|e| crate::writer::line_of(&doc.text, e.range.start))
                    .collect();
                (count, lines)
            }
            None => return Err("no document open".to_string()),
        };
        if self.writers.get(&id).is_some_and(|s| s.process.is_some()) {
            return Err("already processing: stop the run first".to_string());
        }
        if self.writers.get(&id).is_some_and(|s| s.open_prompt.is_some()) {
            return Err("close the prompt first".to_string());
        }
        // The wrap gesture cannot survive the run (its text can): the
        // placeholder is plain marker text the agent reads.
        self.abandon_wrap(id);
        if eligible == 0 {
            let message = if excluded.is_empty() {
                "No markers to process".to_string()
            } else {
                format!("No markers to process ({} excluded)", excluded.len())
            };
            if let Some(session) = self.writers.get_mut(&id) {
                session.banner = Some(message.clone());
            }
            self.dirty = true;
            return Err(message);
        }
        // A conflict aborts with the banner: the agent must read the
        // saved file, never a buffer the disk disagrees with.
        self.writer_save(id);
        let saved = self
            .writers
            .get(&id)
            .and_then(|s| s.save_note.clone())
            .unwrap_or_default();
        if saved != "saved" {
            return Err("save failed: resolve the conflict first".to_string());
        }
        let pre_text = self
            .writers
            .get(&id)
            .and_then(|s| s.doc.as_ref())
            .map(|d| d.text.clone())
            .unwrap_or_default();
        let parsed = crate::writer::markers::parse_markers(&pre_text);
        let spans: Vec<std::ops::Range<usize>> =
            parsed.markers.iter().map(|m| m.whole.clone()).collect();
        let outside = Self::process_outside_segments(&pre_text, &spans);
        // One-undo boundary: capture the pre-run buffer through an
        // Insert-mode re-entry (EdTUI captures there), then restore
        // mode, cursor, and selection. Reloads during the run assign
        // lines directly and never capture, so one Ctrl+Z after the
        // finish lands back here.
        if let Some(session) = self.writers.get_mut(&id) {
            if let Some(editor) = session.editor.as_mut() {
                use edtui::actions::SwitchMode;
                use edtui::EditorMode;
                let (mode, cursor, sel) =
                    (editor.mode, editor.cursor, editor.selection.clone());
                editor.mode = EditorMode::Normal;
                editor.execute(SwitchMode(EditorMode::Insert));
                editor.cursor = cursor;
                editor.selection = sel;
                editor.mode = mode;
            }
            session.type_group = None;
        }
        let rid = self.writer_start_run(id, now, only_in)?;
        if let Some(session) = self.writers.get_mut(&id) {
            let riding = session.runs.iter().find(|r| r.id == rid).map(|r| r.markers.len()).unwrap_or(0);
            let mut notes = Vec::new();
            if eligible > riding {
                notes.push(format!(
                    "{} more markers: process again for the rest",
                    eligible - riding
                ));
            }
            if !excluded.is_empty() {
                let s = if excluded.len() == 1 { "" } else { "s" };
                let lines = excluded
                    .iter()
                    .map(|l| l.to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                notes.push(format!(
                    "{} marker{s} excluded (prompt too long): line{s} {lines}",
                    excluded.len()
                ));
            }
            if !notes.is_empty() {
                session.banner = Some(notes.join("; "));
            }
            session.process = Some(ProcessLock {
                run_id: rid,
                pre_text,
                outside,
                spans,
                stopped: false,
            });
            session.error = None;
        }
        self.dirty = true;
        Ok(rid)
    }

    /// Stop a locked run: interrupt the agent, or ask it to wind
    /// down when its harness carries no interrupt. Completed markers
    /// stay; the lock lifts on done or turn-end (timeout when the
    /// agent goes silent). Stopping twice, or with no lock, is a
    /// no-op. `pub(crate)`: the toggle, Esc, and tests share it.
    pub(crate) fn writer_process_stop(&mut self, id: crate::session::SessionId) {
        let rid = match self.writers.get(&id).and_then(|s| s.process.as_ref()) {
            Some(lock) if !lock.stopped => lock.run_id,
            _ => return,
        };
        // A dead agent never dones: finish at once instead of
        // stranding the lock on settle paths that need its activity.
        // (Untestable in unit tests: fixtures always hold a live
        // pane. Defensive only.)
        if !self.writer_agent_live(id) {
            self.finish_writer_run(id, rid, "stopped (agent gone)".to_string());
            return;
        }
        // Adapter-owned interrupt bytes: no agent name is matched in
        // core. A direct user-initiated inject, exempt from the busy
        // gate like the user pressing Esc in that pane.
        let bytes = self
            .manager
            .get(id)
            .and_then(|rec| crate::agents::adapter_for(&rec.cli_tool).interrupt_bytes())
            .map(|b| b.to_vec());
        match bytes {
            Some(bytes) => match self.manager.inject_write(id, &bytes) {
                Ok(()) => {
                    if let Some(session) = self.writers.get_mut(&id) {
                        if let Some(lock) = session.process.as_mut() {
                            lock.stopped = true;
                        }
                        session.banner = Some("Stopping…".to_string());
                    }
                    self.dirty = true;
                }
                // The pane died mid-stop: same as a dead agent above.
                // (Defensive: see above.)
                Err(_) => self.finish_writer_run(id, rid, "stopped (agent gone)".to_string()),
            },
            None => {
                if let Some(session) = self.writers.get_mut(&id) {
                    if let Some(lock) = session.process.as_mut() {
                        lock.stopped = true;
                    }
                    session.banner = Some(
                        "Stop requested: the agent will finish its current marker".to_string(),
                    );
                }
                self.dirty = true;
            }
        }
    }

    /// Restore the pre-run snapshot as one undo step (the violation
    /// banner's Revert run): buffer, document, and file all return
    /// to the snapshot, so one Ctrl+Z gets back to the run's final
    /// text. `pub(crate)`: the confirm arm and tests share it.
    pub(crate) fn writer_process_revert(&mut self, id: crate::session::SessionId, pre: String) {
        // Capture the final buffer first (one-undo: undo returns
        // here), then wholesale-replace without capturing.
        if let Some(session) = self.writers.get_mut(&id) {
            if let Some(editor) = session.editor.as_mut() {
                use edtui::actions::SwitchMode;
                use edtui::EditorMode;
                let (mode, cursor, sel) =
                    (editor.mode, editor.cursor, editor.selection.clone());
                editor.mode = EditorMode::Normal;
                editor.execute(SwitchMode(EditorMode::Insert));
                editor.cursor = cursor;
                editor.selection = sel;
                editor.mode = mode;
            }
            session.type_group = None;
        }
        let total = self
            .writers
            .get(&id)
            .and_then(|s| s.doc.as_ref())
            .map(|d| d.text.chars().count())
            .unwrap_or(0);
        if let Some(session) = self.writers.get_mut(&id) {
            if let Some(doc) = session.doc.as_mut() {
                if doc.apply_edit(0..total, &pre).is_err() {
                    self.writer_fail(id, "revert failed");
                    return;
                }
            }
            if let Some(editor) = session.editor.as_mut() {
                editor.lines = edtui::Lines::from(pre.as_str());
                editor.cursor =
                    crate::app::writer::adapter::offset_to_index2(pre.as_str(), pre.chars().count());
                editor.selection = None;
            }
            session.proposals.on_edit(&(0..total));
            session.reset_fence_cache();
            session.selection = None;
        }
        // The file follows the buffer (save cannot conflict: the
        // agent is done and nobody else writes mid-revert).
        self.writer_save(id);
        self.dirty = true;
    }

/// The finish walk: outside gaps and skipped/failed marker spans
/// are literals in pre-run order; done (or silent) markers are
/// wildcards standing for their free-form replacements. Literals
/// pin the text around them: the first pins the start, the last
/// pins the end, and a literal right after another must follow
/// immediately (nothing wildcard-shaped fits between them).
/// Literals after a wildcard are searched in order. A missing
/// literal means the agent touched outside text. The walk is
/// sound — a clean run always passes, since every literal sits
/// where the walk looks — but incomplete: insertions absorbed
/// into a neighboring wildcard are indistinguishable from that
/// marker's replacement, so Revert (not this walk) is the exact
/// remedy. Associated without `self`, so tests drive it directly.
pub(crate) fn process_run_intact(
        pre: &str,
        spans: &[std::ops::Range<usize>],
        verbatim: &[bool],
        final_text: &str,
    ) -> bool {
        let chars: Vec<char> = pre.chars().collect();
        enum Item {
            Lit(Vec<char>),
            Any,
        }
        let mut items: Vec<Item> = Vec::new();
        let mut pos = 0;
        for (i, span) in spans.iter().enumerate() {
            let gap: Vec<char> = chars[pos..span.start.min(chars.len())].to_vec();
            if !gap.is_empty() {
                items.push(Item::Lit(gap));
            }
            if verbatim.get(i) == Some(&true) {
                items.push(Item::Lit(chars[span.clone()].to_vec()));
            } else {
                items.push(Item::Any);
            }
            pos = span.end.min(chars.len());
        }
        let tail: Vec<char> = chars[pos..].to_vec();
        if !tail.is_empty() {
            items.push(Item::Lit(tail));
        }
        let fin: Vec<char> = final_text.chars().collect();
        let has_any = items.iter().any(|it| matches!(it, Item::Any));
        if !has_any {
            // No wildcards at all: the literals are the whole text.
            let whole: Vec<char> = items
                .iter()
                .filter_map(|it| match it {
                    Item::Lit(text) => Some(text.clone()),
                    Item::Any => None,
                })
                .flatten()
                .collect();
            return fin == whole;
        }
        let last = items.len().saturating_sub(1);
        let mut fpos = 0;
        // Whether the previous kept item was a literal: adjacent
        // literals must follow immediately, with nothing wildcard-
        // shaped between them. The head starts unadjacent (a leading
        // wildcard's replacement may precede it).
        let mut prev_lit = false;
        for (i, item) in items.iter().enumerate() {
            let lit = match item {
                Item::Lit(text) => text,
                Item::Any => {
                    prev_lit = false;
                    continue;
                }
            };
            if i == 0 {
                // Nothing precedes the head, not even a wildcard.
                if fin.get(..lit.len()) != Some(lit.as_slice()) {
                    return false;
                }
                fpos = lit.len();
            } else if i == last {
                if prev_lit {
                    // Adjacent to the previous literal: exact fit to
                    // the very end.
                    if fin.get(fpos..fpos + lit.len()) != Some(lit.as_slice()) {
                        return false;
                    }
                    if fpos + lit.len() != fin.len() {
                        return false;
                    }
                    fpos = fin.len();
                } else {
                    // Past a wildcard: the tail must close the text.
                    let rest = &fin[fpos..];
                    if rest.len() < lit.len()
                        || rest[rest.len() - lit.len()..] != lit[..]
                    {
                        return false;
                    }
                    fpos = fin.len();
                }
            } else if prev_lit {
                // Adjacent literals: nothing fits between them.
                if fin.get(fpos..fpos + lit.len()) != Some(lit.as_slice()) {
                    return false;
                }
                fpos += lit.len();
            } else {
                // Past a wildcard: searched in order.
                let rest = &fin[fpos..];
                match rest.windows(lit.len()).position(|w| w == lit.as_slice()) {
                    Some(at) => fpos += at + lit.len(),
                    None => return false,
                }
            }
            prev_lit = true;
        }
        true
    }

    /// Split `text` at the marker `spans` (sorted, disjoint) into the
    /// outside segments: head gap, middle gaps, tail gap. Empty gaps
    /// stay (adjacency is information for the finish walk).
    fn process_outside_segments(
        text: &str,
        spans: &[std::ops::Range<usize>],
    ) -> Vec<String> {
        let total = text.chars().count();
        let mut out = Vec::with_capacity(spans.len() + 1);
        let mut pos = 0;
        for span in spans {
            let start = span.start.min(total).max(pos);
            out.push(text.chars().skip(pos).take(start - pos).collect());
            pos = span.end.min(total).max(pos);
        }
        out.push(text.chars().skip(pos).collect());
        out
    }
}
