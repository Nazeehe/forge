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

/// Most finished (answered/cancelled) requests kept; oldest evicted.
/// Open, proposed, and answered-plus-proposed requests are never evicted.
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
            Self::evict_finished_requests(self);
        }
        cancelled
    }

    /// Drop the oldest answered/cancelled requests past the cap.
    fn evict_finished_requests(session: &mut WriterSession) {
        let mut finished: Vec<u64> = session
            .requests
            .iter()
            .filter(|r| {
                matches!(
                    r.state,
                    WriterRequestState::Answered | WriterRequestState::Cancelled
                )
            })
            .map(|r| r.id)
            .collect();
        if finished.len() <= MAX_FINISHED_REQUESTS {
            return;
        }
        finished.sort_unstable();
        let drop_count = finished.len() - MAX_FINISHED_REQUESTS;
        let drop: std::collections::HashSet<u64> =
            finished.into_iter().take(drop_count).collect();
        session.requests.retain(|r| {
            !matches!(
                r.state,
                WriterRequestState::Answered | WriterRequestState::Cancelled
            ) || !drop.contains(&r.id)
        });
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

    /// Open or create a Markdown file under the session cwd. A different
    /// unsaved document blocks the switch; nothing is ever discarded.
    /// No view switching in the slice (no Writer tab yet): opens silently.
    fn writer_open(&mut self, id: crate::session::SessionId, args: &str) -> Result<String, String> {
        let path = Self::tool_arg(args, "path")
            .ok_or_else(|| "writer_open needs a path".to_string())?;
        let title = Self::tool_arg(args, "title");
        let cwd = self
            .manager
            .get(id)
            .map(|rec| rec.cwd.clone())
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        let session = self.writers.entry(id).or_default();
        if let Some(doc) = session.doc.as_ref() {
            if doc.path_rel == path {
                let rev = doc.revision;
                return Ok(format!(
                    r#"{{"opened":true,"path":{},"revision":{rev}}}"#,
                    crate::ipc::mcp::escape_json(&path),
                ));
            }
            if doc.dirty {
                return Err(format!(
                    "unsaved document open: {} (save it before opening another)",
                    doc.path_rel,
                ));
            }
        }
        let doc = Document::open(&cwd, &path).map_err(|e| e.to_string())?;
        let rev = doc.revision;
        // A new document scope drops the old one's proposals, requests,
        // thread, and selection; they addressed the previous text.
        session.doc = Some(doc);
        session.proposals = Proposals::default();
        session.requests.clear();
        session.thread.clear();
        session.selection = None;
        session.title = title;
        self.dirty = true;
        Ok(format!(
            r#"{{"opened":true,"path":{},"revision":{rev}}}"#,
            crate::ipc::mcp::escape_json(&path),
        ))
    }

    /// Read the open document: metadata, selection, pending ids, lines.
    fn writer_read(&mut self, id: crate::session::SessionId, args: &str) -> Result<String, String> {
        let session = self
            .writers
            .get(&id)
            .ok_or_else(|| "no Writer document open for this session".to_string())?;
        let doc = session
            .doc
            .as_ref()
            .ok_or_else(|| "no Writer document open for this session".to_string())?;
        let total_lines = doc.text.chars().filter(|&c| c == '\n').count() as u32 + 1;
        let start = Self::tool_u32(args, "start_line").unwrap_or(1);
        let end = Self::tool_u32(args, "end_line").unwrap_or(total_lines);
        if start < 1 || end < start || start > total_lines {
            return Err("bad read range: lines are 1-based within the document".to_string());
        }
        let end = end.min(total_lines);
        let text_lines: Vec<&str> = doc.text.split('\n').collect();
        let mut entries = String::from("[");
        let mut first_entry = true;
        let mut used = 0usize;
        let mut truncated = false;
        for (index, line) in text_lines
            .iter()
            .enumerate()
            .skip((start - 1) as usize)
            .take((end - start + 1) as usize)
        {
            if !first_entry {
                entries.push(',');
            }
            first_entry = false;
            let remaining = crate::writer::MAX_READ_CHARS.saturating_sub(used);
            let (piece, cut) = if line.chars().count() <= remaining {
                (line.to_string(), false)
            } else {
                (line.chars().take(remaining).collect::<String>(), true)
            };
            used += piece.chars().count();
            truncated = truncated || cut;
            entries.push_str(&format!(
                r#"{{"line":{},"text":{}}}"#,
                index + 1,
                crate::ipc::mcp::escape_json(&piece),
            ));
            if cut {
                break;
            }
        }
        entries.push(']');
        let selection = match session.selection.clone() {
            Some(range) => {
                match (
                    crate::writer::line_col_of(&doc.text, range.start),
                    crate::writer::line_col_of(&doc.text, range.end),
                ) {
                    (Some((start_line, start_col)), Some((end_line, end_col))) => format!(
                        r#"{{"start":{},"end":{},"start_line":{start_line},"start_col":{start_col},"end_line":{end_line},"end_col":{end_col}}}"#,
                        range.start, range.end,
                    ),
                    _ => "null".to_string(),
                }
            }
            None => "null".to_string(),
        };
        let mut pending = String::from("[");
        // Requests the agent still owes work: open, proposed-but-unanswered,
        // or answered-but-unproposed. Both (done) and Cancelled drop off.
        for request in session.requests.iter().filter(|r| {
            matches!(
                r.state,
                WriterRequestState::Open
                    | WriterRequestState::Proposed
                    | WriterRequestState::Answered
            )
        }) {
            if pending.len() > 1 {
                pending.push(',');
            }
            pending.push_str(&request.id.to_string());
        }
        pending.push(']');
        Ok(format!(
            r#"{{"path":{},"revision":{},"total_lines":{total_lines},"selection":{selection},"pending_requests":{pending},"start_line":{start},"end_line":{end},"lines":{entries},"truncated":{truncated}{}}}"#,
            crate::ipc::mcp::escape_json(&doc.path_rel),
            doc.revision,
            if truncated {
                r#","note":"truncated at 64000 chars; re-read with start_line/end_line""#
            } else {
                ""
            },
        ))
    }

    /// Record a proposal by request id (exact request range) or by
    /// 1-based line range (insert: end = start - 1; append: start = total + 1).
    fn writer_propose(&mut self, id: crate::session::SessionId, args: &str) -> Result<String, String> {
        let text = Self::tool_arg(args, "text")
            .ok_or_else(|| "writer_propose needs text".to_string())?;
        let note = Self::tool_arg(args, "note");
        let session = self
            .writers
            .get_mut(&id)
            .ok_or_else(|| "no Writer document open for this session".to_string())?;
        if session.doc.is_none() {
            return Err("no Writer document open for this session".to_string());
        }
        if crate::ipc::mcp::top_raw(args, "request_id").is_some()
            && Self::tool_u32(args, "request_id").is_none()
        {
            return Err("request_id must be a positive integer".to_string());
        }
        let (request_id, range, text) = match Self::tool_u32(args, "request_id") {
            Some(raw) => {
                let rid = raw as u64;
                let record = session
                    .requests
                    .iter()
                    .find(|r| r.id == rid)
                    .ok_or_else(|| format!("unknown request {rid}"))?;
                match record.state {
                    WriterRequestState::Open | WriterRequestState::Answered => {
                        // Request ranges apply verbatim; drift is checked below.
                        (Some(rid), record.range.clone(), text)
                    }
                    WriterRequestState::Proposed | WriterRequestState::Both => {
                        return Err(format!("request {rid} already proposed"));
                    }
                    WriterRequestState::Cancelled => {
                        return Err(format!("request {rid} was cancelled"));
                    }
                }
            }
            None => {
                let start = Self::tool_u32(args, "start_line").ok_or_else(|| {
                    "writer_propose needs request_id or start_line/end_line".to_string()
                })?;
                let end = Self::tool_u32(args, "end_line").ok_or_else(|| {
                    "writer_propose needs request_id or start_line/end_line".to_string()
                })?;
                let doc_text = session.doc.as_ref().expect("checked above").text.clone();
                let (range, text) = normalize_line_edit(&doc_text, start, end, text)?;
                (None, range, text)
            }
        };
        let doc = session.doc.as_ref().expect("checked above");
        let (proposal, stale) = match request_id {
            Some(rid) => {
                let record = session
                    .requests
                    .iter()
                    .find(|r| r.id == rid)
                    .expect("checked above");
                let current = crate::writer::byte_range_of(&doc.text, record.range.clone())
                    .map(|bytes| doc.text[bytes].to_string());
                if current.as_deref() == Some(record.original.as_str()) {
                    let pid = session
                        .proposals
                        .propose(doc, Some(rid), record.range.clone(), text, note)
                        .map_err(|e| e.to_string())?;
                    (pid, false)
                } else {
                    // Drifted since the request: stale on arrival with the
                    // request-time snapshot, so it stays visible but can
                    // never be accepted (§4.4/G8).
                    let pid = session.proposals.propose_stale(
                        Some(rid),
                        record.range.clone(),
                        record.original.clone(),
                        text,
                        note,
                    );
                    (pid, true)
                }
            }
            None => {
                let pid = session
                    .proposals
                    .propose(doc, None, range, text, note)
                    .map_err(|e| e.to_string())?;
                (pid, false)
            }
        };
        if let Some(rid) = request_id {
            if let Some(record) = session.requests.iter_mut().find(|r| r.id == rid) {
                record.state = match record.state {
                    WriterRequestState::Answered => WriterRequestState::Both,
                    _ => WriterRequestState::Proposed,
                };
            }
        }
        self.dirty = true;
        if stale {
            let rid = request_id.expect("stale only on the request path");
            Ok(format!(
                r#"{{"proposed":true,"proposal":{proposal},"stale":true,"reason":"text changed since request {rid}; proposal arrived stale"}}"#,
            ))
        } else {
            Ok(format!(r#"{{"proposed":true,"proposal":{proposal},"stale":false}}"#))
        }
    }

    /// Store a Markdown answer for a request in the thread.
    fn writer_answer(&mut self, id: crate::session::SessionId, args: &str) -> Result<String, String> {
        let rid = Self::tool_u32(args, "request_id")
            .ok_or_else(|| "writer_answer needs request_id".to_string())? as u64;
        let answer = Self::tool_arg(args, "answer")
            .ok_or_else(|| "writer_answer needs an answer".to_string())?;
        let session = self
            .writers
            .get_mut(&id)
            .ok_or_else(|| "no Writer document open for this session".to_string())?;
        if session.doc.is_none() {
            return Err("no Writer document open for this session".to_string());
        }
        let record = session
            .requests
            .iter_mut()
            .find(|r| r.id == rid)
            .ok_or_else(|| format!("unknown request {rid}"))?;
        match record.state {
            WriterRequestState::Open => {
                record.state = WriterRequestState::Answered;
            }
            WriterRequestState::Proposed => {
                record.state = WriterRequestState::Both;
            }
            WriterRequestState::Answered | WriterRequestState::Both => {
                return Err(format!("request {rid} already answered"));
            }
            WriterRequestState::Cancelled => {
                return Err(format!("request {rid} was cancelled"));
            }
        }
        session.thread.push(WriterThreadEntry { request_id: rid, answer });
        while session.thread.len() > crate::writer::MAX_THREAD_ENTRIES {
            session.thread.remove(0);
        }
        WriterSession::evict_finished_requests(session);
        self.dirty = true;
        Ok(r#"{"answered":true}"#.to_string())
    }
}

/// Convert a 1-based inclusive line range to a char-offset range.
/// Insert (`end == start - 1`) and append (`start == total + 1`) denote
/// empty ranges; anything else out of shape is refused.
fn lines_to_chars(text: &str, start_line: u32, end_line: u32) -> Result<std::ops::Range<usize>, String> {
    let bad = || {
        "bad proposal line range: 1-based lines within the document (insert: end_line = start_line - 1; append: start_line = total lines + 1)".to_string()
    };
    if start_line < 1 {
        return Err(bad());
    }
    // Char offset where each 1-based line starts.
    let mut starts = vec![0usize];
    for (index, c) in text.chars().enumerate() {
        if c == '\n' {
            starts.push(index + 1);
        }
    }
    let total = starts.len() as u32;
    let total_chars = text.chars().count();
    if end_line.checked_add(1) == Some(start_line) {
        // Insert at the start of `start_line`; `total + 1` is the end.
        if start_line > total + 1 {
            return Err(bad());
        }
        let at = if start_line > total {
            total_chars
        } else {
            starts[(start_line - 1) as usize]
        };
        return Ok(at..at);
    }
    if end_line < start_line || start_line > total || end_line > total {
        // Append past the last line addresses the very end.
        if start_line == total + 1 && end_line == start_line {
            return Ok(total_chars..total_chars);
        }
        return Err(bad());
    }
    let start = starts[(start_line - 1) as usize];
    let end = if end_line >= total {
        total_chars
    } else {
        starts[end_line as usize] - 1
    };
    Ok(start..end)
}

/// Map a line-addressed proposal to its char range plus line-granular text
/// in one place: inserts before a line gain a trailing newline, appends to
/// a document without a trailing newline gain a leading one, and empty text
/// stays a true no-op. Line replaces apply verbatim.
fn normalize_line_edit(
    doc_text: &str,
    start_line: u32,
    end_line: u32,
    text: String,
) -> Result<(std::ops::Range<usize>, String), String> {
    let range = lines_to_chars(doc_text, start_line, end_line)?;
    if !range.is_empty() || text.is_empty() {
        return Ok((range, text));
    }
    let total_chars = doc_text.chars().count();
    if range.start == total_chars && !doc_text.is_empty() && !doc_text.ends_with('\n') {
        // Append past the last line: separate from it.
        if text.starts_with('\n') {
            return Ok((range, text));
        }
        return Ok((range, format!("\n{text}")));
    }
    // Insert before a line: terminate the inserted line.
    if text.ends_with('\n') {
        return Ok((range, text));
    }
    Ok((range, format!("{text}\n")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_support::*;

    fn writer_agent() -> (AppState, crate::session::SessionId, String, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "forge-writer-tool-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let mut state = AppState::new();
        let id = state
            .manager
            .spawn_agent(
                "agent",
                &dir,
                "exec cat",
                crate::infra::ids::RunId::generate(),
                "codex",
            )
            .unwrap();
        let run = state.manager.get(id).unwrap().run_id.as_str().to_string();
        (state, id, run, dir)
    }

    fn open_doc(state: &mut AppState, run: &str, path: &str) -> String {
        comms_reply(state, run, "writer_open", &format!(r#"{{"path":{}}}"#, crate::ipc::mcp::escape_json(path)))
    }

    #[test]
    fn open_creates_and_opens_markdown() {
        let (mut state, id, run, dir) = writer_agent();
        std::fs::write(dir.join("notes.md"), "# hi\nbody\n").unwrap();
        let opened = open_doc(&mut state, &run, "notes.md");
        assert!(opened.contains(r#""ok":true"#), "opened: {opened}");
        assert!(opened.contains(r#""revision":0"#), "opened: {opened}");
        let session = state.writers.get(&id).expect("writer state");
        assert_eq!(session.doc.as_ref().unwrap().text, "# hi\nbody\n");
        // A missing file opens as an empty document, written on first save.
        let created = open_doc(&mut state, &run, "new.md");
        assert!(created.contains(r#""ok":true"#), "created: {created}");
        assert_eq!(state.writers.get(&id).unwrap().doc.as_ref().unwrap().text, "");
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn open_refuses_over_unsaved_document() {
        let (mut state, id, run, dir) = writer_agent();
        std::fs::write(dir.join("a.md"), "aaa").unwrap();
        assert!(open_doc(&mut state, &run, "a.md").contains(r#""ok":true"#));
        state.writers.get_mut(&id).unwrap().doc.as_mut().unwrap()
            .apply_edit(0..3, "bbb").unwrap();
        let refused = open_doc(&mut state, &run, "b.md");
        assert!(refused.contains(r#""ok":false"#), "refused: {refused}");
        assert!(refused.contains("unsaved document open"), "refused: {refused}");
        // The dirty document is untouched.
        assert_eq!(state.writers.get(&id).unwrap().doc.as_ref().unwrap().text, "bbb");
        // Reopening the same path is fine.
        let same = open_doc(&mut state, &run, "a.md");
        assert!(same.contains(r#""ok":true"#), "same: {same}");
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn open_never_switches_the_view_in_the_slice() {
        let (mut state, _id, run, dir) = writer_agent();
        assert!(state.overlay_view.is_none());
        open_doc(&mut state, &run, "q.md");
        // No Writer tab exists in S3: the open stays silent (Q1).
        assert!(state.overlay_view.is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn foreign_run_id_is_refused_for_all_writer_tools() {
        let (mut state, _id, _run, dir) = writer_agent();
        for (tool, args) in [
            ("writer_open", r#"{"path":"x.md"}"#),
            ("writer_read", "{}"),
            ("writer_propose", r#"{"text":"x","start_line":1,"end_line":1}"#),
            ("writer_answer", r#"{"request_id":1,"answer":"x"}"#),
        ] {
            let reply = comms_reply(&mut state, "bogus-run", tool, args);
            assert!(reply.contains("unknown or stale run ID"), "{tool}: {reply}");
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn propose_by_request_id_uses_the_exact_range() {
        let (mut state, id, run, dir) = writer_agent();
        std::fs::write(dir.join("d.md"), "hello world").unwrap();
        assert!(open_doc(&mut state, &run, "d.md").contains(r#""ok":true"#));
        let rid = state.writers.get_mut(&id).unwrap()
            .new_request(WriterAction::Rephrase, "hello world", 6..11, 0).unwrap();
        let reply = comms_reply(
            &mut state, &run, "writer_propose",
            &format!(r#"{{"request_id":{rid},"text":"Forge","note":"tight"}}"#),
        );
        assert!(reply.contains(r#""proposed":true"#), "reply: {reply}");
        assert!(reply.contains(r#""proposal":1"#), "reply: {reply}");
        let session = state.writers.get(&id).unwrap();
        let proposal = session.proposals.get(1).unwrap();
        assert_eq!(proposal.range, 6..11, "exact request range, no rounding");
        assert_eq!(proposal.original, "world");
        assert_eq!(proposal.request_id, Some(rid));
        // Proposing records; only Accept (S4) applies.
        assert_eq!(session.doc.as_ref().unwrap().text, "hello world");
        assert_eq!(session.requests.iter().find(|r| r.id == rid).unwrap().state, WriterRequestState::Proposed);
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn propose_rejects_unknown_and_cancelled_requests() {
        let (mut state, id, run, dir) = writer_agent();
        std::fs::write(dir.join("d.md"), "hello world").unwrap();
        assert!(open_doc(&mut state, &run, "d.md").contains(r#""ok":true"#));
        let unknown = comms_reply(&mut state, &run, "writer_propose", r#"{"request_id":99,"text":"x"}"#);
        assert!(unknown.contains("unknown request 99"), "unknown: {unknown}");
        let rid = state.writers.get_mut(&id).unwrap()
            .new_request(WriterAction::Ask, "hello world", 0..5, 0).unwrap();
        assert!(state.writers.get_mut(&id).unwrap().cancel_request(rid));
        let cancelled = comms_reply(
            &mut state, &run, "writer_propose",
            &format!(r#"{{"request_id":{rid},"text":"x"}}"#),
        );
        assert!(cancelled.contains("cancelled"), "cancelled: {cancelled}");
        let cancelled_answer = comms_reply(
            &mut state, &run, "writer_answer",
            &format!(r#"{{"request_id":{rid},"answer":"x"}}"#),
        );
        assert!(cancelled_answer.contains("cancelled"), "cancelled answer: {cancelled_answer}");
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn propose_after_answer_and_answer_after_propose() {
        let (mut state, id, run, dir) = writer_agent();
        std::fs::write(dir.join("d.md"), "hello world").unwrap();
        assert!(open_doc(&mut state, &run, "d.md").contains(r#""ok":true"#));
        // Propose first, then answer: both land.
        let rid = state.writers.get_mut(&id).unwrap()
            .new_request(WriterAction::Ask, "hello world", 0..5, 0).unwrap();
        let proposed = comms_reply(
            &mut state, &run, "writer_propose",
            &format!(r#"{{"request_id":{rid},"text":"hi"}}"#),
        );
        assert!(proposed.contains(r#""proposed":true"#), "proposed: {proposed}");
        let answered = comms_reply(
            &mut state, &run, "writer_answer",
            &format!(r#"{{"request_id":{rid},"answer":"why"}}"#),
        );
        assert!(answered.contains(r#""answered":true"#), "answered: {answered}");
        // Answer first, then propose: both land the other way round.
        let rid2 = state.writers.get_mut(&id).unwrap()
            .new_request(WriterAction::Chat, "hello world", 0..5, 0).unwrap();
        let answered2 = comms_reply(
            &mut state, &run, "writer_answer",
            &format!(r#"{{"request_id":{rid2},"answer":"done"}}"#),
        );
        assert!(answered2.contains(r#""answered":true"#), "answered2: {answered2}");
        let proposed2 = comms_reply(
            &mut state, &run, "writer_propose",
            &format!(r#"{{"request_id":{rid2},"text":"yo"}}"#),
        );
        assert!(proposed2.contains(r#""proposed":true"#), "proposed2: {proposed2}");
        let session = state.writers.get(&id).unwrap();
        assert_eq!(
            session.requests.iter().find(|r| r.id == rid).unwrap().state,
            WriterRequestState::Both
        );
        assert_eq!(
            session.requests.iter().find(|r| r.id == rid2).unwrap().state,
            WriterRequestState::Both
        );
        // Each kind arrives once: repeats are refused.
        let propose_again = comms_reply(
            &mut state, &run, "writer_propose",
            &format!(r#"{{"request_id":{rid},"text":"z"}}"#),
        );
        assert!(propose_again.contains("already proposed"), "repeat propose: {propose_again}");
        let answer_again = comms_reply(
            &mut state, &run, "writer_answer",
            &format!(r#"{{"request_id":{rid},"answer":"z"}}"#),
        );
        assert!(answer_again.contains("already answered"), "repeat answer: {answer_again}");
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn malformed_request_id_errors_plainly() {
        let (mut state, _id, run, dir) = writer_agent();
        std::fs::write(dir.join("d.md"), "hello").unwrap();
        assert!(open_doc(&mut state, &run, "d.md").contains(r#""ok":true"#));
        for args in [
            r#"{"request_id":-1,"text":"x"}"#,
            r#"{"request_id":"x","text":"x"}"#,
            r#"{"request_id":-1,"text":"x","start_line":1,"end_line":1}"#,
        ] {
            let reply = comms_reply(&mut state, &run, "writer_propose", args);
            assert!(
                reply.contains("request_id must be a positive integer"),
                "args {args}: {reply}"
            );
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn propose_arriving_after_drift_is_stale() {
        let (mut state, id, run, dir) = writer_agent();
        std::fs::write(dir.join("d.md"), "aaa bbb ccc").unwrap();
        assert!(open_doc(&mut state, &run, "d.md").contains(r#""ok":true"#));
        let rid = state.writers.get_mut(&id).unwrap()
            .new_request(WriterAction::Rephrase, "aaa bbb ccc", 4..7, 0).unwrap();
        // Human types above the range; the request-time text moved.
        state.writers.get_mut(&id).unwrap().doc.as_mut().unwrap()
            .apply_edit(0..0, "XX").unwrap();
        let reply = comms_reply(
            &mut state, &run, "writer_propose",
            &format!(r#"{{"request_id":{rid},"text":"B"}}"#),
        );
        assert!(reply.contains(r#""proposed":true"#), "reply: {reply}");
        assert!(reply.contains(r#""stale":true"#), "stale flag: {reply}");
        assert!(reply.contains("text changed since request"), "reason: {reply}");
        let session = state.writers.get(&id).unwrap();
        let proposal = session.proposals.get(1).unwrap();
        assert_eq!(proposal.original, "bbb", "request-time snapshot kept");
        use crate::writer::proposal::ProposalState;
        use crate::writer::WriterError;
        assert_eq!(proposal.state, ProposalState::Stale);
        // A stale arrival can never be accepted (S2 snapshot check).
        let session = state.writers.get_mut(&id).unwrap();
        let doc = session.doc.as_mut().unwrap();
        assert_eq!(
            session.proposals.accept(doc, 1).unwrap_err(),
            WriterError::StaleProposal(1)
        );
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn propose_without_drift_stays_pending() {
        let (mut state, id, run, dir) = writer_agent();
        std::fs::write(dir.join("d.md"), "aaa bbb ccc").unwrap();
        assert!(open_doc(&mut state, &run, "d.md").contains(r#""ok":true"#));
        let rid = state.writers.get_mut(&id).unwrap()
            .new_request(WriterAction::Rephrase, "aaa bbb ccc", 4..7, 0).unwrap();
        let reply = comms_reply(
            &mut state, &run, "writer_propose",
            &format!(r#"{{"request_id":{rid},"text":"B"}}"#),
        );
        assert!(reply.contains(r#""stale":false"#), "fresh: {reply}");
        use crate::writer::proposal::ProposalState;
        assert_eq!(
            state.writers.get(&id).unwrap().proposals.get(1).unwrap().state,
            ProposalState::Pending
        );
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn thread_and_finished_requests_are_bounded() {
        let (mut state, id, run, dir) = writer_agent();
        std::fs::write(dir.join("d.md"), "hello").unwrap();
        assert!(open_doc(&mut state, &run, "d.md").contains(r#""ok":true"#));
        for _ in 0..70 {
            let rid = state.writers.get_mut(&id).unwrap()
                .new_request(WriterAction::Chat, "hello", 0..5, 0).unwrap();
            let answered = comms_reply(
                &mut state, &run, "writer_answer",
                &format!(r#"{{"request_id":{rid},"answer":"a"}}"#),
            );
            assert!(answered.contains(r#""answered":true"#));
        }
        // Three requests stay open; they are never evicted.
        let mut keepers = Vec::new();
        for _ in 0..3 {
            keepers.push(state.writers.get_mut(&id).unwrap()
                .new_request(WriterAction::Ask, "hello", 0..5, 0).unwrap());
        }
        let session = state.writers.get(&id).unwrap();
        assert_eq!(session.thread.len(), crate::writer::MAX_THREAD_ENTRIES);
        assert_eq!(session.thread[0].request_id, 7, "oldest answers evicted first");
        assert_eq!(session.requests.len(), 64 + 3);
        for rid in &keepers {
            assert_eq!(
                session.requests.iter().find(|r| r.id == *rid).unwrap().state,
                WriterRequestState::Open,
                "open {rid} evicted"
            );
        }
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn line_inserts_and_appends_stay_line_granular() {
        let (mut state, id, run, dir) = writer_agent();
        std::fs::write(dir.join("d.md"), "l1\nl2\nl3").unwrap();
        assert!(open_doc(&mut state, &run, "d.md").contains(r#""ok":true"#));
        // Insert mid-doc gains its newline.
        comms_reply(&mut state, &run, "writer_propose", r#"{"text":"New","start_line":2,"end_line":1}"#);
        // Insert at line 1 the same.
        comms_reply(&mut state, &run, "writer_propose", r#"{"text":"Top","start_line":1,"end_line":0}"#);
        // Append without a trailing newline is separated.
        comms_reply(&mut state, &run, "writer_propose", r#"{"text":"End","start_line":4,"end_line":4}"#);
        // A line replace stays verbatim.
        comms_reply(&mut state, &run, "writer_propose", r#"{"text":"R","start_line":2,"end_line":2}"#);
        let session = state.writers.get(&id).unwrap();
        assert_eq!(session.proposals.get(1).unwrap().range, 3..3);
        assert_eq!(session.proposals.get(1).unwrap().text, "New\n");
        assert_eq!(session.proposals.get(2).unwrap().range, 0..0);
        assert_eq!(session.proposals.get(2).unwrap().text, "Top\n");
        assert_eq!(session.proposals.get(3).unwrap().range, 8..8);
        assert_eq!(session.proposals.get(3).unwrap().text, "\nEnd");
        assert_eq!(session.proposals.get(4).unwrap().range, 3..5);
        assert_eq!(session.proposals.get(4).unwrap().text, "R");
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn append_with_trailing_newline_and_empty_doc() {
        let (mut state, id, run, dir) = writer_agent();
        std::fs::write(dir.join("t.md"), "a\n").unwrap();
        assert!(open_doc(&mut state, &run, "t.md").contains(r#""ok":true"#));
        comms_reply(&mut state, &run, "writer_propose", r#"{"text":"B","start_line":3,"end_line":3}"#);
        let session = state.writers.get(&id).unwrap();
        assert_eq!(session.proposals.get(1).unwrap().range, 2..2);
        assert_eq!(session.proposals.get(1).unwrap().text, "B\n");
        // Empty document: append addresses the start.
        std::fs::write(dir.join("e.md"), "").unwrap();
        assert!(open_doc(&mut state, &run, "e.md").contains(r#""ok":true"#));
        comms_reply(&mut state, &run, "writer_propose", r#"{"text":"A","start_line":2,"end_line":2}"#);
        // Opening the new document reset the proposal store: ids restart.
        let session = state.writers.get(&id).unwrap();
        assert_eq!(session.proposals.get(1).unwrap().range, 0..0);
        assert_eq!(session.proposals.get(1).unwrap().text, "A\n");
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn propose_by_line_range_insert_and_append() {
        let (mut state, _id, run, dir) = writer_agent();
        std::fs::write(dir.join("d.md"), "l1\nl2\nl3").unwrap();
        assert!(open_doc(&mut state, &run, "d.md").contains(r#""ok":true"#));
        // Whole line 2.
        let r2 = comms_reply(&mut state, &run, "writer_propose", r#"{"text":"L2","start_line":2,"end_line":2}"#);
        assert!(r2.contains(r#""proposal":1"#), "line range: {r2}");
        // Insert before line 2 (end = start - 1).
        let ins = comms_reply(&mut state, &run, "writer_propose", r#"{"text":"I","start_line":2,"end_line":1}"#);
        assert!(ins.contains(r#""proposal":2"#), "insert: {ins}");
        // Append past the last line (start = total + 1).
        let app = comms_reply(&mut state, &run, "writer_propose", r#"{"text":"A","start_line":4,"end_line":4}"#);
        assert!(app.contains(r#""proposal":3"#), "append: {app}");
        for bad in [
            r#"{"text":"x","start_line":0,"end_line":1}"#,
            r#"{"text":"x","start_line":3,"end_line":1}"#,
            r#"{"text":"x","start_line":9,"end_line":9}"#,
            r#"{"text":"x"}"#,
        ] {
            let reply = comms_reply(&mut state, &run, "writer_propose", bad);
            assert!(reply.contains(r#""ok":false"#), "bad {bad}: {reply}");
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn propose_line_ranges_map_to_exact_char_ranges() {
        let (mut state, id, run, dir) = writer_agent();
        std::fs::write(dir.join("d.md"), "l1\nl2\nl3").unwrap();
        assert!(open_doc(&mut state, &run, "d.md").contains(r#""ok":true"#));
        comms_reply(&mut state, &run, "writer_propose", r#"{"text":"L2","start_line":2,"end_line":2}"#);
        comms_reply(&mut state, &run, "writer_propose", r#"{"text":"I","start_line":2,"end_line":1}"#);
        comms_reply(&mut state, &run, "writer_propose", r#"{"text":"A","start_line":4,"end_line":4}"#);
        let session = state.writers.get(&id).unwrap();
        // "l1\nl2\nl3": line 2 is chars 3..5; insert/append are empty at 3 and 8.
        assert_eq!(session.proposals.get(1).unwrap().range, 3..5);
        assert_eq!(session.proposals.get(1).unwrap().original, "l2");
        assert_eq!(session.proposals.get(2).unwrap().range, 3..3);
        assert_eq!(session.proposals.get(3).unwrap().range, 8..8);
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn propose_refuses_oversize_and_over_cap() {
        let (mut state, _id, run, dir) = writer_agent();
        std::fs::write(dir.join("d.md"), "abc").unwrap();
        assert!(open_doc(&mut state, &run, "d.md").contains(r#""ok":true"#));
        let big = "x".repeat(crate::writer::MAX_PROPOSAL_CHARS + 1);
        let too_big = comms_reply(
            &mut state, &run, "writer_propose",
            &format!(r#"{{"text":"{big}","start_line":1,"end_line":1}}"#),
        );
        assert!(too_big.contains("too large"), "oversize: {too_big}");
        for _ in 0..crate::writer::MAX_PENDING_PROPOSALS {
            let reply = comms_reply(&mut state, &run, "writer_propose", r#"{"text":"x","start_line":1,"end_line":1}"#);
            assert!(reply.contains(r#""proposed":true"#), "fill: {reply}");
        }
        let capped = comms_reply(&mut state, &run, "writer_propose", r#"{"text":"x","start_line":1,"end_line":1}"#);
        assert!(capped.contains("too many pending"), "cap: {capped}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn answer_stores_thread_entry_and_rejects_repeats() {
        let (mut state, id, run, dir) = writer_agent();
        std::fs::write(dir.join("d.md"), "hello").unwrap();
        assert!(open_doc(&mut state, &run, "d.md").contains(r#""ok":true"#));
        let rid = state.writers.get_mut(&id).unwrap()
            .new_request(WriterAction::Chat, "hello", 0..5, 0).unwrap();
        let reply = comms_reply(
            &mut state, &run, "writer_answer",
            &format!(r##"{{"request_id":{rid},"answer":"# hi"}}"##),
        );
        assert!(reply.contains(r#""answered":true"#), "reply: {reply}");
        let session = state.writers.get(&id).unwrap();
        assert_eq!(session.thread.len(), 1);
        assert_eq!(session.thread[0].request_id, rid);
        assert_eq!(session.thread[0].answer, "# hi");
        let double = comms_reply(
            &mut state, &run, "writer_answer",
            &format!(r#"{{"request_id":{rid},"answer":"again"}}"#),
        );
        assert!(double.contains("already answered"), "double: {double}");
        let unknown = comms_reply(&mut state, &run, "writer_answer", r#"{"request_id":77,"answer":"x"}"#);
        assert!(unknown.contains("unknown request 77"), "unknown: {unknown}");
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn read_returns_metadata_selection_and_lines() {
        let (mut state, id, run, dir) = writer_agent();
        std::fs::write(dir.join("d.md"), "l1\nl2\nl3").unwrap();
        assert!(open_doc(&mut state, &run, "d.md").contains(r#""ok":true"#));
        let rid = state.writers.get_mut(&id).unwrap()
            .new_request(WriterAction::Ask, "l1\nl2\nl3", 0..2, 0).unwrap();
        state.writers.get_mut(&id).unwrap().selection = Some(3..5);
        let reply = comms_reply(&mut state, &run, "writer_read", "{}");
        assert!(reply.contains(r#""path":"d.md""#), "reply: {reply}");
        assert!(reply.contains(r#""revision":0"#), "reply: {reply}");
        assert!(reply.contains(r#""total_lines":3"#), "reply: {reply}");
        assert!(reply.contains(&format!(r#""pending_requests":[{rid}]"#)), "reply: {reply}");
        assert!(reply.contains(r#""selection":{"start":3,"end":5"#), "reply: {reply}");
        assert!(reply.contains("l1") && reply.contains("l3"), "reply: {reply}");
        // A line slice narrows the payload.
        let slice = comms_reply(&mut state, &run, "writer_read", r#"{"start_line":2,"end_line":2}"#);
        assert!(slice.contains("l2"), "slice: {slice}");
        assert!(!slice.contains("l1"), "slice: {slice}");
        assert!(state.manager.remove(id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn read_truncates_past_the_cap_with_a_note() {
        let (mut state, _id, run, dir) = writer_agent();
        let big = "y".repeat(crate::writer::MAX_READ_CHARS + 100);
        std::fs::write(dir.join("big.md"), &big).unwrap();
        assert!(open_doc(&mut state, &run, "big.md").contains(r#""ok":true"#));
        let reply = comms_reply(&mut state, &run, "writer_read", "{}");
        assert!(reply.contains(r#""truncated":true"#), "reply truncated");
        assert!(reply.contains("start_line"), "reread hint: {reply}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn read_without_an_open_document_fails() {
        let (mut state, _id, run, dir) = writer_agent();
        let reply = comms_reply(&mut state, &run, "writer_read", "{}");
        assert!(reply.contains(r#""ok":false"#), "reply: {reply}");
        assert!(reply.contains("no Writer document open"), "reply: {reply}");
        let propose = comms_reply(&mut state, &run, "writer_propose", r#"{"text":"x","start_line":1,"end_line":1}"#);
        assert!(propose.contains("no Writer document open"), "propose: {propose}");
        let answer = comms_reply(&mut state, &run, "writer_answer", r#"{"request_id":1,"answer":"x"}"#);
        assert!(answer.contains("no Writer document open"), "answer: {answer}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn terminate_drops_writer_state() {
        let (mut state, id, run, dir) = writer_agent();
        std::fs::write(dir.join("d.md"), "x").unwrap();
        assert!(open_doc(&mut state, &run, "d.md").contains(r#""ok":true"#));
        assert!(state.writers.contains_key(&id));
        assert!(state.terminate_session(id));
        assert!(!state.writers.contains_key(&id), "writer state drops with the session");
        std::fs::remove_dir_all(&dir).ok();
    }
}
