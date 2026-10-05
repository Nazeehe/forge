//! Writer MCP tool handlers (S3): open/read/propose/answer plus line addressing.

use super::*;
use crate::writer::document::Document;
use crate::writer::proposal::Proposals;

impl AppState {
    /// Open or create a Markdown file under the session cwd. A different
    /// unsaved document blocks the switch; nothing is ever discarded.
    /// No view switching in the slice (no Writer tab yet): opens silently.
    pub(super) fn writer_open(&mut self, id: crate::session::SessionId, args: &str) -> Result<String, String> {
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
    pub(super) fn writer_read(&mut self, id: crate::session::SessionId, args: &str) -> Result<String, String> {
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
    pub(super) fn writer_propose(&mut self, id: crate::session::SessionId, args: &str) -> Result<String, String> {
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
            session.evict_finished();
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
    pub(super) fn writer_answer(&mut self, id: crate::session::SessionId, args: &str) -> Result<String, String> {
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
        session.evict_finished();
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
